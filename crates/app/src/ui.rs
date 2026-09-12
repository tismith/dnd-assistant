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
    pub session_active: bool,
    pub paused: bool,
    #[serde(skip)]
    pub stop_requested: bool,
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

pub fn start_session(state: &SharedLiveState) {
    if let Ok(mut state) = state.lock() {
        state.session_active = true;
        state.paused = false;
        state.stop_requested = false;
        state.status = "running".into();
    }
}

pub fn toggle_pause(state: &SharedLiveState) {
    if let Ok(mut state) = state.lock()
        && state.session_active
    {
        state.paused = !state.paused;
        state.status = if state.paused { "paused" } else { "running" }.into();
    }
}

pub fn stop_session(state: &SharedLiveState) {
    if let Ok(mut state) = state.lock() {
        state.session_active = false;
        state.paused = false;
        state.stop_requested = true;
        state.status = "stopping".into();
    }
}

pub fn control_snapshot(state: &SharedLiveState) -> (bool, bool, bool) {
    state
        .lock()
        .map(|state| (state.session_active, state.paused, state.stop_requested))
        .unwrap_or((false, false, true))
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
    let request_parts = request
        .lines()
        .next()
        .map(|line| line.split_whitespace())
        .map(|mut parts| (parts.next().unwrap_or("GET"), parts.next().unwrap_or("/")))
        .unwrap_or(("GET", "/"));
    let (method, path) = request_parts;
    match (method, path) {
        ("GET", "/") => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML,
        ),
        ("GET", "/api/state") => {
            let body = state_json(state);
            respond(&mut stream, "200 OK", "application/json", &body);
        }
        ("POST", "/api/session/start") => {
            start_session(state);
            respond(&mut stream, "200 OK", "application/json", "{\"ok\":true}");
        }
        ("POST", "/api/session/pause") => {
            toggle_pause(state);
            respond(&mut stream, "200 OK", "application/json", "{\"ok\":true}");
        }
        ("POST", "/api/session/stop") => {
            stop_session(state);
            respond(&mut stream, "200 OK", "application/json", "{\"ok\":true}");
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
    :root { color-scheme: dark; font: 15px/1.5 Inter, ui-sans-serif, system-ui, sans-serif; --ink: #f5efe5; --muted: #a99fb5; --panel: rgba(35, 29, 48, .86); --line: #493d5d; --gold: #e8b86a; --violet: #a98be5; }
    * { box-sizing: border-box; }
    body { min-height: 100vh; margin: 0; color: var(--ink); background: radial-gradient(circle at 12% -10%, #49325d 0, transparent 35rem), radial-gradient(circle at 100% 0, #263e50 0, transparent 32rem), #121019; }
    header { padding: 1.35rem clamp(1rem, 4vw, 3rem) 1.1rem; border-bottom: 1px solid rgba(174, 148, 211, .18); display: flex; align-items: center; justify-content: space-between; gap: 1rem; backdrop-filter: blur(12px); }
    .brand { display: flex; align-items: center; gap: .85rem; }
    .brand-mark { display: grid; place-items: center; width: 2.5rem; height: 2.5rem; border: 1px solid #d6a85d; border-radius: .8rem; color: #17121f; background: linear-gradient(135deg, #f6d58e, #b77bd2); box-shadow: 0 0 2rem rgba(197, 135, 224, .25); font-size: 1.25rem; }
    .eyebrow { margin: 0; color: var(--gold); font-size: .7rem; font-weight: 800; letter-spacing: .16em; text-transform: uppercase; }
    h1, h2, h3, p { margin-top: 0; } h1 { margin-bottom: .12rem; font-size: 1.3rem; letter-spacing: -.02em; } h2 { margin-bottom: .25rem; font-size: 1rem; } h3 { margin-bottom: .4rem; font-size: .92rem; }
    .subtitle { margin: 0; color: var(--muted); font-size: .82rem; }
    .status-pill { display: flex; align-items: center; gap: .45rem; padding: .42rem .75rem; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); background: rgba(24, 19, 34, .7); font-size: .78rem; font-weight: 700; text-transform: capitalize; }
    .status-pill::before { width: .48rem; height: .48rem; border-radius: 50%; background: #777; content: ''; }
    .status-pill.running { border-color: rgba(113, 211, 166, .45); color: #9de8c4; } .status-pill.running::before { background: #70d3a7; box-shadow: 0 0 .6rem #70d3a7; }
    .status-pill.paused { border-color: rgba(232, 184, 106, .45); color: var(--gold); } .status-pill.paused::before { background: var(--gold); }
    .status-pill.stopping, .status-pill.stopped { color: var(--muted); }
    .shell { width: min(1500px, 100%); margin: 0 auto; }
    .toolbar { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: 1.1rem clamp(1rem, 4vw, 3rem) .7rem; }
    .controls { display: flex; flex-wrap: wrap; gap: .5rem; }
    button { border: 1px solid #65527f; border-radius: .55rem; padding: .58rem .85rem; color: var(--ink); background: #302741; cursor: pointer; font: inherit; font-size: .82rem; font-weight: 700; transition: transform .15s, border-color .15s, background .15s; }
    button:hover:not(:disabled) { transform: translateY(-1px); border-color: var(--violet); background: #403355; }
    button.primary { border-color: #c49351; color: #211823; background: linear-gradient(135deg, #f0c875, #c795df); }
    button.danger { border-color: #8d526b; color: #e9a9bc; }
    button:disabled { cursor: not-allowed; opacity: .38; }
    .metrics { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: .7rem; padding: 0 clamp(1rem, 4vw, 3rem) .8rem; }
    .metric { padding: .75rem .9rem; border: 1px solid rgba(174, 148, 211, .18); border-radius: .7rem; background: rgba(35, 29, 48, .55); }
    .metric-label { color: var(--muted); font-size: .7rem; font-weight: 800; letter-spacing: .1em; text-transform: uppercase; }
    .metric-value { display: block; margin-top: .1rem; color: var(--ink); font-size: 1.05rem; font-weight: 750; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    main { display: grid; grid-template-columns: minmax(0, 1.35fr) minmax(20rem, .9fr); gap: .9rem; padding: .2rem clamp(1rem, 4vw, 3rem) 2rem; }
    section { min-width: 0; overflow: hidden; border: 1px solid rgba(174, 148, 211, .2); border-radius: .85rem; background: var(--panel); box-shadow: 0 1rem 3rem rgba(0, 0, 0, .18); }
    .section-heading { display: flex; align-items: baseline; justify-content: space-between; gap: .6rem; padding: 1rem 1.1rem .8rem; border-bottom: 1px solid rgba(174, 148, 211, .14); }
    .section-heading h2 { margin: 0; } .section-hint { color: var(--muted); font-size: .72rem; }
    #transcript { max-height: 68vh; overflow: auto; padding: .25rem 1.1rem 1rem; }
    .segment { padding: .75rem 0; border-bottom: 1px solid rgba(174, 148, 211, .1); }
    .segment:last-child { border-bottom: 0; } .segment-text { margin-top: .18rem; color: #f4eee7; }
    .time, .speaker, .status { color: var(--violet); font-size: .74rem; font-weight: 700; } .speaker { margin-left: .45rem; color: var(--gold); }
    .agent-tabs { display: flex; flex-wrap: wrap; gap: .4rem; padding: .8rem 1.1rem 0; }
    .agent-tab { padding: .35rem .6rem; border-radius: .45rem; font-size: .75rem; }
    .agent-tab.active { border-color: var(--gold); color: #211823; background: linear-gradient(135deg, #edc477, #bf91db); }
    #agents { max-height: 68vh; overflow: auto; padding: 0 1.1rem 1rem; }
    .agent { margin-top: .9rem; padding: .85rem .9rem; border: 1px solid rgba(174, 148, 211, .16); border-radius: .65rem; background: rgba(19, 16, 27, .45); white-space: pre-wrap; }
    .agent small { color: var(--muted); font-weight: 500; }
    .empty-state { display: grid; place-items: center; min-height: 10rem; padding: 2rem; color: var(--muted); text-align: center; }
    .empty-icon { display: block; margin-bottom: .45rem; color: var(--gold); font-size: 1.5rem; }
    @media (max-width: 800px) { header, .toolbar { align-items: flex-start; flex-direction: column; } main { grid-template-columns: 1fr; } .metrics { grid-template-columns: 1fr 1fr; } .metric:last-child { grid-column: 1 / -1; } }
  </style>
</head>
<body>
  <div class="shell">
  <header><div class="brand"><div class="brand-mark">✦</div><div><p class="eyebrow">D&amp;D Assistant</p><h1>Live session cockpit</h1><p class="subtitle">Listen closely. Keep the story moving.</p></div></div><span id="status" class="status-pill">starting</span></header>
  <div class="toolbar"><nav class="controls"><button id="start" class="primary" onclick="action('start')">▶ Start session</button><button id="pause" onclick="action('pause')">Ⅱ Pause</button><button id="stop" class="danger" onclick="action('stop')">■ Stop session</button></nav></div>
  <div class="metrics"><div class="metric"><span class="metric-label">Transcript segments</span><span id="segment-count" class="metric-value">0</span></div><div class="metric"><span class="metric-label">Latest speaker</span><span id="latest-speaker" class="metric-value">—</span></div><div class="metric"><span class="metric-label">Agent signals</span><span id="agent-count" class="metric-value">0</span></div></div>
  <main><section><div class="section-heading"><h2>Transcript</h2><span class="section-hint">rolling live record</span></div><div id="transcript"><div class="empty-state"><div><span class="empty-icon">◌</span>Waiting for the first spoken scene…</div></div></div></section>
    <section><div class="section-heading"><h2>Agent room</h2><span class="section-hint">latest observations</span></div><div id="agent-tabs" class="agent-tabs"></div><div id="agents" class="empty-state"><div><span class="empty-icon">✧</span>Agents are listening…</div></div></section></main>
  </div>
  <script>
    const esc = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
    const time = ms => `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}`;
    async function action(name) { await fetch(`/api/session/${name}`, { method: 'POST' }); await refresh(); }
    let activeAgent = 'all';
    function renderAgents(outputs) {
      const tabs = document.querySelector('#agent-tabs');
      const content = document.querySelector('#agents');
      if (!outputs.length) {
        tabs.innerHTML = '';
        content.className = 'empty-state';
        content.innerHTML = '<div><span class="empty-icon">✧</span>Agents are listening…</div>';
        return;
      }
      if (activeAgent !== 'all' && !outputs.some(a => a.agent_id === activeAgent)) activeAgent = 'all';
      const tabItems = [{ id: 'all', label: 'All agents' }, ...outputs.map(a => ({ id: a.agent_id, label: a.agent_id }))];
      tabs.innerHTML = tabItems.map(tab =>
        `<button class="agent-tab${tab.id === activeAgent ? ' active' : ''}" data-agent="${esc(tab.id)}">${esc(tab.label)}</button>`).join('');
      tabs.querySelectorAll('.agent-tab').forEach(tab => tab.addEventListener('click', () => {
        activeAgent = tab.dataset.agent;
        renderAgents(outputs);
      }));
      const visible = activeAgent === 'all' ? outputs : outputs.filter(a => a.agent_id === activeAgent);
      content.className = '';
      content.innerHTML = visible.map(a =>
        `<div class="agent"><h3>${esc(a.title)} <small>(${esc(a.agent_id)})</small></h3>${esc(a.body)}</div>`).join('');
    }
    async function refresh() {
      try {
        const state = await (await fetch('/api/state')).json();
        const status = document.querySelector('#status'); status.textContent = state.status; status.className = `status-pill ${esc(state.status)}`;
        document.querySelector('#start').disabled = state.session_active;
        document.querySelector('#pause').disabled = !state.session_active;
        document.querySelector('#pause').textContent = state.paused ? 'Resume' : 'Pause';
        document.querySelector('#stop').disabled = !state.session_active;
        document.querySelector('#segment-count').textContent = state.transcript.length;
        const latest = state.transcript[state.transcript.length - 1]; document.querySelector('#latest-speaker').textContent = latest ? (latest.speaker_id || 'unknown speaker') : '—';
        document.querySelector('#agent-count').textContent = state.agent_outputs.length;
        document.querySelector('#transcript').innerHTML = state.transcript.length ? state.transcript.map(s =>
          `<div class="segment"><span class="time">${time(s.start_ms)}</span><span class="speaker">${esc(s.speaker_id || 'unknown speaker')}</span><div class="segment-text">${esc(s.text)}</div></div>`).join('') : '<div class="empty-state"><div><span class="empty-icon">◌</span>Waiting for the first spoken scene…</div></div>';
        renderAgents(state.agent_outputs);
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

    #[test]
    fn session_controls_transition_cleanly() {
        let state = new_state();
        assert_eq!(control_snapshot(&state), (false, false, false));
        start_session(&state);
        assert_eq!(control_snapshot(&state), (true, false, false));
        toggle_pause(&state);
        assert_eq!(control_snapshot(&state), (true, true, false));
        toggle_pause(&state);
        assert_eq!(control_snapshot(&state), (true, false, false));
        stop_session(&state);
        assert_eq!(control_snapshot(&state), (false, false, true));
    }
}
