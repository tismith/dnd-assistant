use dnd_assistant_core::Event;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

/// Append-only session event log. Each event is written as one JSON line and
/// synced before the call returns so a completed event is replayable after a
/// process crash or agent failure.
pub struct SessionLog {
    path: PathBuf,
}

impl SessionLog {
    pub fn open(output_dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(output_dir)?;
        Ok(Self {
            path: output_dir.join("events.jsonl"),
        })
    }

    pub fn append(&self, event: &Event) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, event).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_data()
    }

    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dnd_assistant_core::{SegmentStatus, TranscriptSegment};

    #[test]
    fn appends_complete_replayable_json_lines() {
        let dir =
            std::env::temp_dir().join(format!("dnd-assistant-session-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = SessionLog::open(&dir).unwrap();
        let segment = TranscriptSegment {
            id: "segment-1".into(),
            start_ms: 0,
            end_ms: 1_000,
            speaker_id: Some("speaker_1".into()),
            text: "We enter the temple".into(),
            confidence: Some(0.9),
            status: SegmentStatus::Finalized,
        };
        log.append(&Event::TranscriptSegmentCreated {
            segment: segment.clone(),
        })
        .unwrap();
        log.append(&Event::TranscriptSegmentFinalized {
            segment_id: segment.id,
        })
        .unwrap();
        let lines = fs::read_to_string(log.path()).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(
            lines
                .lines()
                .all(|line| serde_json::from_str::<Event>(line).is_ok())
        );
        let _ = fs::remove_file(log.path());
        let _ = fs::remove_dir(&dir);
    }
}
