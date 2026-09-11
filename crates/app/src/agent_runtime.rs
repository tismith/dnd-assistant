use crate::{llm, session::SessionLog, ui, write_agent_output};
use dnd_assistant_core::{
    AgentConfig, AgentKind, Event, TranscriptContext, WorkspaceDocument, run_builtin_agent,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

pub struct AgentJob {
    pub configs: Vec<AgentConfig>,
    pub context: TranscriptContext,
    pub output_dir: PathBuf,
    pub llm_provider: Option<llm::LlmConfig>,
    pub sequence: usize,
    pub session_log: SessionLog,
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
    for agent in job.configs.iter().filter(|agent| {
        agent.enabled
            && agent.kind != AgentKind::SessionEditor
            && job.sequence.is_multiple_of(agent.run_every_segments.max(1))
    }) {
        let context = context_for_agent(&job.context, agent);
        if let Err(error) = job.session_log.append(&Event::AgentRunRequested {
            timestamp_ms: context.current.end_ms,
        }) {
            eprintln!("session event log append failed for agent run: {error}");
        }
        let result = if agent.kind == AgentKind::Llm {
            job.llm_provider
                .as_ref()
                .ok_or_else(|| "no llm provider is configured".to_owned())
                .and_then(|provider| llm::run(provider, agent, &context))
        } else {
            Ok(run_builtin_agent(agent, &context))
        };
        match result {
            Ok(result) => match write_agent_output(&job.output_dir, agent, &result) {
                Ok(()) => {
                    if let Some(ui_state) = ui_state {
                        ui::update_agent(ui_state, &result);
                    }
                    if let Err(error) = job.session_log.append(&Event::AgentSuggestionCreated {
                        timestamp_ms: job.context.current.end_ms,
                        text: result.body.clone(),
                    }) {
                        eprintln!("session event log append failed for agent result: {error}");
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

fn context_for_agent(context: &TranscriptContext, agent: &AgentConfig) -> TranscriptContext {
    let mut scoped = context.clone();
    scoped.workspace_context = load_workspace_documents(&agent.workspace_paths);
    scoped
}

pub fn load_workspace_documents(paths: &[String]) -> Vec<WorkspaceDocument> {
    let mut documents = Vec::new();
    for configured in paths {
        let path = Path::new(configured);
        if path.is_dir() {
            collect_workspace_files(path, &mut documents);
        } else if path.is_file() {
            read_workspace_file(path, &mut documents);
        } else {
            eprintln!("agent workspace path does not exist; skipping: {configured}");
        }
    }
    documents
}

fn collect_workspace_files(directory: &Path, documents: &mut Vec<WorkspaceDocument>) {
    let Ok(entries) = fs::read_dir(directory) else {
        eprintln!(
            "cannot read agent workspace directory: {}",
            directory.display()
        );
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_workspace_files(&path, documents);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            read_workspace_file(&path, documents);
        }
    }
}

fn read_workspace_file(path: &Path, documents: &mut Vec<WorkspaceDocument>) {
    match fs::read_to_string(path) {
        Ok(content) => documents.push(WorkspaceDocument {
            path: path.display().to_string(),
            content,
        }),
        Err(error) => eprintln!(
            "cannot read agent workspace file {}: {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dnd_assistant_core::{SegmentStatus, TranscriptSegment, WorkspaceDocument};
    use std::fs;

    #[test]
    fn dispatcher_drains_configured_agent_jobs() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!(
            "dnd-assistant-dispatcher-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&output_dir).unwrap();
        let session_log = SessionLog::open(&output_dir).unwrap();
        let dispatcher = AgentDispatcher::start(None);
        dispatcher
            .submit(AgentJob {
                configs: vec![AgentConfig {
                    id: "summary".into(),
                    kind: AgentKind::LiveSummary,
                    enabled: true,
                    output: "summary.md".into(),
                    instruction: Some("Keep this concise.".into()),
                    prompt_file: None,
                    workspace_paths: vec![],
                    write_paths: vec![],
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
                    workspace_context: vec![],
                },
                output_dir: output_dir.clone(),
                llm_provider: None,
                sequence: 1,
                session_log,
            })
            .unwrap();
        dispatcher.finish();
        let summary = fs::read_to_string(output_dir.join("summary.md")).unwrap();
        assert!(summary.contains("We enter the temple"));
        assert!(summary.contains("Keep this concise"));
        let events = fs::read_to_string(output_dir.join("events.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 2);
        let _ = fs::remove_file(output_dir.join("summary.md"));
        let _ = fs::remove_dir(output_dir);
    }

    #[test]
    fn workspace_documents_are_scoped_to_each_agent() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dnd-agent-workspace-{nonce}.md"));
        fs::write(&path, "private campaign fact").unwrap();
        let context = TranscriptContext {
            session_id: "session-1".into(),
            current: TranscriptSegment {
                id: "segment-1".into(),
                start_ms: 0,
                end_ms: 1_000,
                speaker_id: None,
                text: "We investigate the shrine".into(),
                confidence: None,
                status: SegmentStatus::Finalized,
            },
            recent: vec![],
            session_state: None,
            campaign_context: vec![],
            workspace_context: vec![WorkspaceDocument {
                path: "global.md".into(),
                content: "must be replaced".into(),
            }],
        };
        let agent = AgentConfig {
            id: "reader".into(),
            kind: AgentKind::LiveSummary,
            enabled: true,
            output: "reader.md".into(),
            instruction: None,
            prompt_file: None,
            workspace_paths: vec![path.display().to_string()],
            write_paths: vec![],
            run_every_segments: 1,
        };
        let scoped = context_for_agent(&context, &agent);
        assert_eq!(
            scoped.workspace_context,
            vec![WorkspaceDocument {
                path: path.display().to_string(),
                content: "private campaign fact".into(),
            }]
        );
        let _ = fs::remove_file(path);
    }
}
