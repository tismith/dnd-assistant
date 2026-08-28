use dnd_assistant_audio::{
    default_input_description, downmix_and_resample, rms, start_default_input,
};
use dnd_assistant_core::{
    AgentConfig, AgentKind, Event, ReconciliationInput, SegmentStatus, SessionState,
    SpeakerSegment, TranscriptContext, TranscriptSegment, attribute_speaker,
};
use dnd_assistant_models::{default_model_cache_dir, ensure_model};
use dnd_assistant_stt::WhisperTranscriber;
mod agent_runtime;
mod llm;
mod session;
mod ui;
use session::SessionLog;
use std::{
    env, fs,
    io::{BufRead, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, serde::Deserialize)]
struct AppConfig {
    #[serde(default)]
    session_id: Option<String>,
    agents: Vec<AgentConfig>,
    #[serde(default)]
    campaign_context: Vec<String>,
    #[serde(default)]
    model_sha256: Option<String>,
    #[serde(default)]
    llm: Option<llm::LlmConfig>,
}

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("validate") => validate(),
        Some("audio-info") => audio_info(),
        Some("capture") => capture(),
        Some("live") => live(args.next(), args.next(), args.next()),
        Some("reconcile-demo") => reconcile_demo(),
        Some("replay") => replay(args.next(), args.next(), args.next()),
        Some("stream") => stream(args.next(), args.next()),
        Some("record") => record(
            args.next()
                .map(PathBuf::from)
                .unwrap_or_else(|| default_data_dir().join("spike.wav")),
        ),
        _ => usage(),
    }
}

fn validate() {
    for command in ["arecord", "whisper-stream"] {
        let found = Command::new("sh")
            .args(["-c", &format!("command -v {command}")])
            .status()
            .is_ok_and(|s| s.success());
        println!("{command}: {}", if found { "available" } else { "missing" });
    }
}

fn audio_info() {
    match default_input_description() {
        Ok((name, format)) => println!(
            "input: {name} ({} Hz, {} channels)",
            format.sample_rate, format.channels
        ),
        Err(error) => {
            eprintln!("audio unavailable: {error}");
            std::process::exit(1);
        }
    }
}

fn capture() {
    let capture = start_default_input(8).unwrap_or_else(|error| {
        eprintln!("audio unavailable: {error}");
        std::process::exit(1);
    });
    println!(
        "capturing {} Hz, {} channels; press Ctrl-C to stop",
        capture.format.sample_rate, capture.format.channels
    );
    let mut chunks = 0_u64;
    let mut samples = 0_u64;
    for chunk in capture.chunks {
        chunks += 1;
        samples += chunk.samples.len() as u64;
        if chunks % 20 == 0 {
            println!("captured {chunks} chunks / {samples} samples");
        }
    }
}

