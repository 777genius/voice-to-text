use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use app_lib::domain::{
    AudioEnqueueOutcome, LocalPlaybackOutputFactory, LocalPlaybackRoute, TranslationAudioOutput,
    TranslationAudioOutputConfig, TranslationAudioOutputResult,
};
use async_trait::async_trait;
use reqwest::header::CONTENT_TYPE;

const OPENAI_TRANSCRIPTIONS_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
const OPENAI_TRANSCRIPTION_MODEL: &str = "gpt-4o-transcribe";

#[derive(Default)]
#[allow(dead_code)]
pub struct PlaybackProbe {
    opened: AtomicUsize,
    closed: AtomicUsize,
    accepted_samples: AtomicUsize,
    audible_accepted_samples: AtomicUsize,
    dropped_batches: AtomicUsize,
    dropped_audio_micros: AtomicU64,
}

#[allow(dead_code)]
impl PlaybackProbe {
    pub fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    pub fn closed(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn accepted_samples(&self) -> usize {
        self.accepted_samples.load(Ordering::Relaxed)
    }

    pub fn audible_accepted_samples(&self) -> usize {
        self.audible_accepted_samples.load(Ordering::Relaxed)
    }

    pub fn dropped_batches(&self) -> usize {
        self.dropped_batches.load(Ordering::Relaxed)
    }

    pub fn dropped_audio_duration(&self) -> Duration {
        Duration::from_micros(self.dropped_audio_micros.load(Ordering::Relaxed))
    }
}

#[allow(dead_code)]
pub struct ObservedLocalPlaybackFactory {
    inner: Arc<dyn LocalPlaybackOutputFactory>,
    probe: Arc<PlaybackProbe>,
}

#[allow(dead_code)]
impl ObservedLocalPlaybackFactory {
    pub fn new(inner: Arc<dyn LocalPlaybackOutputFactory>, probe: Arc<PlaybackProbe>) -> Self {
        Self { inner, probe }
    }
}

impl LocalPlaybackOutputFactory for ObservedLocalPlaybackFactory {
    fn create_local_playback_output(
        &self,
        route: LocalPlaybackRoute,
    ) -> TranslationAudioOutputResult<Box<dyn TranslationAudioOutput>> {
        Ok(Box::new(ObservedLocalPlaybackOutput {
            inner: self.inner.create_local_playback_output(route)?,
            probe: self.probe.clone(),
        }))
    }
}

#[allow(dead_code)]
struct ObservedLocalPlaybackOutput {
    inner: Box<dyn TranslationAudioOutput>,
    probe: Arc<PlaybackProbe>,
}

#[async_trait]
impl TranslationAudioOutput for ObservedLocalPlaybackOutput {
    async fn open(
        &mut self,
        config: TranslationAudioOutputConfig,
    ) -> TranslationAudioOutputResult<()> {
        self.inner.open(config).await?;
        self.probe.opened.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn enqueue_pcm16(
        &self,
        samples: &[i16],
    ) -> TranslationAudioOutputResult<AudioEnqueueOutcome> {
        let outcome = self.inner.enqueue_pcm16(samples).await?;
        self.probe
            .accepted_samples
            .fetch_add(samples.len(), Ordering::Relaxed);
        self.probe.audible_accepted_samples.fetch_add(
            samples
                .iter()
                .filter(|sample| sample.unsigned_abs() > 256)
                .count(),
            Ordering::Relaxed,
        );
        match outcome {
            AudioEnqueueOutcome::Queued { .. } => {}
            AudioEnqueueOutcome::DroppedOldest { duration, .. } => {
                self.probe.dropped_batches.fetch_add(1, Ordering::Relaxed);
                self.probe.dropped_audio_micros.fetch_add(
                    duration.as_micros().min(u64::MAX as u128) as u64,
                    Ordering::Relaxed,
                );
            }
        }
        Ok(outcome)
    }

    async fn close(&mut self) -> TranslationAudioOutputResult<()> {
        self.inner.close().await?;
        self.probe.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn set_gain(&mut self, gain: f32) -> TranslationAudioOutputResult<()> {
        self.inner.set_gain(gain)
    }

    fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    fn health_check(&self) -> TranslationAudioOutputResult<()> {
        self.inner.health_check()
    }

    fn device_name(&self) -> Option<String> {
        self.inner.device_name()
    }

    fn begin_drain_mode(&self) {
        self.inner.begin_drain_mode();
    }

    fn prepare_for_drain(&self) -> TranslationAudioOutputResult<Duration> {
        self.inner.prepare_for_drain()
    }

    fn pending_playback_duration(&self) -> Duration {
        self.inner.pending_playback_duration()
    }
}

pub fn load_paid_e2e_api_key() -> String {
    assert_eq!(
        std::env::var("VOICETEXT_RUN_PAID_E2E").as_deref(),
        Ok("1"),
        "set VOICETEXT_RUN_PAID_E2E=1 to acknowledge paid realtime API usage"
    );
    std::env::var("OPENAI_E2E_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .expect("OPENAI_E2E_API_KEY must contain a dedicated revocable test key")
}

pub fn wav_pcm16(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let byte_rate = sample_rate * u32::from(channels) * 2;
    let block_align = channels * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }

    wav
}

pub async fn transcribe_pcm16(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> Result<String, String> {
    let wav = wav_pcm16(sample_rate, channels, samples);
    let boundary = format!("voicetext-e2e-{}", std::process::id());
    let response = client
        .post(OPENAI_TRANSCRIPTIONS_URL)
        .bearer_auth(api_key)
        .header(
            CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_transcription_body(&boundary, &wav))
        .send()
        .await
        .map_err(|error| format!("OpenAI transcription request failed: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("OpenAI transcription response failed: {error}"))?;

    if !status.is_success() {
        return Err(format!(
            "OpenAI transcription HTTP {}: {}",
            status.as_u16(),
            text
        ));
    }
    let transcript = text.trim().to_string();
    if transcript.is_empty() {
        return Err("OpenAI transcription returned empty text".into());
    }
    Ok(transcript)
}

fn multipart_transcription_body(boundary: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(OPENAI_TRANSCRIPTION_MODEL.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"response_format\"\r\n\r\n");
    body.extend_from_slice(b"text\r\n");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"translation-e2e.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    body
}
