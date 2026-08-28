//! In-process microphone capture. The crate deliberately emits raw chunks;
//! resampling, VAD, and speech recognition belong to later pipeline stages.

use cpal::{
    Sample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::sync::mpsc::{self, Receiver, SyncSender};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    pub format: AudioFormat,
    pub samples: Vec<f32>,
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("no default input device is available")]
    NoInputDevice,
    #[error("could not query the device name: {0}")]
    DeviceName(#[from] cpal::DeviceNameError),
    #[error("could not query the default input config: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("could not build the input stream: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("could not start the input stream: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
}

/// Owns the platform audio stream and receives normalized interleaved f32
/// chunks from its callback. Dropping it stops capture.
pub struct AudioCapture {
    pub format: AudioFormat,
    pub chunks: Receiver<AudioChunk>,
    _stream: cpal::Stream,
}

pub fn start_default_input(queue_capacity: usize) -> Result<AudioCapture, CaptureError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(CaptureError::NoInputDevice)?;
    let config = device.default_input_config()?;
    let format = AudioFormat {
        sample_rate: config.sample_rate().0,
        channels: config.channels(),
    };
    let (sender, receiver) = mpsc::sync_channel(queue_capacity);
    let error_callback = |error| eprintln!("audio input stream error: {error}");
    let stream_config = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_stream::<f32>(&device, &stream_config, format, sender, error_callback)?
        }
        cpal::SampleFormat::I16 => {
            build_stream::<i16>(&device, &stream_config, format, sender, error_callback)?
        }
        cpal::SampleFormat::U16 => {
            build_stream::<u16>(&device, &stream_config, format, sender, error_callback)?
        }
        _ => {
            return Err(CaptureError::BuildStream(
                cpal::BuildStreamError::StreamConfigNotSupported,
            ));
        }
    };
    stream.play()?;
    Ok(AudioCapture {
        format,
        chunks: receiver,
        _stream: stream,
    })
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: AudioFormat,
    sender: SyncSender<AudioChunk>,
    error_callback: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            let samples = data
                .iter()
                .map(|sample| f32::from_sample(*sample))
                .collect();
            let _ = sender.try_send(AudioChunk { format, samples });
        },
        error_callback,
        None,
    )
}

/// Returns the default input device and its native format for diagnostics.
pub fn default_input_description() -> Result<(String, AudioFormat), CaptureError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(CaptureError::NoInputDevice)?;
    let name = device.name()?;
    let config = device.default_input_config()?;
    Ok((
        name,
        AudioFormat {
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
        },
    ))
}

pub fn downmix_and_resample(
    interleaved: &[f32],
    channels: usize,
    source_rate: u32,
    target_rate: u32,
) -> Vec<f32> {
    assert!(channels > 0);
    let mono: Vec<f32> = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    if mono.is_empty() {
        return Vec::new();
    }
    if source_rate == target_rate {
        return mono;
    }
    let output_len = (mono.len() as u64 * target_rate as u64 / source_rate as u64) as usize;
    (0..output_len)
        .map(|index| {
            let position = index as f32 * source_rate as f32 / target_rate as f32;
            let lower = position.floor() as usize;
            let upper = (lower + 1).min(mono.len().saturating_sub(1));
            let fraction = position - lower as f32;
            mono[lower.min(mono.len().saturating_sub(1))] * (1.0 - fraction)
                + mono[upper] * fraction
        })
        .collect()
}

/// Root-mean-square signal level for a normalized mono buffer.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::{downmix_and_resample, rms};

    #[test]
    fn downmixes_interleaved_stereo_without_resampling() {
        let result = downmix_and_resample(&[1.0, 0.0, 0.0, 1.0], 2, 16_000, 16_000);
        assert_eq!(result, vec![0.5, 0.5]);
    }

    #[test]
    fn resamples_to_expected_length() {
        let result = downmix_and_resample(&[0.0, 1.0], 1, 8_000, 16_000);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], 0.0);
        assert_eq!(result[2], 1.0);
    }

    #[test]
    fn rms_distinguishes_silence_from_signal() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0, 0.0]), 0.0);
        assert!((rms(&[0.5, -0.5]) - 0.5).abs() < f32::EPSILON);
    }
}