fn live(model_path: Option<String>, config_path: Option<String>, output_dir: Option<String>) {
    let (Some(model_path), Some(config_path)) = (model_path, config_path) else {
        usage();
        std::process::exit(2);
    };
    let mut config: AppConfig = read_json(&config_path);
    let campaign_context = load_campaign_context(&config);
    let output_dir = resolve_output_dir(output_dir, &config);
    set_default_session_id(&mut config, &output_dir);
    let model_path = if model_path.starts_with("http://") || model_path.starts_with("https://") {
        let filename = model_path
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("model.bin");
        let destination = default_model_cache_dir().join(filename);
        ensure_model(&model_path, &destination, config.model_sha256.as_deref()).unwrap_or_else(
            |error| {
                eprintln!("model download unavailable: {error}");
                std::process::exit(1);
            },
        )
    } else {
        PathBuf::from(&model_path)
    };
    let transcriber = WhisperTranscriber::load(&model_path).unwrap_or_else(|error| {
        eprintln!("transcription unavailable: {error}");
        std::process::exit(1);
    });
    let capture = start_default_input(128).unwrap_or_else(|error| {
        eprintln!("audio unavailable: {error}");
        std::process::exit(1);
    });
    let ui_state = ui::new_state();
    let ui_address =
        env::var("DND_ASSISTANT_UI_ADDRESS").unwrap_or_else(|_| ui::DEFAULT_UI_ADDRESS.into());
    if let Err(error) = ui::start(ui_state.clone(), ui_address) {
        eprintln!("live UI unavailable; continuing without it: {error}");
    }
    ui::set_status(&ui_state, "running");
    fs::create_dir_all(&output_dir)
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", output_dir.display()));
    let session_log = SessionLog::open(&output_dir)
        .unwrap_or_else(|error| panic!("cannot open session event log: {error}"));
    let (window_sender, window_receiver) = std::sync::mpsc::sync_channel::<(u64, Vec<f32>)>(4);
    let worker_config = config.clone();
    let worker_context = campaign_context.clone();
    let worker_output_dir = output_dir.clone();
    let worker_ui_state = ui_state.clone();
    let agent_dispatcher = agent_runtime::AgentDispatcher::start(Some(ui_state.clone()));
    let transcription_worker = std::thread::spawn(move || {
        let mut transcriber = transcriber;
        let mut recent = Vec::new();
        let silence_rms = env::var("DND_ASSISTANT_SILENCE_RMS")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.005);
        for (window_start_ms, audio) in window_receiver {
            if rms(&audio) < silence_rms {
                continue;
            }
            match transcriber.transcribe_window(&audio) {
                Ok(segments) => {
                    for mut segment in segments {
                        segment.start_ms += window_start_ms;
                        segment.end_ms += window_start_ms;
                        segment.status = SegmentStatus::Finalized;
                        process_segment(
                            &worker_config,
                            &worker_context,
                            &mut recent,
                            &worker_output_dir,
                            segment,
                            &session_log,
                            Some(&worker_ui_state),
                            Some(&agent_dispatcher),
                        );
                    }
                }
                Err(error) => eprintln!("transcription window failed; continuing capture: {error}"),
            }
        }
    });
    let channels = capture.format.channels as usize;
    let sample_rate = capture.format.sample_rate;
    let window_input_samples = sample_rate as usize * channels * 5;
    let mut input_samples = Vec::with_capacity(window_input_samples);
    let mut window_start_ms = 0_u64;
    println!(
        "live transcription started at {} Hz / {} channels; press Ctrl-C to stop",
        sample_rate, channels
    );
    for chunk in capture.chunks {
        input_samples.extend(chunk.samples);
        while input_samples.len() >= window_input_samples {
            let window: Vec<f32> = input_samples.drain(..window_input_samples).collect();
            let audio = downmix_and_resample(&window, channels, sample_rate, 16_000);
            if window_sender.send((window_start_ms, audio)).is_err() {
                eprintln!("transcription worker stopped; ending capture");
                return;
            }
            window_start_ms += 5_000;
        }
    }
    drop(window_sender);
    let _ = transcription_worker.join();
}

fn default_data_dir() -> PathBuf {
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("dnd-assistant").join("sessions")
}

fn resolve_output_dir(explicit: Option<String>, config: &AppConfig) -> PathBuf {
    if let Some(path) = explicit {
        return PathBuf::from(path);
    }
    let session_id = config
        .session_id
        .as_deref()
        .map(sanitize_session_id)
        .unwrap_or_else(|| {
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis());
            format!("session-{millis}")
        });
    default_data_dir().join(session_id)
}

fn set_default_session_id(config: &mut AppConfig, output_dir: &Path) {
    if config.session_id.is_none() {
        if let Some(name) = output_dir.file_name().and_then(|name| name.to_str()) {
            config.session_id = Some(name.to_owned());
        }
    }
}

fn sanitize_session_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "session".into()
    } else {
        sanitized
    }
}

fn record(path: PathBuf) {
    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("cannot create {}: {error}", parent.display());
            std::process::exit(1);
        }
    }
    let capture = start_default_input(128).unwrap_or_else(|error| {
        eprintln!("audio unavailable: {error}");
        std::process::exit(1);
    });
    let format = capture.format;
    let mut file = fs::File::create(&path)
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
    file.write_all(&wav_header(0, format.sample_rate, format.channels))
        .unwrap_or_else(|error| panic!("cannot write WAV header: {error}"));
    println!(
        "Recording 10 seconds to {} (native CPAL capture)...",
        path.display()
    );
    let started = Instant::now();
    let mut sample_count = 0_u64;
    while started.elapsed() < Duration::from_secs(10) {
        if let Ok(chunk) = capture.chunks.recv_timeout(Duration::from_millis(100)) {
            for sample in chunk.samples {
                let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                file.write_all(&pcm.to_le_bytes())
                    .unwrap_or_else(|error| panic!("cannot write WAV data: {error}"));
                sample_count += 1;
            }
        }
    }
    let data_bytes = sample_count.saturating_mul(2);
    if data_bytes > u32::MAX as u64 {
        panic!("recording is too large for a classic WAV file");
    }
    file.seek(SeekFrom::Start(0))
        .unwrap_or_else(|error| panic!("cannot seek WAV header: {error}"));
    file.write_all(&wav_header(
        data_bytes as u32,
        format.sample_rate,
        format.channels,
    ))
    .unwrap_or_else(|error| panic!("cannot finalize WAV header: {error}"));
    file.sync_all()
        .unwrap_or_else(|error| panic!("cannot sync recording: {error}"));
    println!("Audio capture complete: {}", path.display());
}

