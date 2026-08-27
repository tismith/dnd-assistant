use crate::{SessionState, TranscriptSegment};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Recorder,
    LiveSummary,
    NextSteps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: String,
    pub kind: AgentKind,
    pub enabled: bool,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptContext {
    pub session_id: String,
    pub current: TranscriptSegment,
    pub recent: Vec<TranscriptSegment>,
    pub session_state: Option<SessionState>,
    pub campaign_context: Vec<String>,
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
    configs
        .iter()
        .filter(|config| config.enabled)
        .map(|config| run_builtin_agent(config, context))
        .collect()
}

pub fn run_builtin_agent(config: &AgentConfig, context: &TranscriptContext) -> AgentOutput {
    let (title, body) = match &config.kind {
        AgentKind::Recorder => (
            "Transcript".into(),
            serde_json::to_string(&context.current).expect("transcript segment serializes"),
        ),
        AgentKind::LiveSummary => ("Live session summary".into(), render_summary(context)),
        AgentKind::NextSteps => ("GM next-step options".into(), render_next_steps(context)),
    };
    AgentOutput {
        agent_id: config.id.clone(),
        kind: config.kind.clone(),
        title,
        body,
    }
}

fn render_summary(context: &TranscriptContext) -> String {
    let mut output = String::from("# Live Session Summary\n\n");
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

fn render_next_steps(context: &TranscriptContext) -> String {
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
            },
            &context,
        );
        let next_steps = run_builtin_agent(
            &AgentConfig {
                id: "next".into(),
                kind: AgentKind::NextSteps,
                enabled: true,
                output: "next.md".into(),
            },
            &context,
        );
        assert!(summary.body.contains("I search the altar"));
        assert!(next_steps.body.contains("Follow up on: I search the altar"));
        assert!(next_steps.body.contains("Ember Gem"));
    }
}
