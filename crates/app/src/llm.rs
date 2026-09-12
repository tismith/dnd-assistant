use dnd_assistant_core::{
    AgentConfig, AgentKind, AgentOutput, AgentRequest, CampaignUpdatePlan, TranscriptContext,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

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
    if let Some(variable) = provider.api_key_env.as_deref()
        && std::env::var(variable).is_err()
    {
        return Err(format!(
            "model API key environment variable {variable} is unset"
        ));
    }
    let request = AgentRequest {
        agent_id: config.id.clone(),
        instruction: config.instruction.clone(),
        context: context.clone(),
    };
    let system = load_prompt(config)?;
    let user = serde_json::to_string(&request.context).map_err(|error| error.to_string())?;
    if provider.endpoint == "codex://local" {
        let content = run_codex(
            &provider.model,
            &system,
            &format!(
                "Reason over this live session context. Workspace documents are read-only. Return only the useful agent output; do not edit files.\n{user}"
            ),
        )?;
        return Ok(AgentOutput {
            agent_id: config.id.clone(),
            kind: AgentKind::Llm,
            title: "Codex agent".into(),
            body: content,
        });
    }
    let body = ChatRequest {
        model: &provider.model,
        messages: [
            ChatMessage {
                role: "system",
                content: system.to_owned(),
            },
            ChatMessage {
                role: "user",
                content: format!(
                    "Reason over this live session context. Workspace documents are read-only\n{user}"
                ),
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

pub fn run_session_editor(
    provider: &LlmConfig,
    config: &AgentConfig,
    context: &TranscriptContext,
) -> Result<CampaignUpdatePlan, String> {
    let system = load_prompt(config)?;
    let context = serde_json::to_string(context).map_err(|error| error.to_string())?;
    let content = complete(provider, &system, &context)?;
    parse_session_update_plan(&content)
}

fn parse_session_update_plan(content: &str) -> Result<CampaignUpdatePlan, String> {
    let json = content
        .trim()
        .strip_prefix("```")
        .and_then(|text| text.strip_suffix("```"))
        .map(|text| text.trim_start_matches("json").trim())
        .unwrap_or(content.trim());
    serde_json::from_str(json).map_err(|error| format!("invalid session update plan: {error}"))
}

fn complete(provider: &LlmConfig, system: &str, user: &str) -> Result<String, String> {
    if provider.endpoint == "codex://local" {
        return run_codex(&provider.model, system, user);
    }
    let body = ChatRequest {
        model: &provider.model,
        messages: [
            ChatMessage {
                role: "system",
                content: system.to_owned(),
            },
            ChatMessage {
                role: "user",
                content: user.to_owned(),
            },
        ],
    };
    let mut call = ureq::post(&provider.endpoint)
        .config()
        .timeout_global(Some(Duration::from_secs(60)))
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
    response
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| "model response contained no choices".to_owned())
}

fn run_codex(model: &str, system: &str, user: &str) -> Result<String, String> {
    let root = std::env::temp_dir().join(format!(
        "dnd-assistant-codex-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
    ));
    std::fs::create_dir(&root).map_err(|error| format!("cannot create Codex sandbox: {error}"))?;
    let mut child = Command::new("codex")
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start local Codex app-server: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "local Codex app-server stdin was unavailable".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "local Codex app-server stdout was unavailable".to_owned())?;
    let (lines_sender, lines_receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut request_id = 0_u64;
    request_id += 1;
    send_rpc(
        &mut stdin,
        request_id,
        "initialize",
        serde_json::json!({
            "clientInfo": {"name": "dnd-assistant", "title": "D&D Assistant", "version": "0.1.0"},
            "capabilities": {"experimentalApi": true}
        }),
    )?;
    wait_for_response(&lines_receiver, request_id)?;
    send_notification(&mut stdin, "initialized", serde_json::json!({}))?;

    request_id += 1;
    let mut thread_params = serde_json::json!({
        "cwd": root,
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "ephemeral": true,
        "developerInstructions": system
    });
    if !model.is_empty() && model != "default" {
        thread_params["model"] = serde_json::Value::String(model.to_owned());
    }
    send_rpc(&mut stdin, request_id, "thread/start", thread_params)?;
    let thread_response = wait_for_response(&lines_receiver, request_id)?;
    let thread_id = thread_response["result"]["thread"]["id"]
        .as_str()
        .ok_or_else(|| "local Codex app-server returned no thread id".to_owned())?
        .to_owned();

    request_id += 1;
    send_rpc(
        &mut stdin,
        request_id,
        "turn/start",
        serde_json::json!({
            "threadId": thread_id,
            "input": [{"type": "text", "text": user}],
            "approvalPolicy": "never",
            "sandboxPolicy": {"type": "readOnly", "networkAccess": false}
        }),
    )?;
    wait_for_response(&lines_receiver, request_id)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut content = String::new();
    let mut last_delta: Option<Instant> = None;
    while Instant::now() < deadline {
        let line = match lines_receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_delta.is_some_and(|time| time.elapsed() >= Duration::from_secs(3)) {
                    break;
                }
                continue;
            }
            Err(error) => return Err(format!("local Codex app-server stopped: {error}")),
        };
        let message: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| format!("invalid local Codex app-server message: {error}"))?;
        match message["method"].as_str() {
            Some("item/agentMessage/delta") => {
                content.push_str(message["params"]["delta"].as_str().unwrap_or_default());
                last_delta = Some(Instant::now());
            }
            Some("item/completed") => {
                if message["params"]["item"]["type"] == "agentMessage" {
                    if content.is_empty() {
                        content = message["params"]["item"]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                    }
                    // The completed agent-message item is sufficient for this
                    // one-shot request; do not wait for unrelated server
                    // notifications such as MCP startup updates.
                    break;
                }
            }
            Some("turn/completed") => break,
            Some("error") => return Err(format!("local Codex app-server error: {line}")),
            _ => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir(&root);
    if content.trim().is_empty() {
        Err("local Codex app-server returned no output".into())
    } else {
        Ok(content.trim().to_owned())
    }
}

fn send_rpc(
    stdin: &mut impl Write,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let message =
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    writeln!(stdin, "{message}").map_err(|error| format!("cannot write Codex RPC request: {error}"))
}

fn send_notification(
    stdin: &mut impl Write,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let message = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
    writeln!(stdin, "{message}")
        .map_err(|error| format!("cannot write Codex RPC notification: {error}"))
}

fn wait_for_response(lines: &mpsc::Receiver<String>, id: u64) -> Result<serde_json::Value, String> {
    loop {
        let line = lines
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| format!("timed out waiting for Codex RPC response: {error}"))?;
        let message: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| format!("invalid Codex RPC message: {error}"))?;
        if message["id"].as_u64() == Some(id) {
            if message.get("error").is_some() {
                return Err(format!("Codex RPC error: {message}"));
            }
            return Ok(message);
        }
    }
}

