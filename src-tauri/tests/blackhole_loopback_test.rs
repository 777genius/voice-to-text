use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use app_lib::domain::{AudioEnqueueOutcome, TranslationAudioOutputMaintenance};
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

fn start_input_capture(input: &cpal::Device) -> (cpal::Stream, Arc<Mutex<Vec<f32>>>) {
    let input_config = input
        .default_input_config()
        .expect("audio input must have default config");
    let stream_config: cpal::StreamConfig = input_config.clone().into();
    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let captured_for_stream = captured.clone();
    let err_fn = |err| eprintln!("audio input stream error: {err}");
    let stream = match input_config.sample_format() {
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
    stream.play().expect("must start input stream");
    (stream, captured)
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

fn tone_amplitude(samples: &[f32], sample_rate: u32, frequency: f32) -> f32 {
    if samples.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let omega = 2.0 * std::f32::consts::PI * frequency / sample_rate as f32;
    let coefficient = 2.0 * omega.cos();
    let mut previous = 0.0f32;
    let mut previous_two = 0.0f32;
    for sample in samples {
        let current = *sample + coefficient * previous - previous_two;
        previous_two = previous;
        previous = current;
    }
    let power =
        previous_two * previous_two + previous * previous - coefficient * previous * previous_two;
    2.0 * power.max(0.0).sqrt() / samples.len() as f32
}

fn first_channel(samples: &[f32], channels: u16) -> Vec<f32> {
    samples
        .iter()
        .step_by(channels.max(1) as usize)
        .copied()
        .collect()
}

const OUTPUT_RESTORE_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_RESTORE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const OUTPUT_RESTORE_CONFIRMATIONS: usize = 2;

fn try_switch_audio_source(args: &[&str]) -> Result<String, String> {
    let output = Command::new("SwitchAudioSource")
        .args(args)
        .output()
        .map_err(|error| format!("failed to run SwitchAudioSource {args:?}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "SwitchAudioSource {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|stdout| stdout.trim().to_string())
        .map_err(|error| format!("SwitchAudioSource output was not UTF-8: {error}"))
}

fn switch_audio_source(args: &[&str]) -> String {
    try_switch_audio_source(args).unwrap_or_else(|error| panic!("{error}"))
}

fn current_output_device() -> String {
    switch_audio_source(&["-c", "-t", "output"])
}

fn set_output_device(name: &str) {
    switch_audio_source(&["-s", name, "-t", "output"]);
}

#[derive(Debug, PartialEq, Eq)]
enum RoutePollDecision {
    Restored,
    Retry { consecutive_matches: usize },
    TimedOut,
}

fn route_poll_decision(
    expected: &str,
    observed: Option<&str>,
    consecutive_matches: usize,
    deadline_reached: bool,
) -> RoutePollDecision {
    let next_matches = if observed == Some(expected) {
        consecutive_matches + 1
    } else {
        0
    };
    if next_matches >= OUTPUT_RESTORE_CONFIRMATIONS {
        RoutePollDecision::Restored
    } else if deadline_reached {
        RoutePollDecision::TimedOut
    } else {
        RoutePollDecision::Retry {
            consecutive_matches: next_matches,
        }
    }
}

fn wait_for_output_device(expected: &str) -> Result<(), String> {
    let deadline = Instant::now() + OUTPUT_RESTORE_TIMEOUT;
    let mut consecutive_matches = 0;

    loop {
        let observed = try_switch_audio_source(&["-c", "-t", "output"]);
        let last_observation = match &observed {
            Ok(name) => format!("'{name}'"),
            Err(error) => format!("query error: {error}"),
        };
        match route_poll_decision(
            expected,
            observed.as_deref().ok(),
            consecutive_matches,
            Instant::now() >= deadline,
        ) {
            RoutePollDecision::Restored => return Ok(()),
            RoutePollDecision::Retry {
                consecutive_matches: matches,
            } => consecutive_matches = matches,
            RoutePollDecision::TimedOut => {
                return Err(format!(
                    "timed out waiting for output device '{expected}'; last observation: {last_observation}"
                ));
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            std::thread::sleep(OUTPUT_RESTORE_POLL_INTERVAL.min(remaining));
        }
    }
}

struct DefaultOutputRestore {
    original: String,
    restored: bool,
}

impl DefaultOutputRestore {
    fn new(original: String) -> Self {
        Self {
            original,
            restored: false,
        }
    }

    fn try_restore(&mut self) -> Result<(), String> {
        let already_current = try_switch_audio_source(&["-c", "-t", "output"])
            .map(|current| current == self.original)
            .unwrap_or(false);
        if !already_current {
            try_switch_audio_source(&["-s", &self.original, "-t", "output"])?;
        }
        wait_for_output_device(&self.original)?;
        self.restored = true;
        Ok(())
    }

    fn restore(&mut self) {
        if let Err(error) = self.try_restore() {
            panic!(
                "failed to restore original default output device '{}': {error}",
                self.original
            );
        }
    }
}

impl Drop for DefaultOutputRestore {
    fn drop(&mut self) {
        if self.restored {
            return;
        }
        if let Err(error) = self.try_restore() {
            eprintln!(
                "failed to restore original default output device '{}': {error}",
                self.original,
            );
        }
    }
}

#[test]
fn route_polling_requires_stable_confirmation_and_resets_on_mismatch() {
    assert_eq!(
        route_poll_decision("Headphones", Some("Headphones"), 0, false),
        RoutePollDecision::Retry {
            consecutive_matches: 1
        }
    );
    assert_eq!(
        route_poll_decision("Headphones", Some("BlackHole 2ch"), 1, false),
        RoutePollDecision::Retry {
            consecutive_matches: 0
        }
    );
    assert_eq!(
        route_poll_decision("Headphones", Some("Headphones"), 1, true),
        RoutePollDecision::Restored
    );
    assert_eq!(
        route_poll_decision("Headphones", None, 1, true),
        RoutePollDecision::TimedOut
    );
}

fn alternate_output_device(original: &str) -> String {
    let devices = switch_audio_source(&["-a", "-t", "output"])
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != original)
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(
        !devices.is_empty(),
        "default-output switch test requires at least two output devices"
    );

    devices
        .iter()
        .find(|name| name.contains("BlackHole"))
        .or_else(|| devices.iter().find(|name| name.contains("MacBook")))
        .or_else(|| devices.iter().find(|name| name.contains("Built-in")))
        .unwrap_or(&devices[0])
        .clone()
}

fn blackhole_output_device() -> String {
    switch_audio_source(&["-a", "-t", "output"])
        .lines()
        .map(str::trim)
        .find(|name| name.contains("BlackHole"))
        .map(str::to_string)
        .expect("BlackHole output device must exist")
}

#[tokio::test]
#[ignore = "requires installed BlackHole 2ch and real CoreAudio devices"]
async fn cpal_output_reaches_blackhole_input() {
    let input = find_blackhole_input();
    let input_name = input.name().unwrap_or_else(|_| "unknown".to_string());
    let input_config = input
        .default_input_config()
        .expect("BlackHole input must have default config");
    let input_sample_rate = input_config.sample_rate().0;
    let input_channels = input_config.channels();
    let (input_stream, captured) = start_input_capture(&input);

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
    let tone = first_channel(&samples, input_channels);
    let measured_tone_amplitude = tone_amplitude(&tone, input_sample_rate, 880.0);

    assert!(
        measured_rms > 0.01 && peak > 0.05 && measured_tone_amplitude > 0.05,
        "BlackHole loopback did not capture the 880 Hz fixture: device={input_name}, samples={}, rms={measured_rms:.6}, peak={peak:.6}, tone={measured_tone_amplitude:.6}",
        samples.len()
    );
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "macOS release gate: requires BlackHole and switchaudio-osx"]
async fn system_default_output_reaches_selected_blackhole_device() {
    assert_eq!(std::env::consts::OS, "macos", "this test targets macOS");
    let original = current_output_device();
    let blackhole = blackhole_output_device();
    let mut restore = DefaultOutputRestore::new(original.clone());
    if original != blackhole {
        set_output_device(&blackhole);
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while current_output_device() != blackhole {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("BlackHole must become the system default output");

    let input = find_blackhole_input();
    let input_config = input
        .default_input_config()
        .expect("BlackHole input must have default config");
    let input_sample_rate = input_config.sample_rate().0;
    let input_channels = input_config.channels();
    let (input_stream, captured) = start_input_capture(&input);
    let mut output = CpalAudioOutput::system_default();
    output
        .open(AudioOutputConfig::incoming_spoken_translation())
        .await
        .expect("must open BlackHole through the production SystemDefault route");
    assert_eq!(output.device_name().as_deref(), Some(blackhole.as_str()));

    let tone = generated_tone_pcm16(24_000, Duration::from_secs(2), 880.0);
    output
        .enqueue_pcm16(&tone)
        .await
        .expect("must enqueue translated tone to SystemDefault");
    tokio::time::sleep(Duration::from_millis(2_800)).await;
    output
        .close()
        .await
        .expect("must close SystemDefault output");
    drop(input_stream);

    let samples = captured.lock().unwrap().clone();
    let measured_rms = rms(&samples);
    let peak = samples
        .iter()
        .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
    let tone = first_channel(&samples, input_channels);
    let measured_tone_amplitude = tone_amplitude(&tone, input_sample_rate, 880.0);
    restore.restore();

    assert!(
        measured_rms > 0.01 && peak > 0.05 && measured_tone_amplitude > 0.05,
        "SystemDefault route did not capture the 880 Hz fixture: samples={}, rms={measured_rms:.6}, peak={peak:.6}, tone={measured_tone_amplitude:.6}",
        samples.len()
    );
}

#[tokio::test]
#[ignore = "requires installed BlackHole 2ch and real CoreAudio devices"]
async fn incoming_spoken_profile_accepts_nine_second_burst_without_drop() {
    let mut output = CpalAudioOutput::new();
    output
        .open(AudioOutputConfig::incoming_spoken_translation())
        .await
        .expect("must open BlackHole with incoming spoken profile");

    let tone = generated_tone_pcm16(24_000, Duration::from_secs(9), 660.0);
    let outcome = output
        .enqueue_pcm16(&tone)
        .await
        .expect("must enqueue long incoming translation burst");

    assert!(
        matches!(outcome, AudioEnqueueOutcome::Queued { .. }),
        "nine-second incoming burst must fit without dropping audio: {outcome:?}"
    );
    assert!(
        output.pending_playback_duration() <= Duration::from_secs(10),
        "bounded incoming playback exceeded its configured headroom"
    );
    output.close().await.expect("must close BlackHole output");
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "macOS release gate: requires switchaudio-osx and at least two real output devices"]
async fn system_default_output_switch_recovers_on_the_new_route_and_restores_original() {
    assert_eq!(std::env::consts::OS, "macos", "this test targets macOS");
    let original = current_output_device();
    let alternate = alternate_output_device(&original);
    let mut restore = DefaultOutputRestore::new(original.clone());
    let mut output = CpalAudioOutput::system_default();
    let muted_config = AudioOutputConfig::incoming_spoken_translation().with_gain(0.0);
    output
        .open(muted_config)
        .await
        .expect("must open the original system default output");
    assert_eq!(output.device_name().as_deref(), Some(original.as_str()));

    set_output_device(&alternate);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if current_output_device() == alternate {
                match output.maintain().await {
                    Ok(TranslationAudioOutputMaintenance::Recovered { .. }) => break,
                    Ok(TranslationAudioOutputMaintenance::Healthy) => {}
                    Err(error) => panic!("route switch recovery failed: {error}"),
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("output maintenance must recover a default route switch");

    assert_eq!(output.device_name().as_deref(), Some(alternate.as_str()));
    assert!(output.health_check().is_ok());
    let translated_audio = vec![2_000; 2_400];
    let outcome = output
        .enqueue_pcm16(&translated_audio)
        .await
        .expect("recovered route must accept translated audio");
    assert_eq!(
        outcome,
        AudioEnqueueOutcome::Queued {
            pending: Duration::ZERO
        },
        "runtime mute/gain must survive route recovery"
    );

    output
        .close()
        .await
        .expect("must close the original output stream");
    restore.restore();
    tokio::time::timeout(Duration::from_secs(10), async {
        while current_output_device() != original {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("test must restore the original default output route");
}
