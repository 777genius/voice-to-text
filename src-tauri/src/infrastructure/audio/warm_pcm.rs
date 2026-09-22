//! Episode-local normalization. Uses the same conversion helpers, chunk sizes
//! and resampler parameters as SystemAudioCapture; there is deliberately no flush.

use super::system_capture::SystemAudioCapture;
use crate::domain::{AudioError, AudioResult};
use rubato::{Resampler, SincFixedIn};

pub(super) struct EpisodePcm {
    channels: usize,
    input_chunk: usize,
    factor: Option<usize>,
    resampler: Option<SincFixedIn<f32>>,
    buffer: Vec<i16>,
    #[cfg(test)]
    normalized_samples: usize,
}

impl EpisodePcm {
    pub fn new(rate: u32, channels: u16) -> AudioResult<Self> {
        if rate == 0 || channels == 0 {
            return Err(AudioError::Configuration(
                "Invalid warm input format".into(),
            ));
        }
        let factor = (rate > 16_000 && rate % 16_000 == 0).then_some((rate / 16_000) as usize);
        let input_chunk = factor.map_or(1024, |factor| (1024 / factor).max(1) * factor);
        let resampler = if rate != 16_000 && factor.is_none() {
            Some(SystemAudioCapture::create_resampler(rate, 16_000, 1)?)
        } else {
            None
        };
        Ok(Self {
            channels: channels as usize,
            input_chunk,
            factor,
            resampler,
            buffer: Vec::with_capacity(4096),
            #[cfg(test)]
            normalized_samples: 0,
        })
    }

    pub fn ingest(&mut self, pcm: &[i16]) -> AudioResult<()> {
        if pcm.len() % self.channels != 0 {
            return Err(AudioError::Capture(
                "Incomplete interleaved warm input frame".into(),
            ));
        }
        if self.channels == 1 {
            self.buffer.extend_from_slice(pcm);
        } else {
            self.buffer
                .extend(SystemAudioCapture::downmix_to_mono(pcm, self.channels));
        }
        Ok(())
    }

    /// Normalize one legacy-sized chunk at a time. The caller delivers it before
    /// normalizing the next one, preserving cold stop/accounting boundaries.
    pub fn next_chunk(&mut self) -> AudioResult<Option<Vec<i16>>> {
        if self.buffer.len() >= self.input_chunk {
            let chunk: Vec<_> = self.buffer.drain(..self.input_chunk).collect();
            let samples = if let Some(factor) = self.factor {
                SystemAudioCapture::downsample_integer_average(&chunk, factor)
            } else if let Some(resampler) = self.resampler.as_mut() {
                let floats: Vec<f32> = chunk
                    .iter()
                    .map(|&sample| sample as f32 / 32767.0)
                    .collect();
                let resampled = resampler.process(&[floats], None).map_err(|e| {
                    AudioError::Capture(format!("Warm input resampling failed: {e}"))
                })?;
                SystemAudioCapture::f32_to_i16(&resampled[0])
            } else {
                chunk
            };
            #[cfg(test)]
            {
                self.normalized_samples += samples.len();
            }
            return Ok(Some(samples));
        }
        Ok(None)
    }

    #[cfg(test)]
    pub fn accounting(&self) -> (usize, usize) {
        (self.normalized_samples, self.buffer.len())
    }

    #[cfg(test)]
    pub fn process(&mut self, pcm: &[i16]) -> AudioResult<Vec<Vec<i16>>> {
        self.ingest(pcm)?;
        let mut output = Vec::new();
        while let Some(samples) = self.next_chunk()? {
            output.push(samples);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_native_counts_match_legacy_chunk_boundaries_without_flush() {
        for rate in [16_000, 48_000] {
            for frames in [0, 479, 480, 481, 1022, 1023, 1024, 1025, 2048] {
                for channels in [1, 2] {
                    let mut processor = EpisodePcm::new(rate, channels).unwrap();
                    let output = processor
                        .process(&vec![1200; frames * channels as usize])
                        .unwrap();
                    // Independent constants from the current cold pipeline: 48k
                    // consumes 1023 mono frames and emits 341 normalized samples.
                    let (input_chunk, output_chunk) = if rate == 48_000 {
                        (1023, 341)
                    } else {
                        (1024, 1024)
                    };
                    assert_eq!(
                        output.iter().map(Vec::len).sum::<usize>(),
                        frames / input_chunk * output_chunk,
                        "rate={rate} channels={channels} frames={frames}"
                    );
                    assert_eq!(processor.buffer.len(), frames % input_chunk);
                    assert!(output.iter().flatten().all(|sample| *sample == 1200));
                }
            }
        }
    }

    #[test]
    fn new_episode_does_not_inherit_integer_remainder_or_sinc_history() {
        for rate in [16_000, 44_100, 48_000] {
            let mut a = EpisodePcm::new(rate, 1).unwrap();
            a.process(&vec![16_000; 2050]).unwrap();
            let mut b = EpisodePcm::new(rate, 1).unwrap();
            let output = b.process(&vec![0; 4096]).unwrap();
            assert!(
                output.iter().flatten().all(|sample| *sample == 0),
                "rate={rate}"
            );
        }
    }

    #[test]
    fn sinc_counts_and_samples_match_cold_reference() {
        for frames in [479, 1023, 1024, 1025, 2048, 4096] {
            let raw: Vec<i16> = (0..frames)
                .map(|i| if i == 0 { 16_000 } else { 0 })
                .collect();
            let mut warm = EpisodePcm::new(44_100, 1).unwrap();
            let actual: Vec<_> = warm.process(&raw).unwrap().into_iter().flatten().collect();
            let mut cold = SystemAudioCapture::create_resampler(44_100, 16_000, 1).unwrap();
            let delay = cold.output_delay();
            let mut reference = Vec::new();
            for chunk in raw.chunks_exact(1024) {
                let floats: Vec<f32> = chunk.iter().map(|&s| s as f32 / 32767.0).collect();
                reference.extend(SystemAudioCapture::f32_to_i16(
                    &cold.process(&[floats], None).unwrap()[0],
                ));
            }
            assert_eq!(
                actual, reference,
                "frames={frames} sinc_output_delay={delay}"
            );
            assert_eq!(warm.buffer.len(), frames % 1024);
        }
    }
}
