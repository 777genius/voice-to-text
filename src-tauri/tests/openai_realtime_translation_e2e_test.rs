use std::fs;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_lib::infrastructure::audio::{AudioOutput, AudioOutputConfig, CpalAudioOutput};
use app_lib::infrastructure::openai::{OpenAIRealtimeEvent, OpenAIRealtimeTranslationClient};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

fn load_openai_api_key() -> String {
    let _ = dotenv::dotenv();
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .expect("OPENAI_API_KEY must be set in src-tauri/.env or environment")
}

fn generate_russian_pcm24() -> Vec<i16> {
    let tmp_dir = std::env::temp_dir();
    let aiff_path = tmp_dir.join("voicetext_openai_ru_source.aiff");
    let raw_path = tmp_dir.join("voicetext_openai_ru_source.s16le");

    let say_status = Command::new("say")
        .args([
            "-v",
            "Milena",
            "-o",
            aiff_path.to_str().expect("valid aiff path"),
            "Привет, меня зовут Алексей. Я проверяю перевод голоса на английский язык.",
        ])
        .status()
        .expect("must run macOS say");
    assert!(say_status.success(), "macOS say failed");

    let ffmpeg_status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            aiff_path.to_str().expect("valid aiff path"),
            "-ac",
            "1",
            "-ar",
            "24000",
            "-f",
            "s16le",
            raw_path.to_str().expect("valid raw path"),
        ])
        .status()
        .expect("must run ffmpeg");
    assert!(ffmpeg_status.success(), "ffmpeg conversion failed");

    let bytes = fs::read(raw_path).expect("must read generated pcm");
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

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

fn start_blackhole_capture(captured: Arc<Mutex<Vec<f32>>>) -> cpal::Stream {
    let input = find_blackhole_input();
    let input_config = input
        .default_input_config()
        .expect("BlackHole input must have default config");
    let stream_config: cpal::StreamConfig = input_config.clone().into();
    let err_fn = |err| eprintln!("BlackHole input stream error: {err}");

    match input_config.sample_format() {
        cpal::SampleFormat::F32 => input
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    captured.lock().unwrap().extend_from_slice(data);
                },
                err_fn,
                None,
            )
            .expect("must build f32 input stream"),
        cpal::SampleFormat::I16 => input
            .build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut guard = captured.lock().unwrap();
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
                    let mut guard = captured.lock().unwrap();
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
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|v| v * v).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn drain_openai_events(
    rx: &mut mpsc::UnboundedReceiver<OpenAIRealtimeEvent>,
    translated_text: &mut String,
    pending_audio: &mut Vec<Vec<i16>>,
) -> Option<String> {
    let mut failure = None;
    while let Ok(event) = rx.try_recv() {
        match event {
            OpenAIRealtimeEvent::SessionCreated => println!("openai_event=session.created"),
            OpenAIRealtimeEvent::SessionUpdated => println!("openai_event=session.updated"),
            OpenAIRealtimeEvent::TranscriptDelta(text) => {
                print!("{text}");
                translated_text.push_str(&text);
            }
            OpenAIRealtimeEvent::AudioDelta(samples) => pending_audio.push(samples),
            OpenAIRealtimeEvent::InputTranscriptDelta(text) => {
                println!("openai_input_delta={text}");
            }
            OpenAIRealtimeEvent::Error {
                message,
                kind,
                code,
            } => {
                failure = Some(format!(
                    "OpenAI realtime error ({kind:?}, code={code:?}): {message}"
                ));
            }
            OpenAIRealtimeEvent::Closed => {
                failure = Some("OpenAI realtime session closed while streaming".to_string());
            }
        }
    }
    failure
}

#[tokio::test]
#[ignore = "calls OpenAI realtime translation API and requires BlackHole 2ch"]
async fn openai_translation_audio_is_written_to_blackhole() {
    let api_key = load_openai_api_key();
    let source_pcm = generate_russian_pcm24();

    let mut client = OpenAIRealtimeTranslationClient::new(api_key, "en".to_string());
    let mut rx = client
        .connect()
        .await
        .expect("must connect OpenAI realtime");
    let mut translated_text = String::new();
    let mut pending_audio = Vec::<Vec<i16>>::new();

    tokio::time::sleep(Duration::from_millis(500)).await;
    if let Some(failure) = drain_openai_events(&mut rx, &mut translated_text, &mut pending_audio) {
        panic!("{failure}");
    }

    for chunk in source_pcm.chunks(4_800) {
        if let Some(failure) =
            drain_openai_events(&mut rx, &mut translated_text, &mut pending_audio)
        {
            panic!("{failure}");
        }
        client
            .append_input_audio(chunk)
            .await
            .expect("must append input audio");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    client
        .close(Duration::from_secs(6))
        .await
        .expect("must close OpenAI realtime session");
    let _ = drain_openai_events(&mut rx, &mut translated_text, &mut pending_audio);

    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let input_stream = start_blackhole_capture(captured.clone());
    input_stream.play().expect("must start BlackHole capture");

    let mut output = CpalAudioOutput::new();
    let output_config = AudioOutputConfig {
        max_buffered_frames: 1_000_000,
        ..AudioOutputConfig::openai_translation()
    };
    output
        .open(output_config)
        .await
        .expect("must open BlackHole output");

    let mut audio_samples = 0usize;
    let mut translated_audio_for_stats = Vec::new();
    for samples in pending_audio {
        audio_samples += samples.len();
        translated_audio_for_stats.extend(
            samples
                .iter()
                .map(|sample| *sample as f32 / i16::MAX as f32),
        );
        output
            .enqueue_pcm16(&samples)
            .await
            .expect("must enqueue translated audio");
    }
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
        match event {
            OpenAIRealtimeEvent::TranscriptDelta(text) => translated_text.push_str(&text),
            OpenAIRealtimeEvent::AudioDelta(samples) => {
                audio_samples += samples.len();
                translated_audio_for_stats.extend(
                    samples
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32),
                );
                output
                    .enqueue_pcm16(&samples)
                    .await
                    .expect("must enqueue translated audio");
            }
            OpenAIRealtimeEvent::Error { message, kind, .. } => {
                panic!("OpenAI realtime error ({kind:?}): {message}");
            }
            _ => {}
        }
    }

    tokio::time::sleep(Duration::from_secs(6)).await;
    output.close().await.expect("must close output");
    drop(input_stream);

    let captured_samples = captured.lock().unwrap().clone();
    let measured_rms = rms(&captured_samples);
    let peak = captured_samples
        .iter()
        .fold(0.0f32, |acc, sample| acc.max(sample.abs()));

    println!("translated_text={translated_text}");
    println!(
        "openai_audio_samples={audio_samples}, openai_audio_rms={:.6}, blackhole_samples={}, blackhole_rms={measured_rms:.6}, blackhole_peak={peak:.6}",
        rms(&translated_audio_for_stats),
        captured_samples.len()
    );

    assert!(audio_samples > 0, "OpenAI did not return translated audio");
    assert!(
        translated_text.to_lowercase().contains("english")
            || translated_text.to_lowercase().contains("voice")
            || translated_text.to_lowercase().contains("translation")
            || translated_text.to_lowercase().contains("alex"),
        "translated text looks unexpected/empty: {translated_text}"
    );
    assert!(
        measured_rms > 0.005 && peak > 0.03,
        "translated audio did not reach BlackHole input: rms={measured_rms:.6}, peak={peak:.6}"
    );
}
