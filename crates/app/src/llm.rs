use dnd_assistant_core::{AgentConfig, AgentKind, AgentOutput, AgentRequest, TranscriptContext};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
}

#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: String,
}

pub fn run(
    provider: &LlmConfig,
    config: &AgentConfig,
    context: &TranscriptContext,
) -> Result<AgentOutput, String> {
    let request = AgentRequest {
        agent_id: config.id.clone(),
        instruction: config.instruction.clone(),
        context: context.clone(),
    };
    let system = request.instruction.as_deref().unwrap_or(
        "You are a concise tabletop RPG assistant. Return only useful observations or options.",
    );
    let user = serde_json::to_string(&request.context).map_err(|error| error.to_string())?;
    let body = ChatRequest {
        model: &provider.model,
        messages: [
            ChatMessage {
                role: "system",
                content: system.to_owned(),
            },
            ChatMessage {
                role: "user",
                content: format!("Reason over this live session context:\n{user}"),
            },
        ],
    };
    let mut call = ureq::post(&provider.endpoint)
        .config()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .header("Content-Type", "application/json");
    if let Some(variable) = provider.api_key_env.as_deref() {
        let key = std::env::var(variable)
            .map_err(|_| format!("model API key environment variable {variable} is unset"))?;
        call = call.header("Authorization", &format!("Bearer {key}"));
    }
    let payload = serde_json::to_vec(&body).map_err(|error| error.to_string())?;
    let mut response = call
        .send(payload)
        .map_err(|error| format!("model request failed: {error}"))?;
    let response_body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("invalid model response body: {error}"))?;
    let response: ChatResponse = serde_json::from_str(&response_body)
        .map_err(|error| format!("invalid model response: {error}"))?;
    let content = response
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| "model response contained no choices".to_owned())?;
    Ok(AgentOutput {
        agent_id: config.id.clone(),
        kind: AgentKind::Llm,
        title: "Model agent".into(),
        body: content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dnd_assistant_core::{SegmentStatus, TranscriptSegment};

    #[test]
    fn request_context_contains_instruction_inputs() {
        let context = TranscriptContext {
            session_id: "session-1".into(),
            current: TranscriptSegment {
                id: "segment-1".into(),
                start_ms: 0,
                end_ms: 1_000,
                speaker_id: Some("speaker_1".into()),
                text: "I inspect the altar".into(),
                confidence: None,
                status: SegmentStatus::Finalized,
            },
            recent: vec![],
            session_state: None,
            campaign_context: vec!["The altar hides an Amber Gem".into()],
        };
        let request = AgentRequest {
            agent_id: "gm".into(),
            instruction: Some("Suggest two non-prescriptive options.".into()),
            context,
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("Suggest two non-prescriptive options"));
        assert!(encoded.contains("Amber Gem"));
    }

    #[test]
    fn missing_api_key_fails_before_network_request() {
        let config = AgentConfig {
            id: "gm".into(),
            kind: AgentKind::Llm,
            enabled: true,
            output: "gm.md".into(),
            instruction: None,
            run_every_segments: 1,
        };
        let context = TranscriptContext {
            session_id: "session-1".into(),
            current: TranscriptSegment {
                id: "segment-1".into(),
                start_ms: 0,
                end_ms: 1_000,
                speaker_id: None,
                text: "Hello".into(),
                confidence: None,
                status: SegmentStatus::Finalized,
            },
            recent: vec![],
            session_state: None,
            campaign_context: vec![],
        };
        let provider = LlmConfig {
            endpoint: "http://127.0.0.1:1/v1/chat/completions".into(),
            model: "local-model".into(),
            api_key_env: Some("DND_ASSISTANT_TEST_KEY_UNSET".into()),
        };
        let error = run(&provider, &config, &context).unwrap_err();
        assert!(error.contains("DND_ASSISTANT_TEST_KEY_UNSET"));
    }
}