fn load_prompt(config: &AgentConfig) -> Result<String, String> {
    let inline = config.instruction.as_deref().unwrap_or("").trim();
    let file = config
        .prompt_file
        .as_deref()
        .map(read_prompt_file)
        .transpose()?
        .unwrap_or_default();
    let prompt = [file.trim(), inline]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if prompt.is_empty() {
        Err(format!(
            "agent {} has no prompt_file or instruction",
            config.id
        ))
    } else {
        Ok(prompt)
    }
}

fn read_prompt_file(path: &str) -> Result<String, String> {
    let configured = std::path::Path::new(path);
    let mut candidates = Vec::new();
    if configured.is_absolute() {
        candidates.push(configured.to_owned());
    } else {
        candidates.push(
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join(path),
        );
        let config_base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|path| path.join(".config"))
            });
        if let Some(config_base) = config_base {
            candidates.push(config_base.join("dnd-assistant").join(path));
        }
        if let Ok(executable) = std::env::current_exe()
            && let Some(parent) = executable.parent()
        {
            candidates.push(parent.join(path));
            candidates.push(parent.join("../../").join(path));
            candidates.push(parent.join("../../../").join(path));
        }
    }
    for candidate in candidates {
        if let Ok(content) = std::fs::read_to_string(&candidate) {
            return Ok(content);
        }
    }
    Err(format!("cannot read agent prompt file: {path}"))
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
            workspace_context: vec![],
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
            prompt_file: None,
            workspace_paths: vec![],
            workspace_query: None,
            write_paths: vec![],
            include_campaign_context: true,
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
            workspace_context: vec![],
        };
        let provider = LlmConfig {
            endpoint: "http://127.0.0.1:1/v1/chat/completions".into(),
            model: "local-model".into(),
            api_key_env: Some("DND_ASSISTANT_TEST_KEY_UNSET".into()),
        };
        let error = run(&provider, &config, &context).unwrap_err();
        assert!(error.contains("DND_ASSISTANT_TEST_KEY_UNSET"));
    }

    #[test]
    fn session_update_plan_accepts_fenced_json() {
        let plan =
            parse_session_update_plan("```json\n{\"summary\":\"A discovery\",\"updates\":[]}\n```")
                .unwrap();
        assert_eq!(plan.summary, "A discovery");
        assert!(plan.updates.is_empty());
    }
}
