use dnd_assistant_core::{
    ReconciliationInput, SegmentStatus, SpeakerSegment, TranscriptSegment, attribute_speaker,
};
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("validate") => validate(),
        Some("reconcile-demo") => reconcile_demo(),
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
    println!("Usage: cargo run -p dnd-assistant -- <validate|record [path]|reconcile-demo>");
}
