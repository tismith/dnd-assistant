use dnd_assistant_core::{AgentOutput, TranscriptSegment};
use serde::Serialize;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

pub const DEFAULT_UI_ADDRESS: &str = "127.0.0.1:8787";

#[derive(Debug, Default, Serialize)]
pub struct LiveState {
    pub status: String,
    pub transcript: Vec<TranscriptSegment>,
    pub agent_outputs: Vec<AgentPanel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentPanel {
    pub agent_id: String,
    pub title: String,
    pub body: String,
}

pub type SharedLiveState = Arc<Mutex<LiveState>>;

pub fn new_state() -> SharedLiveState {
    Arc::new(Mutex::new(LiveState {
        status: "starting".into(),
        ..LiveState::default()
    }))
}

pub fn update_segment(state: &SharedLiveState, segment: &TranscriptSegment) {
    if let Ok(mut state) = state.lock() {
        state.transcript.push(segment.clone());
        let keep = state.transcript.len().saturating_sub(100);
        if keep > 0 {
            state.transcript.drain(..keep);
        }
    }
}

pub fn update_agent(state: &SharedLiveState, output: &AgentOutput) {
    if let Ok(mut state) = state.lock() {
        if let Some(existing) = state
            .agent_outputs
            .iter_mut()
            .find(|panel| panel.agent_id == output.agent_id)
        {
            existing.title = output.title.clone();
            existing.body = output.body.clone();
        } else {
            state.agent_outputs.push(AgentPanel {
                agent_id: output.agent_id.clone(),
                title: output.title.clone(),
                body: output.body.clone(),
            });
        }
    }
}

pub fn set_status(state: &SharedLiveState, status: impl Into<String>) {
    if let Ok(mut state) = state.lock() {
        state.status = status.into();
    }
}

pub fn start(
    state: SharedLiveState,
    address: String,
) -> std::io::Result<(SocketAddr, thread::JoinHandle<()>)> {
    let listener = TcpListener::bind(&address)?;
    let local_address = listener.local_addr()?;
    println!("live UI listening at http://{local_address}/");
    let handle = thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            serve(stream, &state);
        }
    });
    Ok((local_address, handle))
}

fn serve(mut stream: TcpStream, state: &SharedLiveState) {
    let mut request = [0_u8; 1024];
    let bytes_read = stream.read(&mut request).unwrap_or(0);
    let request = String::from_utf8_lossy(&request[..bytes_read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    match path {
        "/" => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML,
        ),
        "/api/state" => {
            let body = state_json(state);
            respond(&mut stream, "200 OK", "application/json", &body);
        }
        _ => respond(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n",
        ),
    }
}

fn state_json(state: &SharedLiveState) -> String {
    state
        .lock()
        .ok()
        .and_then(|state| serde_json::to_string(&*state).ok())
        .unwrap_or_else(|| "{\"status\":\"unavailable\"}".into())
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>D&amp;D Assistant</title>
  <style>
    :root { color-scheme: dark; font: 16px system-ui, sans-serif; }
    body { margin: 0; background: #17151d; color: #eee9e1; }
    header { padding: 1rem 1.4rem; border-bottom: 1px solid #3b3548; display: flex; justify-content: space-between; }
    main { display: grid; grid-template-columns: minmax(0, 1.4fr) minmax(18rem, 1fr); gap: 1rem; padding: 1rem; }
    section { background: #211e2a; border: 1px solid #3b3548; border-radius: .5rem; padding: 1rem; }
    h1, h2, h3 { margin-top: 0; } h1 { font-size: 1.2rem; } h2 { font-size: 1rem; }
    #transcript { max-height: 75vh; overflow: auto; }
    .segment { border-bottom: 1px solid #332e3e; padding: .55rem 0; }
    .time, .speaker, .status { color: #b9a8d9; font-size: .8rem; }
    .agent { border-top: 1px solid #3b3548; margin-top: 1rem; padding-top: 1rem; white-space: pre-wrap; }
    @media (max-width: 800px) { main { grid-template-columns: 1fr; } }
  </style>
</head>
<body>
  <header><h1>LIVE SESSION</h1><span id="status" class="status">starting</span></header>
  <main><section><h2>Transcript</h2><div id="transcript">Waiting for transcript…</div></section>
    <section><h2>Agents</h2><div id="agents">Waiting for agent output…</div></section></main>
  <script>
    const esc = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
    const time = ms => `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}`;
    async function refresh() {
      try {
        const state = await (await fetch('/api/state')).json();
        document.querySelector('#status').textContent = state.status;
        document.querySelector('#transcript').innerHTML = state.transcript.length ? state.transcript.map(s =>
          `<div class="segment"><span class="time">${time(s.start_ms)}</span> <span class="speaker">${esc(s.speaker_id || 'unknown speaker')}</span><br>${esc(s.text)}</div>`).join('') : 'Waiting for transcript…';
        document.querySelector('#agents').innerHTML = state.agent_outputs.length ? state.agent_outputs.map(a =>
          `<div class="agent"><h3>${esc(a.title)} <small>(${esc(a.agent_id)})</small></h3>${esc(a.body)}</div>`).join('') : 'Waiting for agent output…';
        const transcript = document.querySelector('#transcript'); transcript.scrollTop = transcript.scrollHeight;
      } catch (_) { document.querySelector('#status').textContent = 'UI disconnected'; }
    }
    refresh(); setInterval(refresh, 1000);
  </script>
</body>
</html>"##;

#[cfg(test)]
mod tests {
    use super::*;
    use dnd_assistant_core::{AgentKind, SegmentStatus};

    #[test]
    fn state_keeps_recent_transcript_and_latest_agent_output() {
        let state = new_state();
        let segment = TranscriptSegment {
            id: "segment-1".into(),
            start_ms: 0,
            end_ms: 1_000,
            speaker_id: None,
            text: "I search the altar".into(),
            confidence: None,
            status: SegmentStatus::Finalized,
        };
        update_segment(&state, &segment);
        update_agent(
            &state,
            &AgentOutput {
                agent_id: "summary".into(),
                kind: AgentKind::LiveSummary,
                title: "Summary".into(),
                body: "The party searched.".into(),
            },
        );
        update_agent(
            &state,
            &AgentOutput {
                agent_id: "summary".into(),
                kind: AgentKind::LiveSummary,
                title: "Summary".into(),
                body: "The party searched the altar.".into(),
            },
        );
        let state = state.lock().unwrap();
        assert_eq!(state.transcript.len(), 1);
        assert_eq!(state.agent_outputs.len(), 1);
        assert!(state.agent_outputs[0].body.contains("altar"));
    }

    #[test]
    fn api_snapshot_projects_state_as_json() {
        let state = new_state();
        set_status(&state, "running");
        let snapshot = state_json(&state);
        assert!(snapshot.contains("\"status\":\"running\""));
    }
}
