use dnd_assistant_audio::{default_input_description, downmix_and_resample, start_default_input};
use dnd_assistant_core::{
    AgentConfig, AgentKind, ReconciliationInput, SegmentStatus, SessionState, SpeakerSegment,
    TranscriptContext, TranscriptSegment, attribute_speaker, run_enabled_agents,
};
use dnd_assistant_models::{default_model_cache_dir, ensure_model};
use dnd_assistant_stt::WhisperTranscriber;
use std::{
    env, fs,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, serde::Deserialize)]
struct AppConfig {
    agents: Vec<AgentConfig>,
    #[serde(default)]
    campaign_context: Vec<String>,
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
    let (Some(model_path), Some(config_path), Some(output_dir)) =
        (model_path, config_path, output_dir)
    else {
        usage();
        std::process::exit(2);
    };
    let config: AppConfig = read_json(&config_path);
    let campaign_context = load_campaign_context(&config);
    let capture = start_default_input(8).unwrap_or_else(|error| {
        eprintln!("audio unavailable: {error}");
        std::process::exit(1);
    });
    let model_path = if model_path.starts_with("http://") || model_path.starts_with("https://") {
        let filename = model_path
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("model.bin");
        let destination = default_model_cache_dir().join(filename);
        ensure_model(&model_path, &destination, None).unwrap_or_else(|error| {
            eprintln!("model download unavailable: {error}");
            std::process::exit(1);
        })
    } else {
        PathBuf::from(&model_path)
    };
    let mut transcriber = WhisperTranscriber::load(&model_path).unwrap_or_else(|error| {
        eprintln!("transcription unavailable: {error}");
        std::process::exit(1);
    });
    fs::create_dir_all(&output_dir)
        .unwrap_or_else(|error| panic!("cannot create {output_dir}: {error}"));
    let channels = capture.format.channels as usize;
    let sample_rate = capture.format.sample_rate;
    let window_input_samples = sample_rate as usize * channels * 5;
    let mut input_samples = Vec::with_capacity(window_input_samples);
    let mut recent = Vec::new();
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
            let segments = transcriber
                .transcribe_window(&audio)
                .unwrap_or_else(|error| panic!("transcription failed: {error}"));
            for mut segment in segments {
                segment.start_ms += window_start_ms;
                segment.end_ms += window_start_ms;
                segment.status = SegmentStatus::Finalized;
                process_segment(
                    &config,
                    &campaign_context,
                    &mut recent,
                    Path::new(&output_dir),
                    segment,
                );
            }
            window_start_ms += 5_000;
        }
    }
}

fn default_data_dir() -> PathBuf {
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("dnd-assistant").join("sessions")
}

fn record(path: PathBuf) {
    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("cannot create {}: {error}", parent.display());
            std::process::exit(1);
        }
    }
    println!(
        "Recording 10 seconds to {} (Ctrl-C to stop early)...",
        path.display()
    );
    let status = Command::new("arecord")
        .args([
            "-q", "-D", "default", "-f", "S16_LE", "-r", "16000", "-c", "1", "-d", "10",
        ])
        .arg(&path)
        .status();
    match status {
        Ok(status) if status.success() => println!("Audio capture complete: {}", path.display()),
        Ok(status) => {
            eprintln!("arecord exited with {status}");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("could not start arecord: {error}");
            std::process::exit(1);
        }
    }
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
        "Usage: cargo run -p dnd-assistant -- <validate|audio-info|capture|live <model> <config.json> <output-dir>|record [path]|reconcile-demo|replay <config.json> <transcript.jsonl> <output-dir>|stream <config.json> <output-dir>>"
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
    let campaign_context = config
        .campaign_context
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("cannot read campaign context {path}: {error}"))
        })
        .collect::<Vec<_>>();
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
    let mut recent = Vec::new();
    for segment in segments {
        process_segment(
            &config,
            &campaign_context,
            &mut recent,
            Path::new(&output_dir),
            segment,
        );
    }
}

fn stream(config_path: Option<String>, output_dir: Option<String>) {
    let (Some(config_path), Some(output_dir)) = (config_path, output_dir) else {
        usage();
        std::process::exit(2);
    };
    let config: AppConfig = read_json(&config_path);
    let campaign_context = load_campaign_context(&config);
    fs::create_dir_all(&output_dir)
        .unwrap_or_else(|error| panic!("cannot create {output_dir}: {error}"));
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
            Path::new(&output_dir),
            segment,
        );
    }
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
) {
    recent.push(segment);
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
        session_id: "live-session".into(),
        current: recent.last().cloned().expect("current segment exists"),
        recent: recent_window,
        session_state: Some(SessionState::default()),
        campaign_context: campaign_context.to_vec(),
    };
    for (agent, result) in config
        .agents
        .iter()
        .filter(|agent| agent.enabled)
        .zip(run_enabled_agents(&config.agents, &context))
    {
        write_agent_output(output_dir, agent, &result);
        println!("{} -> {}", result.agent_id, agent.output);
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
) {
    let path = output_dir.join(&config.output);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("agent output directory creates");
    }
    match &config.kind {
        AgentKind::Recorder => {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("recorder output opens");
            writeln!(file, "{}", result.body).expect("recorder output writes");
        }
        AgentKind::LiveSummary | AgentKind::NextSteps => {
            fs::write(path, format!("{}\n\n{}", result.title, result.body))
                .expect("agent output writes");
        }
    }
}
