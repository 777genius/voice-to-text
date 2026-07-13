#![allow(dead_code)]

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
const OPENAI_TRANSCRIPTION_SECONDARY_MODEL: &str = "gpt-4o-mini-transcribe";
const OPENAI_TRANSCRIPTION_LEGACY_MODEL: &str = "whisper-1";
const OPENAI_TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(90);

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
    transcribe_pcm16_with_model(
        client,
        api_key,
        sample_rate,
        channels,
        samples,
        OPENAI_TRANSCRIPTION_MODEL,
    )
    .await
}

pub async fn transcribe_pcm16_with_whisper(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> Result<String, String> {
    transcribe_pcm16_with_model(
        client,
        api_key,
        sample_rate,
        channels,
        samples,
        OPENAI_TRANSCRIPTION_LEGACY_MODEL,
    )
    .await
}

pub async fn transcribe_pcm16_with_mini(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> Result<String, String> {
    transcribe_pcm16_with_model(
        client,
        api_key,
        sample_rate,
        channels,
        samples,
        OPENAI_TRANSCRIPTION_SECONDARY_MODEL,
    )
    .await
}

#[derive(Debug, Default)]
pub struct TranscriptionCascade {
    pub primary_transcript: String,
    pub primary_error: Option<String>,
    pub segmented_attempted: bool,
    pub segmented_transcript: String,
    pub segmented_error: Option<String>,
    pub gpt_transcript: String,
    pub mini_attempted: bool,
    pub mini_transcript: String,
    pub mini_error: Option<String>,
    pub whisper_attempted: bool,
    pub whisper_transcript: String,
    pub whisper_error: Option<String>,
    pub transcript: String,
}

impl TranscriptionCascade {
    pub fn failure_summary(&self) -> String {
        format!(
            "primary: {}; segmented fallback: {}; mini fallback: {}; whisper fallback: {}",
            self.primary_error
                .as_deref()
                .unwrap_or("empty or incomplete transcript"),
            self.segmented_error.as_deref().unwrap_or("not available"),
            self.mini_error.as_deref().unwrap_or("not available"),
            self.whisper_error.as_deref().unwrap_or("not available")
        )
    }
}

pub async fn transcribe_pcm16_with_fallbacks(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
    is_incomplete: impl Fn(&str) -> bool,
) -> TranscriptionCascade {
    let primary = transcribe_pcm16(client, api_key, sample_rate, channels, samples).await;
    let primary_error = primary.as_ref().err().cloned();
    let primary_transcript = primary.unwrap_or_default();
    let segmented_attempted = primary_error.is_some() || is_incomplete(&primary_transcript);
    let segmented = if segmented_attempted {
        Some(transcribe_segmented_pcm16(client, api_key, sample_rate, channels, samples).await)
    } else {
        None
    };
    let segmented_error = segmented
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let segmented_transcript = segmented.and_then(Result::ok).unwrap_or_default();
    let primary_model_transcript =
        join_non_empty_transcripts(&primary_transcript, &segmented_transcript);
    let mini_attempted = is_incomplete(&primary_model_transcript);
    let mini = if mini_attempted {
        Some(transcribe_pcm16_with_mini(client, api_key, sample_rate, channels, samples).await)
    } else {
        None
    };
    let mini_error = mini
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let mini_transcript = mini.and_then(Result::ok).unwrap_or_default();
    let gpt_transcript = join_non_empty_transcripts(&primary_model_transcript, &mini_transcript);
    let whisper_attempted = is_incomplete(&gpt_transcript);
    let whisper = if whisper_attempted {
        Some(transcribe_pcm16_with_whisper(client, api_key, sample_rate, channels, samples).await)
    } else {
        None
    };
    let whisper_error = whisper
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let whisper_transcript = whisper.and_then(Result::ok).unwrap_or_default();
    let transcript = join_non_empty_transcripts(&gpt_transcript, &whisper_transcript);

    TranscriptionCascade {
        primary_transcript,
        primary_error,
        segmented_attempted,
        segmented_transcript,
        segmented_error,
        gpt_transcript,
        mini_attempted,
        mini_transcript,
        mini_error,
        whisper_attempted,
        whisper_transcript,
        whisper_error,
        transcript,
    }
}

fn join_non_empty_transcripts(first: &str, second: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (false, false) => format!("{first}\n{second}"),
        (false, true) => first.to_string(),
        (true, false) => second.to_string(),
        (true, true) => String::new(),
    }
}

async fn transcribe_pcm16_with_model(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
    model: &str,
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
        .timeout(OPENAI_TRANSCRIPTION_TIMEOUT)
        .body(multipart_transcription_body(&boundary, model, &wav))
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

pub fn audible_pcm16_window(samples: &[i16], sample_rate: u32, channels: u16) -> &[i16] {
    const AUDIBLE_THRESHOLD: i16 = 256;
    let Some(first) = samples
        .iter()
        .position(|sample| sample.unsigned_abs() >= AUDIBLE_THRESHOLD as u16)
    else {
        return samples;
    };
    let last = samples
        .iter()
        .rposition(|sample| sample.unsigned_abs() >= AUDIBLE_THRESHOLD as u16)
        .unwrap_or(first);
    let channels = usize::from(channels.max(1));
    let padding = (sample_rate as usize / 2).saturating_mul(channels);
    let mut start = first.saturating_sub(padding);
    start -= start % channels;
    let mut end = last
        .saturating_add(padding)
        .saturating_add(1)
        .min(samples.len());
    end -= end % channels;
    if end <= start {
        return samples;
    }
    &samples[start..end]
}

pub fn audible_pcm16_segments(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<&[i16]> {
    const AUDIBLE_THRESHOLD: i16 = 256;
    const SPLIT_SILENCE_MS: usize = 800;
    const PADDING_MS: usize = 500;
    const MIN_SPEECH_MS: usize = 100;

    let channels = usize::from(channels.max(1));
    let total_frames = samples.len() / channels;
    let sample_rate = sample_rate as usize;
    let split_silence_frames = sample_rate.saturating_mul(SPLIT_SILENCE_MS) / 1_000;
    let padding_frames = sample_rate.saturating_mul(PADDING_MS) / 1_000;
    let min_speech_frames = (sample_rate.saturating_mul(MIN_SPEECH_MS) / 1_000).max(2);
    let mut ranges = Vec::<(usize, usize)>::new();
    let mut speech_start = None::<usize>;
    let mut last_audible = 0usize;

    let mut finish_segment = |start: usize, last: usize| {
        if last.saturating_sub(start).saturating_add(1) < min_speech_frames {
            return;
        }
        ranges.push((
            start.saturating_sub(padding_frames),
            last.saturating_add(padding_frames)
                .saturating_add(1)
                .min(total_frames),
        ));
    };

    for (frame_index, frame) in samples.chunks_exact(channels).enumerate() {
        let audible = frame
            .iter()
            .any(|sample| sample.unsigned_abs() >= AUDIBLE_THRESHOLD as u16);
        if audible {
            speech_start.get_or_insert(frame_index);
            last_audible = frame_index;
        } else if let Some(start) = speech_start {
            if frame_index.saturating_sub(last_audible) >= split_silence_frames {
                finish_segment(start, last_audible);
                speech_start = None;
            }
        }
    }
    if let Some(start) = speech_start {
        finish_segment(start, last_audible);
    }
    if ranges.is_empty() {
        return vec![samples];
    }
    ranges
        .into_iter()
        .map(|(start, end)| &samples[start * channels..end * channels])
        .collect()
}

pub fn transcription_segments(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<&[i16]> {
    const MAX_CONTEXT_SECONDS: usize = 30;

    let max_context_samples = (sample_rate as usize)
        .saturating_mul(usize::from(channels.max(1)))
        .saturating_mul(MAX_CONTEXT_SECONDS);
    if samples.len() <= max_context_samples {
        vec![samples]
    } else {
        audible_pcm16_segments(samples, sample_rate, channels)
    }
}

async fn transcribe_segments(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    segments: Vec<&[i16]>,
) -> Result<String, String> {
    let mut transcripts = Vec::with_capacity(segments.len());
    for segment in segments {
        transcripts.push(
            transcribe_pcm16(client, api_key, sample_rate, channels, segment)
                .await?
                .trim()
                .to_string(),
        );
    }
    Ok(transcripts.join(" "))
}

pub async fn transcribe_audible_pcm16(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> Result<String, String> {
    transcribe_segments(
        client,
        api_key,
        sample_rate,
        channels,
        transcription_segments(samples, sample_rate, channels),
    )
    .await
}

pub async fn transcribe_segmented_pcm16(
    client: &reqwest::Client,
    api_key: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[i16],
) -> Result<String, String> {
    transcribe_segments(
        client,
        api_key,
        sample_rate,
        channels,
        audible_pcm16_segments(samples, sample_rate, channels),
    )
    .await
}

fn multipart_transcription_body(boundary: &str, model: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"model\"\r\n\r\n");
    body.extend_from_slice(model.as_bytes());
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
