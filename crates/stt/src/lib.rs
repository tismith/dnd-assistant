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
    initial_prompt: Option<String>,
}

impl WhisperTranscriber {
    pub fn load(model_path: impl AsRef<Path>) -> Result<Self, TranscriptionError> {
        Self::load_with_prompt(model_path, None)
    }

    /// Load Whisper with optional vocabulary/context from a local glossary.
    /// The prompt is applied to every independent live window so names and
    /// setting-specific terms remain available after the window boundary.
    pub fn load_with_prompt(
        model_path: impl AsRef<Path>,
        initial_prompt: Option<&str>,
    ) -> Result<Self, TranscriptionError> {
        let context = WhisperContext::new_with_params(
            model_path.as_ref().to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .map_err(|error| TranscriptionError::Model(error.to_string()))?;
        Ok(Self {
            context,
            next_segment_number: 0,
            initial_prompt: initial_prompt
                .map(str::trim)
                .filter(|prompt| !prompt.is_empty())
                .map(truncate_prompt),
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
        if let Some(prompt) = self.initial_prompt.as_deref() {
            params.set_initial_prompt(prompt);
        }
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

const MAX_INITIAL_PROMPT_CHARS: usize = 4_000;

fn truncate_prompt(prompt: &str) -> String {
    prompt
        .char_indices()
        .nth(MAX_INITIAL_PROMPT_CHARS)
        .map_or_else(
            || prompt.to_owned(),
            |(index, _)| prompt[..index].to_owned(),
        )
}

fn is_blank_audio_marker(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "[BLANK_AUDIO]" | "[BLANK AUDIO]" | "BLANK_AUDIO" | "BLANK AUDIO"
    )
}

#[cfg(test)]
mod tests {
    use super::{MAX_INITIAL_PROMPT_CHARS, is_blank_audio_marker, truncate_prompt};

    #[test]
    fn recognizes_whisper_blank_audio_markers() {
        assert!(is_blank_audio_marker("[BLANK_AUDIO]"));
        assert!(is_blank_audio_marker(" [blank audio] "));
        assert!(!is_blank_audio_marker("The room is quiet."));
    }

    #[test]
    fn truncates_long_initial_prompts_at_a_character_boundary() {
        let prompt = "é".repeat(MAX_INITIAL_PROMPT_CHARS + 10);
        let truncated = truncate_prompt(&prompt);
        assert_eq!(truncated.chars().count(), MAX_INITIAL_PROMPT_CHARS);
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
