use dnd_assistant_audio::{
    default_input_description, downmix_and_resample, rms, start_default_input,
};
use dnd_assistant_core::{
    AgentConfig, AgentKind, CampaignUpdatePlan, Event, ReconciliationInput, SegmentStatus,
    SessionState, SpeakerSegment, TranscriptContext, TranscriptSegment, WorkspaceDocument,
    attribute_speaker,
};
use dnd_assistant_models::{default_model_cache_dir, ensure_model};
use dnd_assistant_stt::WhisperTranscriber;
mod agent_runtime;
mod llm;
mod session;
mod ui;
mod workspace;
use session::SessionLog;
use std::{
    collections::HashSet,
    env, fs,
    io::{BufRead, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin";
const DEFAULT_MODEL_FILENAME: &str = "ggml-tiny.en.bin";

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
        Some("transcribe-wav") => {
            transcribe_wav(args.next(), args.next(), args.next(), args.next())
        }
        Some("session-end") => session_end(
            args.next(),
            args.next(),
            args.next().as_deref() == Some("--apply"),
        ),
        Some("record") => record(
            args.next()
                .map(PathBuf::from)
                .unwrap_or_else(default_recording_path),
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
        if chunks.is_multiple_of(20) {
            println!("captured {chunks} chunks / {samples} samples");
        }
    }
}

fn live(model_path: Option<String>, config_path: Option<String>, output_dir: Option<String>) {
    let mut config = load_app_config(config_path.as_deref());
    let campaign_context = load_campaign_context(&config);
    let output_dir = resolve_output_dir(output_dir, &config);
    set_default_session_id(&mut config, &output_dir);
    let model_path = resolve_model_path(model_path, &config);
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
    let ui_available = match ui::start(ui_state.clone(), ui_address) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("live UI unavailable; continuing without it: {error}");
            false
        }
    };
    let auto_start = env::var("DND_ASSISTANT_AUTO_START")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(!ui_available);
    if auto_start {
        ui::start_session(&ui_state);
    } else {
        ui::set_status(&ui_state, "ready");
    }
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
            let level = rms(&audio);
            eprintln!(
                "audio window {}-{} ms: RMS {:.5}",
                window_start_ms,
                window_start_ms + 5_000,
                level
            );
            if level < silence_rms {
                eprintln!("audio window below silence threshold ({silence_rms:.5}); skipped");
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
    loop {
        let (_, _, stop_requested) = ui::control_snapshot(&ui_state);
        if stop_requested {
            break;
        }
        let chunk = match capture.chunks.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => chunk,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let (active, paused, stop_requested) = ui::control_snapshot(&ui_state);
        if stop_requested {
            break;
        }
        if !active || paused {
            input_samples.clear();
            continue;
        }
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
    ui::set_status(&ui_state, "stopped");
}

fn default_data_dir() -> PathBuf {
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("dnd-assistant").join("sessions")
}

fn default_recording_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    default_data_dir().join(format!("recording-{millis}.wav"))
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
    if config.session_id.is_none()
        && let Some(name) = output_dir.file_name().and_then(|name| name.to_str())
    {
        config.session_id = Some(name.to_owned());
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
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        eprintln!("cannot create {}: {error}", parent.display());
        std::process::exit(1);
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

fn read_pcm16_wav(path: &Path) -> Result<(u32, u16, Vec<f32>), String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("WAV must have RIFF/WAVE headers".into());
    }
    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[offset + 4..offset + 8]
                .try_into()
                .expect("chunk size has four bytes"),
        ) as usize;
        offset += 8;
        let end = offset
            .checked_add(chunk_size)
            .ok_or_else(|| "WAV chunk size overflows the file".to_owned())?;
        if end > bytes.len() {
            return Err("WAV chunk extends past the end of the file".into());
        }
        match chunk_id {
            b"fmt " if chunk_size >= 16 => {
                let chunk = &bytes[offset..end];
                format = Some((
                    u16::from_le_bytes(chunk[0..2].try_into().unwrap()),
                    u16::from_le_bytes(chunk[2..4].try_into().unwrap()),
                    u32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                    u16::from_le_bytes(chunk[14..16].try_into().unwrap()),
                ));
            }
            b"data" => data = Some(&bytes[offset..end]),
            _ => {}
        }
        offset = end + (chunk_size & 1);
    }
    let (audio_format, channels, sample_rate, bits_per_sample) =
        format.ok_or_else(|| "WAV has no PCM format chunk".to_owned())?;
    if audio_format != 1 || channels == 0 || bits_per_sample != 16 {
        return Err("WAV must contain signed 16-bit PCM audio".into());
    }
    let data = data.ok_or_else(|| "WAV has no data chunk".to_owned())?;
    if data.len() % 2 != 0 {
        return Err("WAV PCM data has an incomplete sample".into());
    }
    let samples = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| i16::from_le_bytes(*sample) as f32 / i16::MAX as f32)
        .collect();
    Ok((sample_rate, channels, samples))
}