fn wav_header(data_bytes: u32, sample_rate: u32, channels: u16) -> [u8; 44] {
    let block_align = channels * 2;
    let byte_rate = sample_rate * block_align as u32;
    let riff_size = 36_u32.saturating_add(data_bytes);
    let mut header = [0_u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&riff_size.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&16_u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    header
}

fn reconcile_demo() {
    let segment = TranscriptSegment {
        id: "demo-1".into(),
        start_ms: 10_200,
        end_ms: 14_800,
        speaker_id: None,
        text: "I search the altar for traps".into(),
        confidence: Some(0.9),
        status: SegmentStatus::Provisional,
    };
    let result = attribute_speaker(ReconciliationInput {
        transcript: segment,
        speaker_segments: vec![SpeakerSegment {
            start_ms: 9_900,
            end_ms: 15_100,
            speaker_id: "speaker_2".into(),
        }],
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("demo serializes")
    );
}

fn usage() {
    println!(
        "Usage: cargo run -p dnd-assistant -- <validate|audio-info|capture|live <model> <config.json> [output-dir]|record [path]|reconcile-demo|replay <config.json> <transcript.jsonl> <output-dir>|stream <config.json> [output-dir]>"
    );
}

fn replay(
    config_path: Option<String>,
    transcript_path: Option<String>,
    output_dir: Option<String>,
) {
    let (Some(config_path), Some(transcript_path), Some(output_dir)) =
        (config_path, transcript_path, output_dir)
    else {
        usage();
        std::process::exit(2);
    };
    let config: AppConfig = read_json(&config_path);
    let campaign_context = load_campaign_context(&config);
    let segments: Vec<TranscriptSegment> = fs::read_to_string(&transcript_path)
        .unwrap_or_else(|error| panic!("cannot read {transcript_path}: {error}"))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("invalid transcript line: {error}"))
        })
        .collect();
    fs::create_dir_all(&output_dir)
        .unwrap_or_else(|error| panic!("cannot create {output_dir}: {error}"));
    let session_log = SessionLog::open(Path::new(&output_dir))
        .unwrap_or_else(|error| panic!("cannot open session event log: {error}"));
    let agent_dispatcher = agent_runtime::AgentDispatcher::start(None);
    let mut recent = Vec::new();
    for segment in segments {
        process_segment(
            &config,
            &campaign_context,
            &mut recent,
            Path::new(&output_dir),
            segment,
            &session_log,
            None,
            Some(&agent_dispatcher),
        );
    }
    agent_dispatcher.finish();
}

fn stream(config_path: Option<String>, output_dir: Option<String>) {
    let Some(config_path) = config_path else {
        usage();
        std::process::exit(2);
    };
    let mut config: AppConfig = read_json(&config_path);
    let campaign_context = load_campaign_context(&config);
    let output_dir = resolve_output_dir(output_dir, &config);
    set_default_session_id(&mut config, &output_dir);
    fs::create_dir_all(&output_dir)
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", output_dir.display()));
    let session_log = SessionLog::open(&output_dir)
        .unwrap_or_else(|error| panic!("cannot open session event log: {error}"));
    let agent_dispatcher = agent_runtime::AgentDispatcher::start(None);
    let stdin = std::io::stdin();
    let mut recent = Vec::new();
    for line in stdin.lock().lines() {
        let line = line.expect("read transcript stream line");
        if line.trim().is_empty() {
            continue;
        }
        let segment: TranscriptSegment = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("invalid transcript stream line: {error}"));
        process_segment(
            &config,
            &campaign_context,
            &mut recent,
            &output_dir,
            segment,
            &session_log,
            None,
            Some(&agent_dispatcher),
        );
    }
    agent_dispatcher.finish();
}

fn load_campaign_context(config: &AppConfig) -> Vec<String> {
    config
        .campaign_context
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("cannot read campaign context {path}: {error}"))
        })
        .collect()
}

