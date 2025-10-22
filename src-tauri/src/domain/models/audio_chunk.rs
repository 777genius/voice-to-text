/// Represents a chunk of audio data for processing
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Raw PCM audio data (16-bit signed integers)
    pub data: Vec<i16>,

    /// Sample rate in Hz (e.g., 16000 for 16kHz)
    pub sample_rate: u32,

    /// Number of channels (1 for mono, 2 for stereo)
    pub channels: u16,

    /// Timestamp when this chunk was captured
    pub timestamp: i64,
}

impl AudioChunk {
    pub fn new(data: Vec<i16>, sample_rate: u32, channels: u16) -> Self {
        Self {
            data,
            sample_rate,
            channels,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
        }
    }

    /// Returns the duration of this chunk in milliseconds
    pub fn duration_ms(&self) -> u64 {
        (self.data.len() as u64 * 1000) / (self.sample_rate as u64 * self.channels as u64)
    }

    /// Converts to bytes for transmission (little-endian)
    pub fn to_bytes(&self) -> Vec<u8> {
        self.data
            .iter()
            .flat_map(|&sample| sample.to_le_bytes())
            .collect()
    }

    /// Creates from bytes (little-endian)
    pub fn from_bytes(bytes: &[u8], sample_rate: u32, channels: u16) -> Self {
        let data: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        Self::new(data, sample_rate, channels)
    }
}

/// Audio configuration parameters
#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    /// Sample rate in Hz (typically 16000 for speech recognition)
    pub sample_rate: u32,

    /// Number of channels (1 for mono, 2 for stereo)
    pub channels: u16,

    /// Buffer size in frames
    pub buffer_size: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000, // 16kHz is standard for speech recognition
            channels: 1,        // Mono
            buffer_size: 4096,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_chunk_new() {
        let data = vec![100, 200, 300];
        let chunk = AudioChunk::new(data.clone(), 16000, 1);
        assert_eq!(chunk.data, data);
        assert_eq!(chunk.sample_rate, 16000);
        assert_eq!(chunk.channels, 1);
        assert!(chunk.timestamp > 0);
    }

    #[test]
    fn test_audio_chunk_duration_mono() {
        let data = vec![0i16; 16000]; // 1 секунда @ 16kHz mono
        let chunk = AudioChunk::new(data, 16000, 1);
        assert_eq!(chunk.duration_ms(), 1000);
    }

    #[test]
    fn test_audio_chunk_duration_stereo() {
        let data = vec![0i16; 32000]; // 1 секунда @ 16kHz stereo
        let chunk = AudioChunk::new(data, 16000, 2);
        assert_eq!(chunk.duration_ms(), 1000);
    }

    #[test]
    fn test_audio_chunk_to_bytes() {
        let data = vec![0x0102i16, 0x0304i16];
        let chunk = AudioChunk::new(data, 16000, 1);
        let bytes = chunk.to_bytes();
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn test_audio_chunk_from_bytes() {
        let bytes = vec![0x02, 0x01, 0x04, 0x03];
        let chunk = AudioChunk::from_bytes(&bytes, 16000, 1);
        assert_eq!(chunk.data.len(), 2);
        assert_eq!(chunk.sample_rate, 16000);
        assert_eq!(chunk.channels, 1);
    }

    #[test]
    fn test_audio_chunk_round_trip() {
        let original_data = vec![100, -200, 300, -400];
        let chunk1 = AudioChunk::new(original_data.clone(), 16000, 1);
        let bytes = chunk1.to_bytes();
        let chunk2 = AudioChunk::from_bytes(&bytes, 16000, 1);
        assert_eq!(chunk2.data, original_data);
    }

    #[test]
    fn test_audio_chunk_clone() {
        let data = vec![1, 2, 3];
        let chunk1 = AudioChunk::new(data, 16000, 1);
        let chunk2 = chunk1.clone();
        assert_eq!(chunk1.data, chunk2.data);
        assert_eq!(chunk1.sample_rate, chunk2.sample_rate);
    }

    #[test]
    fn test_audio_config_default() {
        let config = AudioConfig::default();
        assert_eq!(config.sample_rate, 16000);
        assert_eq!(config.channels, 1);
        assert_eq!(config.buffer_size, 4096);
    }
}
