use crate::{llm, ui, write_agent_output};
use dnd_assistant_core::{AgentConfig, AgentKind, TranscriptContext, run_enabled_agents_at};
use std::{path::PathBuf, sync::mpsc, thread};

pub struct AgentJob {
    pub configs: Vec<AgentConfig>,
    pub context: TranscriptContext,
    pub output_dir: PathBuf,
    pub llm_provider: Option<llm::LlmConfig>,
    pub sequence: usize,
}

pub struct AgentDispatcher {
    sender: Option<mpsc::Sender<AgentJob>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl AgentDispatcher {
    pub fn start(ui_state: Option<ui::SharedLiveState>) -> Self {
        // Submission must never block transcription. A job contains only the
        // rolling context and is drained by the independent agent worker.
        let (sender, receiver) = mpsc::channel::<AgentJob>();
        let worker = thread::spawn(move || {
            for job in receiver {
                run_job(job, ui_state.as_ref());
            }
        });
        Self {
            sender: Some(sender),
            worker: Some(worker),
        }
    }

    pub fn submit(&self, job: AgentJob) -> Result<(), String> {
        self.sender
            .as_ref()
            .ok_or_else(|| "agent dispatcher is stopped".to_owned())
            .and_then(|sender| {
                sender
                    .send(job)
                    .map_err(|_| "agent dispatcher stopped".into())
            })
    }

    pub fn finish(mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AgentDispatcher {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn run_job(job: AgentJob, ui_state: Option<&ui::SharedLiveState>) {
    for (agent, result) in
        job.configs
            .iter()
            .filter(|agent| agent.enabled)
            .zip(run_enabled_agents_at(
                &job.configs,
                &job.context,
                job.sequence,
            ))
    {
        let result = if agent.kind == AgentKind::Llm {
            job.llm_provider
                .as_ref()
                .ok_or_else(|| "no llm provider is configured".to_owned())
                .and_then(|provider| llm::run(provider, agent, &job.context))
        } else {
            Ok(result)
        };
        match result {
            Ok(result) => match write_agent_output(&job.output_dir, agent, &result) {
                Ok(()) => {
                    if let Some(ui_state) = ui_state {
                        ui::update_agent(ui_state, &result);
                    }
                    println!("{} -> {}", agent.id, agent.output);
                }
                Err(error) => eprintln!(
                    "agent {} failed; continuing other agents: {error}",
                    agent.id
                ),
            },
            Err(error) => eprintln!(
                "agent {} failed; continuing other agents: {error}",
                agent.id
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dnd_assistant_core::{SegmentStatus, TranscriptSegment};
    use std::fs;

    #[test]
    fn dispatcher_drains_configured_agent_jobs() {
        let output_dir = std::env::temp_dir().join(format!(
            "dnd-assistant-dispatcher-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&output_dir).unwrap();
        let dispatcher = AgentDispatcher::start(None);
        dispatcher
            .submit(AgentJob {
                configs: vec![AgentConfig {
                    id: "summary".into(),
                    kind: AgentKind::LiveSummary,
                    enabled: true,
                    output: "summary.md".into(),
                    instruction: Some("Keep this concise.".into()),
                    run_every_segments: 1,
                }],
                context: TranscriptContext {
                    session_id: "session-1".into(),
                    current: TranscriptSegment {
                        id: "segment-1".into(),
                        start_ms: 0,
                        end_ms: 1_000,
                        speaker_id: None,
                        text: "We enter the temple".into(),
                        confidence: None,
                        status: SegmentStatus::Finalized,
                    },
                    recent: vec![TranscriptSegment {
                        id: "segment-1".into(),
                        start_ms: 0,
                        end_ms: 1_000,
                        speaker_id: None,
                        text: "We enter the temple".into(),
                        confidence: None,
                        status: SegmentStatus::Finalized,
                    }],
                    session_state: None,
                    campaign_context: vec![],
                },
                output_dir: output_dir.clone(),
                llm_provider: None,
                sequence: 1,
            })
            .unwrap();
        dispatcher.finish();
        let summary = fs::read_to_string(output_dir.join("summary.md")).unwrap();
        assert!(summary.contains("We enter the temple"));
        assert!(summary.contains("Keep this concise"));
        let _ = fs::remove_file(output_dir.join("summary.md"));
        let _ = fs::remove_dir(output_dir);
    }
}
