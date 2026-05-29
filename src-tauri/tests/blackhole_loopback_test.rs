use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_lib::infrastructure::audio::{AudioOutput, AudioOutputConfig, CpalAudioOutput};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn find_blackhole_input() -> cpal::Device {
    let host = cpal::default_host();
    host.input_devices()
        .expect("must list input devices")
        .find(|device| {
            device
                .name()
                .map(|name| name.contains("BlackHole 2ch") || name.contains("BlackHole"))
                .unwrap_or(false)
        })
        .expect("BlackHole input device must exist")
}

fn generated_tone_pcm16(sample_rate: u32, duration: Duration, frequency: f32) -> Vec<i16> {
    let sample_count = (sample_rate as f32 * duration.as_secs_f32()) as usize;
    (0..sample_count)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.35;
            (sample * i16::MAX as f32) as i16
        })
        .collect()
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|v| v * v).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[tokio::test]
#[ignore = "requires installed BlackHole 2ch and real CoreAudio devices"]
async fn cpal_output_reaches_blackhole_input() {
    let input = find_blackhole_input();
    let input_name = input.name().unwrap_or_else(|_| "unknown".to_string());
    let input_config = input
        .default_input_config()
        .expect("BlackHole input must have default config");
    let stream_config: cpal::StreamConfig = input_config.clone().into();
    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let captured_for_stream = captured.clone();

    let err_fn = |err| eprintln!("BlackHole input stream error: {err}");
    let input_stream = match input_config.sample_format() {
        cpal::SampleFormat::F32 => input
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    captured_for_stream.lock().unwrap().extend_from_slice(data);
                },
                err_fn,
                None,
            )
            .expect("must build f32 input stream"),
        cpal::SampleFormat::I16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut guard = captured_for_stream.lock().unwrap();
                    guard.extend(data.iter().map(|sample| *sample as f32 / i16::MAX as f32));
                },
                err_fn,
                None,
            )
            .expect("must build i16 input stream"),
        cpal::SampleFormat::U16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let mut guard = captured_for_stream.lock().unwrap();
                    guard.extend(
                        data.iter()
                            .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0),
                    );
                },
                err_fn,
                None,
            )
            .expect("must build u16 input stream"),
        other => panic!("unsupported input sample format: {other:?}"),
    };
    input_stream.play().expect("must start input stream");

    let mut output = CpalAudioOutput::new();
    output
        .open(AudioOutputConfig::openai_translation())
        .await
        .expect("must open BlackHole output");

    let tone = generated_tone_pcm16(24_000, Duration::from_secs(2), 880.0);
    output
        .enqueue_pcm16(&tone)
        .await
        .expect("must enqueue tone");

    tokio::time::sleep(Duration::from_millis(2_800)).await;
    output.close().await.expect("must close output");
    drop(input_stream);

    let samples = captured.lock().unwrap().clone();
    let measured_rms = rms(&samples);
    let peak = samples
        .iter()
        .fold(0.0f32, |acc, sample| acc.max(sample.abs()));

    assert!(
        measured_rms > 0.01 && peak > 0.05,
        "BlackHole loopback captured silence/too low signal: device={input_name}, samples={}, rms={measured_rms:.6}, peak={peak:.6}",
        samples.len()
    );
}
