/// Linear gain requested by the microphone sensitivity setting.
///
/// Sensitivity:
/// - 0%   = 0.0x
/// - 100% = 1.0x
/// - 200% = 5.0x
pub fn microphone_sensitivity_gain(sensitivity: u8) -> f32 {
    let sensitivity = sensitivity.min(200);
    if sensitivity <= 100 {
        sensitivity as f32 / 100.0
    } else {
        1.0 + (sensitivity - 100) as f32 / 100.0 * 4.0
    }
}

/// Gain after limiter headroom is applied for a concrete i16 frame/chunk.
pub fn limited_microphone_gain(sensitivity: u8, max_amplitude: i32) -> f32 {
    let requested_gain = microphone_sensitivity_gain(sensitivity);
    if max_amplitude <= 0 {
        return requested_gain;
    }

    let headroom = 0.98_f32;
    let limiter_gain = (32767.0 * headroom) / (max_amplitude as f32);
    requested_gain.min(limiter_gain)
}

pub fn amplify_i16_sample(sample: i16, gain: f32) -> i16 {
    let amplified = (sample as f32 * gain).clamp(-32767.0, 32767.0);
    amplified as i16
}

pub fn amplify_i16_samples(samples: &[i16], gain: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|&sample| amplify_i16_sample(sample, gain))
        .collect()
}
