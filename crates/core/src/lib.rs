//! Stable domain types and pure transformations for the live-session pipeline.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type SegmentId = String;
pub type SpeakerId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    Provisional,
    Finalized,
    Corrected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: SegmentId,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: Option<SpeakerId>,
    pub text: String,
    pub confidence: Option<f32>,
    pub status: SegmentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Speaker {
    pub id: SpeakerId,
    pub display_name: Option<String>,
    pub character_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: SpeakerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub current_location: Option<String>,
    pub active_npcs: Vec<String>,
    pub active_threads: Vec<String>,
    pub recent_discoveries: Vec<String>,
    pub recent_player_intentions: Vec<String>,
    pub open_questions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    AudioChunkReady { timestamp_ms: u64, duration_ms: u64 },
    SpeechSegmentDetected { segment: TranscriptSegment },
    TranscriptSegmentCreated { segment: TranscriptSegment },
    TranscriptSegmentUpdated { segment: TranscriptSegment },
    SpeakerSegmentDetected { segment: SpeakerSegment },
    SpeakerMappingChanged { speaker: Speaker },
    TranscriptSegmentFinalized { segment_id: SegmentId },
    SessionStateUpdated { state: SessionState },
    AgentRunRequested { timestamp_ms: u64 },
    AgentSuggestionCreated { timestamp_ms: u64, text: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReconciliationInput {
    pub transcript: TranscriptSegment,
    pub speaker_segments: Vec<SpeakerSegment>,
}

/// Assigns the speaker with the greatest timestamp overlap.
///
/// This deliberately has no special handling for overlapping speech yet. It is
/// deterministic, testable, and leaves room for a richer reconciler later.
pub fn attribute_speaker(input: ReconciliationInput) -> TranscriptSegment {
    let mut overlaps: BTreeMap<&str, u64> = BTreeMap::new();
    for candidate in &input.speaker_segments {
        let start = input.transcript.start_ms.max(candidate.start_ms);
        let end = input.transcript.end_ms.min(candidate.end_ms);
        if start < end {
            *overlaps.entry(candidate.speaker_id.as_str()).or_default() += end - start;
        }
    }

    let speaker_id = overlaps
        .into_iter()
        .max_by_key(|(_, overlap_ms)| *overlap_ms)
        .map(|(speaker_id, _)| speaker_id.to_owned());

    TranscriptSegment {
        speaker_id,
        ..input.transcript
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(start_ms: u64, end_ms: u64) -> TranscriptSegment {
        TranscriptSegment {
            id: "segment-1".into(),
            start_ms,
            end_ms,
            speaker_id: None,
            text: "I search the altar for traps".into(),
            confidence: Some(0.9),
            status: SegmentStatus::Provisional,
        }
    }

    #[test]
    fn chooses_speaker_with_greatest_overlap() {
        let result = attribute_speaker(ReconciliationInput {
            transcript: transcript(10_200, 14_800),
            speaker_segments: vec![
                SpeakerSegment {
                    start_ms: 9_900,
                    end_ms: 11_000,
                    speaker_id: "speaker_1".into(),
                },
                SpeakerSegment {
                    start_ms: 10_900,
                    end_ms: 15_100,
                    speaker_id: "speaker_2".into(),
                },
            ],
        });
        assert_eq!(result.speaker_id.as_deref(), Some("speaker_2"));
    }

    #[test]
    fn leaves_speaker_unknown_without_overlap() {
        let result = attribute_speaker(ReconciliationInput {
            transcript: transcript(100, 200),
            speaker_segments: vec![SpeakerSegment {
                start_ms: 300,
                end_ms: 400,
                speaker_id: "speaker_1".into(),
            }],
        });
        assert_eq!(result.speaker_id, None);
    }
}
