use std::time::Duration;
use webrtc_vad::{Vad, VadMode, SampleRate};

use crate::domain::{SttError, SttResult};

/// Voice Activity Detection processor using WebRTC VAD
///
/// Requirements:
/// - Fixed 30ms frames (480 samples @ 16kHz)
/// - Configurable silence timeout for auto-stop (default: 3000ms from AppConfig)
/// - Sample rate: 16kHz mono PCM i16
const FRAME_SIZE_MS: usize = 30;
const FRAME_SIZE_SAMPLES: usize = 480; // 16kHz * 30ms / 1000
const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 3000; // По умолчанию 3 секунды

/// Result of VAD processing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadResult {
    /// Speech detected in current frame
    Speech,
    /// Silence detected, but below timeout threshold
    Silence,
    /// Silence timeout reached - should stop recording
    SilenceTimeout,
    /// Still buffering samples (not enough for full frame yet)
    Buffering,
}

/// VAD processor with fixed-size frame buffering
pub struct VadProcessor {
    /// WebRTC VAD instance
    vad: Vad,
    /// Buffer for accumulating samples until we have a full frame
    buffer: Vec<i16>,
    /// Accumulated silence duration
    silence_duration: Duration,
    /// Timeout threshold for stopping
    timeout: Duration,
}

impl VadProcessor {
    /// Create new VAD processor with specified silence timeout
    ///
    /// # Arguments
    /// * `timeout_ms` - Silence timeout in milliseconds (default: 3000ms)
    /// * `mode` - VAD sensitivity mode (default: Quality)
    ///
    /// # Returns
    /// New VadProcessor instance configured for 16kHz audio
    pub fn new(timeout_ms: Option<u64>, mode: Option<VadMode>) -> SttResult<Self> {
        let mut vad = Vad::new();
        vad.set_mode(mode.unwrap_or(VadMode::Quality));
        vad.set_sample_rate(SampleRate::Rate16kHz);

        Ok(Self {
            vad,
            buffer: Vec::with_capacity(FRAME_SIZE_SAMPLES * 2), // Pre-allocate for efficiency
            silence_duration: Duration::from_millis(0),
            timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_SILENCE_TIMEOUT_MS)),
        })
    }

    /// Create VAD processor with default settings (3000ms timeout, Quality mode)
    pub fn default() -> SttResult<Self> {
        Self::new(None, None)
    }

    /// Process incoming audio samples
    ///
    /// Accumulates samples in internal buffer until we have exactly 480 samples (30ms @ 16kHz),
    /// then runs VAD detection.
    ///
    /// # Arguments
    /// * `samples` - PCM i16 samples @ 16kHz mono
    ///
    /// # Returns
    /// * `VadResult::Buffering` - Not enough samples yet
    /// * `VadResult::Speech` - Speech detected, silence counter reset
    /// * `VadResult::Silence` - Silence detected, counter incremented
    /// * `VadResult::SilenceTimeout` - Silence threshold exceeded
    pub fn process_samples(&mut self, samples: &[i16]) -> SttResult<VadResult> {
        // Add incoming samples to buffer
        self.buffer.extend_from_slice(samples);

        // If we don't have a full frame yet, keep buffering
        if self.buffer.len() < FRAME_SIZE_SAMPLES {
            return Ok(VadResult::Buffering);
        }

        // Extract exactly one frame (480 samples)
        let frame: Vec<i16> = self.buffer.drain(..FRAME_SIZE_SAMPLES).collect();

        // Run VAD detection
        let is_speech = self.vad.is_voice_segment(&frame)
            .map_err(|_| SttError::Processing("VAD error".to_string()))?;

        if is_speech {
            // Speech detected - reset silence counter
            self.silence_duration = Duration::from_millis(0);
            Ok(VadResult::Speech)
        } else {
            // Silence detected - increment counter
            self.silence_duration += Duration::from_millis(FRAME_SIZE_MS as u64);

            if self.silence_duration >= self.timeout {
                Ok(VadResult::SilenceTimeout)
            } else {
                Ok(VadResult::Silence)
            }
        }
    }

    /// Reset silence counter (useful when manually restarting recording)
    pub fn reset(&mut self) {
        self.silence_duration = Duration::from_millis(0);
        self.buffer.clear();
    }

    /// Get current silence duration
    pub fn silence_duration(&self) -> Duration {
        self.silence_duration
    }

    /// Get configured timeout threshold
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Check if buffer has pending samples
    pub fn has_buffered_samples(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Get number of buffered samples
    pub fn buffered_samples(&self) -> usize {
        self.buffer.len()
    }
}

// SAFETY: webrtc_vad::Vad internally uses a raw pointer but we ensure
// it's only accessed from one thread at a time through Mutex
unsafe impl Send for VadProcessor {}
unsafe impl Sync for VadProcessor {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vad_creation() {
        let vad = VadProcessor::default();
        assert!(vad.is_ok());
    }

