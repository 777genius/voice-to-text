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
