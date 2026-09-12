//! Embedded Whisper transcription. This crate owns model execution but not
//! microphone capture, scheduling, persistence, or agent fan-out.

use dnd_assistant_core::{SegmentStatus, TranscriptSegment};
use std::path::Path;
use thiserror::Error;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error("could not load Whisper model: {0}")]
    Model(String),
    #[error("Whisper inference failed: {0}")]
    Inference(String),
}

pub struct WhisperTranscriber {
    context: WhisperContext,
    next_segment_number: u64,
}

impl WhisperTranscriber {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self, TranscriptionError> {
        let context = WhisperContext::new_with_params(
            model_path.as_ref().to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .map_err(|error| TranscriptionError::Model(error.to_string()))?;
        Ok(Self {
            context,
            next_segment_number: 0,
        })
    }

    /// Transcribe one mono 16 kHz window. Timestamps are relative to this
    /// window; the session scheduler adds the window's absolute start time.
    pub fn transcribe_window(
        &mut self,
        audio: &[f32],
    ) -> Result<Vec<TranscriptSegment>, TranscriptionError> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // Room microphones often produce Whisper's textual blank-audio marker
        // on quiet/noisy windows. Non-speech suppression keeps that marker and
        // similar noise tokens out of the application transcript.
        params.set_suppress_nst(true);
        let mut state = self
            .context
            .create_state()
            .map_err(|error| TranscriptionError::Inference(error.to_string()))?;
        state
            .full(params, audio)
            .map_err(|error| TranscriptionError::Inference(error.to_string()))?;

        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let start_ms = segment.start_timestamp() as u64 * 10;
            let end_ms = segment.end_timestamp() as u64 * 10;
            let text = segment.to_string().trim().to_owned();
            if text.is_empty() || is_blank_audio_marker(&text) || end_ms <= start_ms {
                continue;
            }
            self.next_segment_number += 1;
            segments.push(TranscriptSegment {
                id: format!("stt-{}", self.next_segment_number),
                start_ms,
                end_ms,
                speaker_id: None,
                text,
                confidence: None,
                status: SegmentStatus::Provisional,
            });
        }
        Ok(segments)
    }
}

fn is_blank_audio_marker(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "[BLANK_AUDIO]" | "[BLANK AUDIO]" | "BLANK_AUDIO" | "BLANK AUDIO"
    )
}

#[cfg(test)]
mod tests {
    use super::is_blank_audio_marker;

    #[test]
    fn recognizes_whisper_blank_audio_markers() {
        assert!(is_blank_audio_marker("[BLANK_AUDIO]"));
        assert!(is_blank_audio_marker(" [blank audio] "));
        assert!(!is_blank_audio_marker("The room is quiet."));
    }
}