fn process_segment(
    config: &AppConfig,
    campaign_context: &[String],
    recent: &mut Vec<TranscriptSegment>,
    output_dir: &Path,
    segment: TranscriptSegment,
    session_log: &SessionLog,
    ui_state: Option<&ui::SharedLiveState>,
    dispatcher: Option<&agent_runtime::AgentDispatcher>,
) {
    if let Err(error) = session_log.append(&Event::TranscriptSegmentCreated {
        segment: segment.clone(),
    }) {
        eprintln!("session event log append failed; continuing agents: {error}");
    }
    if segment.status == SegmentStatus::Finalized {
        if let Err(error) = session_log.append(&Event::TranscriptSegmentFinalized {
            segment_id: segment.id.clone(),
        }) {
            eprintln!("session event log append failed; continuing agents: {error}");
        }
    }
    recent.push(segment);
    if let Some(ui_state) = ui_state {
        ui::update_segment(ui_state, recent.last().expect("current segment exists"));
    }
    let recent_window = recent
        .iter()
        .rev()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let context = TranscriptContext {
        session_id: config
            .session_id
            .clone()
            .unwrap_or_else(|| "live-session".into()),
        current: recent.last().cloned().expect("current segment exists"),
        recent: recent_window,
        session_state: Some(SessionState::default()),
        campaign_context: campaign_context.to_vec(),
    };
    let job = agent_runtime::AgentJob {
        configs: config.agents.clone(),
        context,
        output_dir: output_dir.to_owned(),
        llm_provider: config.llm.clone(),
        sequence: recent.len(),
        session_log: session_log.clone(),
    };
    if let Some(dispatcher) = dispatcher {
        if let Err(error) = dispatcher.submit(job) {
            eprintln!("agent dispatcher failed; continuing transcript: {error}");
        }
    } else {
        agent_runtime::run_job(job, None);
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> T {
    let contents =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("invalid JSON in {path}: {error}"))
}

fn write_agent_output(
    output_dir: &Path,
    config: &AgentConfig,
    result: &dnd_assistant_core::AgentOutput,
) -> Result<(), String> {
    let configured_path = Path::new(&config.output);
    if configured_path.is_absolute()
        || configured_path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err("output path must be relative and cannot contain '..'".into());
    }
    if configured_path == Path::new("events.jsonl") {
        return Err("events.jsonl is reserved for the session event log".into());
    }
    let path = output_dir.join(&config.output);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match &config.kind {
        AgentKind::Recorder => {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
            writeln!(file, "{}", result.body).map_err(|error| error.to_string())?;
        }
        AgentKind::LiveSummary | AgentKind::NextSteps | AgentKind::Llm => {
            fs::write(path, format!("{}\n\n{}", result.title, result.body))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfig, resolve_output_dir, sanitize_session_id, wav_header, write_agent_output,
    };
    use dnd_assistant_core::{AgentConfig, AgentKind, AgentOutput};
    use std::path::Path;

    #[test]
    fn agent_output_cannot_escape_session_directory() {
        let config = AgentConfig {
            id: "bad".into(),
            kind: AgentKind::Recorder,
            enabled: true,
            output: "../outside.jsonl".into(),
            instruction: None,
            run_every_segments: 1,
        };
        let output = AgentOutput {
            agent_id: "bad".into(),
            kind: AgentKind::Recorder,
            title: "Transcript".into(),
            body: "{}".into(),
        };
        assert!(write_agent_output(Path::new("/tmp"), &config, &output).is_err());
    }

    #[test]
    fn agent_cannot_overwrite_session_event_log() {
        let config = AgentConfig {
            id: "bad".into(),
            kind: AgentKind::Recorder,
            enabled: true,
            output: "events.jsonl".into(),
            instruction: None,
            run_every_segments: 1,
        };
        let output = AgentOutput {
            agent_id: "bad".into(),
            kind: AgentKind::Recorder,
            title: "Transcript".into(),
            body: "{}".into(),
        };
        assert!(write_agent_output(Path::new("/tmp"), &config, &output).is_err());
    }

    #[test]
    fn wav_header_describes_pcm_recording() {
        let header = wav_header(8_820, 44_100, 2);
        assert_eq!(&header[0..4], b"RIFF");
        assert_eq!(&header[8..12], b"WAVE");
        assert_eq!(&header[22..24], &2_u16.to_le_bytes());
        assert_eq!(&header[24..28], &44_100_u32.to_le_bytes());
        assert_eq!(&header[40..44], &8_820_u32.to_le_bytes());
    }

    #[test]
    fn default_session_directory_uses_a_safe_configured_id() {
        let config = AppConfig {
            session_id: Some("Friday / session 1".into()),
            agents: vec![],
            campaign_context: vec![],
            model_sha256: None,
            llm: None,
        };
        assert_eq!(
            sanitize_session_id("Friday / session 1"),
            "Friday---session-1"
        );
        assert!(
            resolve_output_dir(None, &config)
                .ends_with("dnd-assistant/sessions/Friday---session-1")
        );
    }
}
