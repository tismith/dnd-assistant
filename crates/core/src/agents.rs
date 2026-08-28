use crate::{SessionState, TranscriptSegment};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Recorder,
    LiveSummary,
    NextSteps,
    Llm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: String,
    pub kind: AgentKind,
    pub enabled: bool,
    pub output: String,
    #[serde(default)]
    pub instruction: Option<String>,
    #[serde(default = "default_run_every_segments")]
    pub run_every_segments: usize,
}

fn default_run_every_segments() -> usize {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptContext {
    pub session_id: String,
    pub current: TranscriptSegment,
    pub recent: Vec<TranscriptSegment>,
    pub session_state: Option<SessionState>,
    pub campaign_context: Vec<String>,
}

/// Provider-neutral input for a configured agent. A future LLM runner can
/// consume this request without changing capture, persistence, or scheduling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRequest {
    pub agent_id: String,
    pub instruction: Option<String>,
    pub context: TranscriptContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOutput {
    pub agent_id: String,
    pub kind: AgentKind,
    pub title: String,
    pub body: String,
}

/// Fan out one immutable context to every enabled configured agent.
pub fn run_enabled_agents(
    configs: &[AgentConfig],
    context: &TranscriptContext,
) -> Vec<AgentOutput> {
    run_enabled_agents_at(configs, context, 1)
}

/// Run enabled agents whose configured segment cadence is due. Sequence is
/// one-based so the first finalized segment always triggers default agents.
pub fn run_enabled_agents_at(
    configs: &[AgentConfig],
    context: &TranscriptContext,
    sequence: usize,
) -> Vec<AgentOutput> {
    configs
        .iter()
        .filter(|config| {
            config.enabled && sequence.is_multiple_of(config.run_every_segments.max(1))
        })
        .map(|config| run_builtin_agent(config, context))
        .collect()
}

pub fn run_builtin_agent(config: &AgentConfig, context: &TranscriptContext) -> AgentOutput {
    let (title, body) = match &config.kind {
        AgentKind::Recorder => (
            "Transcript".into(),
            serde_json::to_string(&context.current).expect("transcript segment serializes"),
        ),
        AgentKind::LiveSummary => (
            "Live session summary".into(),
            render_summary(config, context),
        ),
        AgentKind::NextSteps => (
            "GM next-step options".into(),
            render_next_steps(config, context),
        ),
        AgentKind::Llm => (
            "Model agent".into(),
            "This agent requires a configured model provider.".into(),
        ),
    };
    AgentOutput {
        agent_id: config.id.clone(),
        kind: config.kind.clone(),
        title,
        body,
    }
}

fn render_summary(config: &AgentConfig, context: &TranscriptContext) -> String {
    let mut output = String::from("# Live Session Summary\n\n");
    append_instruction(&mut output, config);
    if let Some(state) = &context.session_state {
        if let Some(location) = &state.current_location {
            output.push_str(&format!("**Current location:** {location}\n\n"));
        }
    }
    output.push_str("## Recent transcript\n\n");
    for segment in &context.recent {
        output.push_str(&format!("- {}\n", display_segment(segment)));
    }
    output
}

fn render_next_steps(config: &AgentConfig, context: &TranscriptContext) -> String {
    let mut intents = Vec::new();
    for segment in &context.recent {
        let text = segment.text.trim();
        let lower = text.to_ascii_lowercase();
        if [
            "i ",
            "we ",
            "let's",
            "lets ",
            "can we",
            "should we",
            "look ",
            "search ",
            "talk ",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            intents.push(text.to_owned());
        }
    }
    let mut output = String::from("Use these as prompts, not prescriptions.\n\n");
    append_instruction(&mut output, config);
    if intents.is_empty() {
        output.push_str("- No clear player intention detected yet.\n");
    } else {
        for intent in intents.iter().rev().take(3) {
            output.push_str(&format!("- Follow up on: {intent}\n"));
            output.push_str(
                "  - Offer a discovery, complication, or choice connected to that intent.\n",
            );
        }
    }
    if !context.campaign_context.is_empty() {
        output.push_str("\nRelevant campaign context:\n");
        for item in context.campaign_context.iter().take(3) {
            output.push_str(&format!("- {item}\n"));
        }
    }
    output
}

fn append_instruction(output: &mut String, config: &AgentConfig) {
    if let Some(instruction) = config
        .instruction
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        output.push_str(&format!("**Agent focus:** {instruction}\n\n"));
    }
}

fn display_segment(segment: &TranscriptSegment) -> String {
    let speaker = segment.speaker_id.as_deref().unwrap_or("unknown speaker");
    format!("[{speaker}] {}", segment.text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SegmentStatus;

    fn segment(id: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.into(),
            start_ms: 0,
            end_ms: 1_000,
            speaker_id: Some("speaker_1".into()),
            text: text.into(),
            confidence: None,
            status: SegmentStatus::Finalized,
        }
    }

    #[test]
    fn fanout_outputs_are_independent_and_contextual() {
        let context = TranscriptContext {
            session_id: "session-1".into(),
            current: segment("2", "I search the altar"),
            recent: vec![
                segment("1", "We enter the temple"),
                segment("2", "I search the altar"),
            ],
            session_state: None,
            campaign_context: vec!["The altar is connected to the Ember Gem".into()],
        };
        let summary = run_builtin_agent(
            &AgentConfig {
                id: "summary".into(),
                kind: AgentKind::LiveSummary,
                enabled: true,
                output: "summary.md".into(),
                instruction: Some("Keep attention on unresolved player questions.".into()),
                run_every_segments: 1,
            },
            &context,
        );
        let next_steps = run_builtin_agent(
            &AgentConfig {
                id: "next".into(),
                kind: AgentKind::NextSteps,
                enabled: true,
                output: "next.md".into(),
                instruction: None,
                run_every_segments: 1,
            },
            &context,
        );
        assert!(summary.body.contains("I search the altar"));
        assert!(summary.body.contains("unresolved player questions"));
        assert!(next_steps.body.contains("Follow up on: I search the altar"));
        assert!(next_steps.body.contains("Ember Gem"));
    }

    #[test]
    fn cadence_skips_agents_until_their_configured_turn() {
        let config = AgentConfig {
            id: "summary".into(),
            kind: AgentKind::LiveSummary,
            enabled: true,
            output: "summary.md".into(),
            instruction: None,
            run_every_segments: 3,
        };
        let context = TranscriptContext {
            session_id: "session-1".into(),
            current: segment("1", "We wait"),
            recent: vec![segment("1", "We wait")],
            session_state: None,
            campaign_context: vec![],
        };
        assert!(run_enabled_agents_at(&[config.clone()], &context, 1).is_empty());
        assert_eq!(run_enabled_agents_at(&[config], &context, 3).len(), 1);
    }

    #[test]
    fn older_agent_config_defaults_to_every_segment_without_instruction() {
        let config: AgentConfig = serde_json::from_str(
            r#"{"id":"summary","kind":"live_summary","enabled":true,"output":"summary.md"}"#,
        )
        .unwrap();
        assert_eq!(config.instruction, None);
        assert_eq!(config.run_every_segments, 1);
    }
}
