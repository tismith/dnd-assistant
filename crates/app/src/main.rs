use dnd_assistant_core::{
    AgentConfig, AgentKind, ReconciliationInput, SegmentStatus, SessionState, SpeakerSegment,
    TranscriptContext, TranscriptSegment, attribute_speaker, run_enabled_agents,
};
use std::{
    env, fs,
    io::Write,
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
        Some("reconcile-demo") => reconcile_demo(),
        Some("replay") => replay(args.next(), args.next(), args.next()),
        Some("record") => record(
            args.next()
                .map(PathBuf::from)
                .unwrap_or_else(|| "data/spike.wav".into()),
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
        "Usage: cargo run -p dnd-assistant -- <validate|record [path]|reconcile-demo|replay <config.json> <transcript.jsonl> <output-dir>>"
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
        recent.push(segment.clone());
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
            session_id: "replay-session".into(),
            current: recent.last().cloned().expect("current segment exists"),
            recent: recent_window,
            session_state: Some(SessionState::default()),
            campaign_context: campaign_context.clone(),
        };
        for (agent, result) in config
            .agents
            .iter()
            .filter(|agent| agent.enabled)
            .zip(run_enabled_agents(&config.agents, &context))
        {
            write_agent_output(Path::new(&output_dir), agent, &result);
            println!("{} -> {}", result.agent_id, agent.output);
        }
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