    #[test]
    fn test_buffering() {
        let mut vad = VadProcessor::default().unwrap();

        // Send less than 480 samples
        let samples = vec![0i16; 240];
        let result = vad.process_samples(&samples).unwrap();

        assert_eq!(result, VadResult::Buffering);
        assert_eq!(vad.buffered_samples(), 240);
    }

    #[test]
    fn test_full_frame_processing() {
        let mut vad = VadProcessor::default().unwrap();

        // Send exactly 480 samples (silence)
        let samples = vec![0i16; 480];
        let result = vad.process_samples(&samples).unwrap();

        // Should process the frame (either Speech or Silence, not Buffering)
        assert_ne!(result, VadResult::Buffering);
    }

    #[test]
    fn test_silence_timeout() {
        let mut vad = VadProcessor::new(Some(90), None).unwrap(); // 90ms timeout (3 frames)

        let silence_frame = vec![0i16; 480];

        // First frame - silence but no timeout
        let result1 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(result1, VadResult::Silence);

        // Second frame - still silence
        let result2 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(result2, VadResult::Silence);

        // Third frame - should hit timeout
        let result3 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(result3, VadResult::SilenceTimeout);
    }

    #[test]
    fn test_reset() {
        let mut vad = VadProcessor::default().unwrap();

        let samples = vec![0i16; 240];
        let _ = vad.process_samples(&samples);
        assert_eq!(vad.buffered_samples(), 240);

        vad.reset();
        assert_eq!(vad.buffered_samples(), 0);
        assert_eq!(vad.silence_duration(), Duration::from_millis(0));
    }

    #[test]
    fn test_buffer_overflow() {
        let mut vad = VadProcessor::default().unwrap();

        // Отправляем больше чем один фрейм за раз (например, 1000 samples)
        let large_chunk = vec![0i16; 1000];
        let result = vad.process_samples(&large_chunk).unwrap();

        // Должен обработать первые 480 samples, остальные остаются в буфере
        assert_ne!(result, VadResult::Buffering);
        assert_eq!(vad.buffered_samples(), 1000 - 480); // 520 samples в буфере
    }

    #[test]
    fn test_speech_silence_alternation() {
        let mut vad = VadProcessor::new(Some(100), None).unwrap(); // 100ms timeout

        let silence_frame = vec![0i16; 480];

        // Первый фрейм тишины (30ms)
        let r1 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(r1, VadResult::Silence);
        assert_eq!(vad.silence_duration(), Duration::from_millis(30));

        // Второй фрейм тишины (60ms) - еще не timeout
        let r2 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(r2, VadResult::Silence);
        assert_eq!(vad.silence_duration(), Duration::from_millis(60));

        // Третий фрейм (90ms) - еще не timeout
        let r3 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(r3, VadResult::Silence);
        assert_eq!(vad.silence_duration(), Duration::from_millis(90));

        // Четвертый фрейм (120ms) - должен сработать timeout
        let r4 = vad.process_samples(&silence_frame).unwrap();
        assert_eq!(r4, VadResult::SilenceTimeout);
    }

    #[test]
    fn test_boundary_cases() {
        let mut vad = VadProcessor::default().unwrap();

        // 479 samples - один sample не хватает до полного фрейма
        let samples_479 = vec![0i16; 479];
        let result = vad.process_samples(&samples_479).unwrap();
        assert_eq!(result, VadResult::Buffering);
        assert_eq!(vad.buffered_samples(), 479);

        // Добавляем еще один sample - должен обработаться фрейм
        let sample_1 = vec![0i16; 1];
        let result2 = vad.process_samples(&sample_1).unwrap();
        assert_ne!(result2, VadResult::Buffering);
        assert_eq!(vad.buffered_samples(), 0);

        // 481 samples - один sample лишний
        let samples_481 = vec![0i16; 481];
        let result3 = vad.process_samples(&samples_481).unwrap();
        assert_ne!(result3, VadResult::Buffering);
        assert_eq!(vad.buffered_samples(), 1); // Один sample остался
    }

    #[test]
    fn test_vad_modes() {
        // Тестируем разные режимы VAD
        let vad_quality = VadProcessor::new(Some(800), Some(VadMode::Quality)).unwrap();
        let vad_low_bitrate = VadProcessor::new(Some(800), Some(VadMode::LowBitrate)).unwrap();
        let vad_aggressive = VadProcessor::new(Some(800), Some(VadMode::Aggressive)).unwrap();
        let vad_very_aggressive = VadProcessor::new(Some(800), Some(VadMode::VeryAggressive)).unwrap();

        // Проверяем что все режимы создались успешно
        assert_eq!(vad_quality.timeout(), Duration::from_millis(800));
        assert_eq!(vad_low_bitrate.timeout(), Duration::from_millis(800));
        assert_eq!(vad_aggressive.timeout(), Duration::from_millis(800));
        assert_eq!(vad_very_aggressive.timeout(), Duration::from_millis(800));
    }
}