fn transcribe_wav(
    model_path: Option<String>,
    wav_path: Option<String>,
    config_path: Option<String>,
    output_dir: Option<String>,
) {
    let (Some(model_path), Some(wav_path), Some(config_path)) = (model_path, wav_path, config_path)
    else {
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
        ensure_model(&model_path, &destination, config.model_sha256.as_deref())
            .unwrap_or_else(|error| panic!("model download unavailable: {error}"))
    } else {
        PathBuf::from(model_path)
    };
    let mut transcriber = WhisperTranscriber::load(&model_path)
        .unwrap_or_else(|error| panic!("transcription unavailable: {error}"));
    let (sample_rate, channels, samples) = read_pcm16_wav(Path::new(&wav_path))
        .unwrap_or_else(|error| panic!("cannot parse {wav_path}: {error}"));
    let audio = downmix_and_resample(&samples, channels as usize, sample_rate, 16_000);
    fs::create_dir_all(&output_dir)
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", output_dir.display()));
    let session_log = SessionLog::open(&output_dir)
        .unwrap_or_else(|error| panic!("cannot open session event log: {error}"));
    let agent_dispatcher = agent_runtime::AgentDispatcher::start(None);
    let mut recent = Vec::new();
    let silence_rms = env::var("DND_ASSISTANT_SILENCE_RMS")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.005);
    println!(
        "Transcribing {} ({} Hz / {} channels)...",
        wav_path, sample_rate, channels
    );
    for (window_index, window) in audio.chunks(16_000 * 5).enumerate() {
        if rms(window) < silence_rms {
            continue;
        }
        match transcriber.transcribe_window(window) {
            Ok(segments) => {
                for mut segment in segments {
                    let window_start_ms = window_index as u64 * 5_000;
                    segment.start_ms += window_start_ms;
                    segment.end_ms += window_start_ms;
                    segment.status = SegmentStatus::Finalized;
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
            }
            Err(error) => eprintln!("transcription window failed; continuing: {error}"),
        }
    }
    agent_dispatcher.finish();
    println!("Transcription complete: {}", output_dir.display());
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
        "Usage: cargo run -p dnd-assistant -- <validate|audio-info|capture|live [model] [config.json] [output-dir]|record [path]|transcribe-wav <model> <recording.wav> <config.json> [output-dir]|session-end <config.json> <session-dir> [--apply]|reconcile-demo|replay <config.json> <transcript.jsonl> <output-dir>|stream <config.json> [output-dir]>"
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

#[allow(clippy::too_many_arguments)]
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
    if segment.status == SegmentStatus::Finalized
        && let Err(error) = session_log.append(&Event::TranscriptSegmentFinalized {
            segment_id: segment.id.clone(),
        })
    {
        eprintln!("session event log append failed; continuing agents: {error}");
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
        workspace_context: vec![],
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

fn default_config_path() -> PathBuf {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("dnd-assistant").join("agents.json")
}

fn default_app_config() -> AppConfig {
    AppConfig {
        session_id: None,
        agents: vec![
            AgentConfig {
                id: "recorder".into(),
                kind: AgentKind::Recorder,
                enabled: true,
                output: "transcript.jsonl".into(),
                instruction: None,
                prompt_file: None,
                workspace_paths: vec![],
                workspace_query: None,
                write_paths: vec![],
                include_campaign_context: true,
                run_every_segments: 1,
            },
            AgentConfig {
                id: "summary".into(),
                kind: AgentKind::LiveSummary,
                enabled: true,
                output: "summary.md".into(),
                instruction: Some("Keep unresolved player questions visible.".into()),
                prompt_file: None,
                workspace_paths: vec![],
                workspace_query: None,
                write_paths: vec![],
                include_campaign_context: true,
                run_every_segments: 1,
            },
            AgentConfig {
                id: "gm-next-steps".into(),
                kind: AgentKind::NextSteps,
                enabled: true,
                output: "gm-next-steps.md".into(),
                instruction: Some("Offer choices without prescribing a single action.".into()),
                prompt_file: None,
                workspace_paths: vec![],
                workspace_query: None,
                write_paths: vec![],
                include_campaign_context: true,
                run_every_segments: 3,
            },
            AgentConfig {
                id: "gm-copilot".into(),
                kind: AgentKind::Llm,
                enabled: true,
                output: "gm-copilot.md".into(),
                instruction: None,
                prompt_file: Some("prompts/gm-copilot.md".into()),
                workspace_paths: vec![],
                workspace_query: Some(
                    "campaign canon open threads NPC lore locations quests factions".into(),
                ),
                write_paths: vec![],
                include_campaign_context: false,
                run_every_segments: 6,
            },
        ],
        campaign_context: vec![],
        model_sha256: None,
        llm: Some(llm::LlmConfig {
            endpoint: "codex://local".into(),
            model: "default".into(),
            api_key_env: None,
        }),
    }
}

fn load_app_config(config_path: Option<&str>) -> AppConfig {
    if let Some(path) = config_path {
        println!("Using agent config: {path}");
        return read_json(path);
    }
    let xdg_path = default_config_path();
    for path in [xdg_path, PathBuf::from("agents.json")] {
        if path.is_file() {
            println!("Using agent config: {}", path.display());
            return read_json(&path.display().to_string());
        }
    }
    println!(
        "Using built-in agent defaults (customize {})",
        default_config_path().display()
    );
    default_app_config()
}

fn resolve_model_path(model_path: Option<String>, config: &AppConfig) -> PathBuf {
    let model = model_path.unwrap_or_else(|| {
        println!("Using default Whisper model: {DEFAULT_MODEL_FILENAME}");
        DEFAULT_MODEL_URL.into()
    });
    if model.starts_with("http://") || model.starts_with("https://") {
        let filename = model
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(DEFAULT_MODEL_FILENAME);
        let destination = default_model_cache_dir().join(filename);
        ensure_model(&model, &destination, config.model_sha256.as_deref()).unwrap_or_else(|error| {
            eprintln!("model download unavailable: {error}");
            std::process::exit(1);
        })
    } else {
        PathBuf::from(model)
    }
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
        AgentKind::LiveSummary
        | AgentKind::NextSteps
        | AgentKind::Llm
        | AgentKind::SessionEditor => {
            fs::write(path, format!("{}\n\n{}", result.title, result.body))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn session_end(config_path: Option<String>, session_dir: Option<String>, apply: bool) {
    let (Some(config_path), Some(session_dir)) = (config_path, session_dir) else {
        usage();
        std::process::exit(2);
    };
    let config: AppConfig = read_json(&config_path);
    let editor = config
        .agents
        .iter()
        .find(|agent| agent.enabled && agent.kind == AgentKind::SessionEditor)
        .unwrap_or_else(|| panic!("no enabled session_editor agent is configured"));
    let provider = config
        .llm
        .as_ref()
        .unwrap_or_else(|| panic!("session_editor requires an llm provider"));
    let session_path = PathBuf::from(&session_dir);
    let events = fs::read_to_string(session_path.join("events.jsonl"))
        .unwrap_or_else(|error| panic!("cannot read session events: {error}"));
    let mut segments = Vec::new();
    for line in events.lines().filter(|line| !line.trim().is_empty()) {
        let event: Event = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("invalid session event: {error}"));
        if let Event::TranscriptSegmentCreated { segment } = event {
            segments.push(segment);
        }
    }
    let current = segments
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("session contains no transcript segments"));
    let mut workspace_context = workspace::Workspace::load(&editor.workspace_paths).all();
    if let Ok(entries) = fs::read_dir(&session_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.file_name().and_then(|name| name.to_str()) != Some("events.jsonl")
                && let Ok(content) = fs::read_to_string(&path)
            {
                workspace_context.push(WorkspaceDocument {
                    path: path.display().to_string(),
                    content,
                });
            }
        }
    }
    let context = TranscriptContext {
        session_id: config
            .session_id
            .clone()
            .unwrap_or_else(|| session_dir.clone()),
        current,
        recent: segments,
        session_state: Some(SessionState::default()),
        campaign_context: load_campaign_context(&config),
        workspace_context,
    };
    let plan = llm::run_session_editor(provider, editor, &context)
        .unwrap_or_else(|error| panic!("session editor failed: {error}"));
    let plan_path = session_path.join("campaign-update-plan.json");
    fs::write(&plan_path, serde_json::to_string_pretty(&plan).unwrap())
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", plan_path.display()));
    let summary_path = session_path.join("campaign-update-summary.md");
    fs::write(&summary_path, &plan.summary)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", summary_path.display()));
    println!("Campaign update summary: {}", summary_path.display());
    println!("Campaign update plan: {}", plan_path.display());
    if apply {
        let write_paths = default_workspace_paths(&editor.write_paths);
        apply_campaign_updates(&plan, &write_paths, &session_path);
        println!("Campaign updates applied.");
    } else {
        println!("Review the plan, then rerun with --apply to update configured campaign paths.");
    }
}

fn default_workspace_paths(configured: &[String]) -> Vec<String> {
    if configured.is_empty() {
        vec![
            env::current_dir()
                .unwrap_or_else(|error| panic!("cannot determine workspace directory: {error}"))
                .display()
                .to_string(),
        ]
    } else {
        configured.to_vec()
    }
}

fn apply_campaign_updates(plan: &CampaignUpdatePlan, write_paths: &[String], session_dir: &Path) {
    if write_paths.is_empty() && !plan.updates.is_empty() {
        panic!("session_editor has updates but no configured write_paths");
    }
    let mut changes = Vec::new();
    let mut changed_paths = HashSet::new();
    for update in &plan.updates {
        let path = resolve_allowed_update_path(&update.path, write_paths)
            .unwrap_or_else(|| panic!("update path is outside write_paths: {}", update.path));
        if !changed_paths.insert(path.clone()) {
            panic!(
                "session update plan contains duplicate path: {}",
                path.display()
            );
        }
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let replacement = match &update.find {
            Some(find) => {
                if find.is_empty() {
                    panic!("update for {} has an empty find block", path.display());
                }
                let matches = existing.matches(find).count();
                if matches != 1 {
                    panic!(
                        "update for {} expected one matching block, found {matches}",
                        path.display()
                    );
                }
                existing.replacen(find, &update.replace, 1)
            }
            None if path.exists() => format!("{}\n{}\n", existing.trim_end(), update.replace),
            None => update.replace.clone(),
        };
        changes.push((path, existing, replacement));
    }
    for (index, (path, existing, replacement)) in changes.into_iter().enumerate() {
        if path.exists() {
            let backup = session_dir.join("campaign-backups").join(format!(
                "{index}-{}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file")
            ));
            fs::create_dir_all(backup.parent().unwrap()).unwrap();
            fs::write(&backup, existing).unwrap();
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        write_atomic(&path, replacement.as_bytes(), index)
            .unwrap_or_else(|error| panic!("cannot apply update to {}: {error}", path.display()));
    }
}

fn write_atomic(path: &Path, contents: &[u8], sequence: usize) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temporary = path.with_file_name(format!(".{file_name}.dnd-assistant-{sequence}.tmp"));
    let mut file = fs::File::create(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(temporary, path)
}

fn resolve_allowed_update_path(update: &str, roots: &[String]) -> Option<PathBuf> {
    let update_path = Path::new(update);
    if update_path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return None;
    }
    roots.iter().find_map(|root| {
        let root = fs::canonicalize(root).ok()?;
        let candidate = if update_path.is_absolute() {
            update_path.to_owned()
        } else {
            root.join(update_path)
        };
        if candidate.exists() && !candidate.is_file() {
            return None;
        }
        let canonical_candidate = if candidate.exists() {
            fs::canonicalize(&candidate).ok()?
        } else {
            let mut parent = candidate.parent()?;
            let mut missing = Vec::new();
            while !parent.exists() {
                missing.push(parent.file_name()?.to_owned());
                parent = parent.parent()?;
            }
            let mut canonical_parent = fs::canonicalize(parent).ok()?;
            for component in missing.iter().rev() {
                canonical_parent.push(component);
            }
            canonical_parent.join(candidate.file_name()?)
        };
        canonical_candidate.starts_with(&root).then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfig, DEFAULT_MODEL_FILENAME, apply_campaign_updates, default_app_config,
        read_pcm16_wav, resolve_output_dir, sanitize_session_id, wav_header, write_agent_output,
    };
    use dnd_assistant_core::{
        AgentConfig, AgentKind, AgentOutput, CampaignFileUpdate, CampaignUpdatePlan,
    };
    use std::path::Path;

    #[test]
    fn agent_output_cannot_escape_session_directory() {
        let config = AgentConfig {
            id: "bad".into(),
            kind: AgentKind::Recorder,
            enabled: true,
            output: "../outside.jsonl".into(),
            instruction: None,
            prompt_file: None,
            workspace_paths: vec![],
            workspace_query: None,
            write_paths: vec![],
            include_campaign_context: true,
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
            prompt_file: None,
            workspace_paths: vec![],
            workspace_query: None,
            write_paths: vec![],
            include_campaign_context: true,
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
    fn pcm_wav_reader_handles_native_recording() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("dnd-assistant-wav-{nonce}.wav"));
        let mut bytes = wav_header(4, 8_000, 1).to_vec();
        bytes.extend_from_slice(&i16::MIN.to_le_bytes());
        bytes.extend_from_slice(&i16::MAX.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let (sample_rate, channels, samples) = read_pcm16_wav(&path).unwrap();
        assert_eq!((sample_rate, channels), (8_000, 1));
        assert_eq!(samples.len(), 2);
        assert!(samples[0] < -0.99 && samples[1] > 0.99);
        let _ = std::fs::remove_file(path);
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

    #[test]
    fn built_in_defaults_are_local_and_workspace_relative() {
        let config = default_app_config();
        assert_eq!(DEFAULT_MODEL_FILENAME, "ggml-tiny.en.bin");
        assert_eq!(config.llm.as_ref().unwrap().endpoint, "codex://local");
        assert_eq!(config.agents.len(), 4);
        assert!(
            config
                .agents
                .iter()
                .all(|agent| agent.workspace_paths.is_empty())
        );
        assert!(
            config
                .agents
                .iter()
                .all(|agent| agent.write_paths.is_empty())
        );
    }

    #[test]
    fn campaign_update_requires_exact_matching_text() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("dnd-campaign-update-{nonce}"));
        let campaign = root.join("campaign");
        let session = root.join("session");
        std::fs::create_dir_all(&campaign).unwrap();
        std::fs::create_dir_all(&session).unwrap();
        let target = campaign.join("canon.md");
        std::fs::write(&target, "The party found the bell.").unwrap();
        let plan = CampaignUpdatePlan {
            summary: "The bell was found.".into(),
            updates: vec![CampaignFileUpdate {
                path: target.display().to_string(),
                reason: "Established in the transcript.".into(),
                evidence: vec!["stt-1".into()],
                find: Some("The party found the bell.".into()),
                replace: "The party found the Wind Bell.".into(),
            }],
        };
        apply_campaign_updates(&plan, &[campaign.display().to_string()], &session);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "The party found the Wind Bell."
        );
        assert!(session.join("campaign-backups/0-canon.md").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
