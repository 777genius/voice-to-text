// Domain types mirroring Rust backend

export enum RecordingStatus {
  Idle = 'Idle',
  Starting = 'Starting', // Запись инициализируется
  Recording = 'Recording',
  Processing = 'Processing',
  Error = 'Error',
}

export interface Transcription {
  text: string;
  is_final: boolean;
  confidence?: number;
  language?: string;
  timestamp: number;
}

export interface PartialTranscriptionPayload {
  session_id: number;
  text: string;
  timestamp: number;
  completion_v1?: boolean;
  delivery_seq?: number | null;
  timing_known?: boolean;
  is_segment_final: boolean; // true когда сегмент финализирован (но речь продолжается)
  start: number; // start время utterance в секундах (от Deepgram)
  duration: number; // длительность utterance в секундах (от Deepgram)
}

export interface FinalTranscriptionPayload {
  completion_v1?: boolean;
  delivery_seq?: number | null;
  timing_known?: boolean;
  session_id: number;
  text: string;
  confidence?: number;
  language?: string;
  timestamp: number;
  start?: number;
  duration?: number;
}

export interface ProviderFinalizeReport {
  reason: 'drained' | 'no_audio' | 'deadline' | 'provider_error' | 'cancelled';
  tail_evidence: 'no_audio' | 'segment_observed' | 'unconfirmed';
  provider_release: 'released' | 'reusable' | 'unconfirmed';
  last_delivery_seq: number;
  stable_snapshot: string;
  error?: string | null;
}

export interface FinalizeReport {
  run_id: number;
  audio: {
    accepted_bytes: number;
    read_bytes: number;
    submitted_bytes: number;
    acknowledged_bytes: number | null;
    unacknowledged_bytes: number | null;
    remaining_bytes: number;
    unknown_bytes: number;
    reason: 'drained' | 'deadline' | 'cancelled' | 'processor_error';
  };
  provider_release: 'released' | 'reusable' | 'unconfirmed';
  error: string | null;
  shared_failure: boolean;
  provider?: ProviderFinalizeReport | null;
}

export interface TranscriptionTerminalPayload {
  session_id: number;
  stable_snapshot: string;
  delivery_complete: boolean;
  report: FinalizeReport | null;
  error: string | null;
}

export type RecordingMode = 'dictation' | 'live_translation';

export interface RecordingStatusPayload {
  session_id: number;
  status: RecordingStatus;
  stopped_via_hotkey?: boolean;
  /** Активный режим сессии. Если undefined — считаем 'dictation' (back-compat). */
  mode?: RecordingMode;
}

export type ContinuationPhase = 'active' | 'pausing' | 'paused_reclaimable' | 'continue_pending' | 'active_awaiting_audio' | 'finalizing' | 'terminal';
export type GuardedPasteOutcome =
  | { status: 'confirmed'; revision: number }
  | { status: 'context_mismatch' | 'unavailable' | 'uncertain' };

export interface RecordingIntentProjectionPayload {
  logicalRunId?: number | null;
  captureEpisodeId?: number | null;
  continuationPhase?: ContinuationPhase | null;
  runId?: number | null;
  faultRunId?: number | null;
  intentRevision: number;
  status: RecordingStatus;
  desiredOn: boolean;
  pendingStart: boolean;
  processingJobs: number;
  shutdownRequested: boolean;
  fault?: 'startFailed' | 'runtimeFailed' | 'stopUncertain' | 'finalizeFailed';
}

export type RecordingCaptureReadinessState = 'unavailable' | 'buffering' | 'streaming';

export type RecordingCaptureReadinessReason =
  | 'idle'
  | 'starting-capture'
  | 'finalizing-previous'
  | 'connecting-provider'
  | 'recording'
  | 'cancelled'
  | 'error';

export interface RecordingCaptureReadinessPayload {
  logicalRunId?: number | null;
  captureEpisodeId?: number | null;
  captureGeneration?: number | null;
  captureReady?: boolean;
  transportReady?: boolean;
  /** Recording intent revision. Capture readiness is valid only for this exact intent. */
  revision: number | null;
  /** Capture run identity. This is deliberately independent from transcript session_id. */
  runId: number | null;
  state: RecordingCaptureReadinessState;
  reason: RecordingCaptureReadinessReason;
  /** Monotonic native event generation used to reject delayed delivery. */
  generation: number;
}

export interface TranslationDeltaPayload {
  session_id: number;
  text: string;
  timestamp: number;
}

export interface TranslationErrorPayload {
  session_id: number;
  error: string;
  /** 'configuration' | 'authentication' | 'rate_limited' | 'connection' | 'timeout' | 'processing' */
  error_type: string;
}

export interface IncomingTranslationStatusPayload {
  session_id: number;
  status: RecordingStatus;
  delivery?: 'captions_only' | 'text_and_audio';
  playback_state?: IncomingPlaybackState | null;
  muted?: boolean;
}

export type IncomingPlaybackState = 'opening' | 'playing' | 'draining' | 'stopped';

export interface IncomingTranslationPlaybackPayload {
  session_id: number;
  state: IncomingPlaybackState;
  muted: boolean;
}

export interface IncomingTranslationTextPayload {
  session_id: number;
  text: string;
  timestamp: number;
  delivery?: 'captions_only' | 'text_and_audio';
}

export interface IncomingTranslationErrorPayload {
  session_id: number;
  error: string;
  error_type: string;
}

export interface LiveTranslationHealthCheckItem {
  id: string;
  label: string;
  ok: boolean;
  required: boolean;
  message: string;
}

export interface LiveTranslationHealthCheck {
  ok: boolean;
  checked_at_ms: number;
  items: LiveTranslationHealthCheckItem[];
}

export interface ErrorPayload {
  message: string;
  code?: string;
}

export interface TranscriptionErrorPayload {
  session_id: number;
  error: string;
  error_type:
    | 'connection'
    | 'configuration'
    | 'processing'
    | 'timeout'
    | 'authentication'
    | 'rate_limited'
    | 'limit_exceeded'
    | 'provider_quota_exceeded';
  error_details?: TranscriptionErrorDetailsPayload;
}

export interface TranscriptionErrorDetailsPayload {
  category?:
    | 'offline'
    | 'dns'
    | 'tls'
    | 'refused'
    | 'reset'
    | 'timeout'
    | 'http'
    | 'server_unavailable'
    | 'server_error'
    | 'closed'
    | 'rate_limited'
    | 'limit_exceeded'
    | 'provider_quota_exceeded'
    | 'unknown';
  httpStatus?: number;
  wsCloseCode?: number;
  ioErrorKind?: string;
  osError?: number;
  serverCode?: string;
}

export enum ConnectionQuality {
  Good = 'Good',
  Poor = 'Poor',
  Recovering = 'Recovering',
}

export interface ConnectionQualityPayload {
  session_id: number;
  quality: ConnectionQuality;
  reason?: string;
}

// Event names (must match Rust backend)
export const EVENT_TRANSCRIPTION_PARTIAL = 'transcription:partial';
export const EVENT_TRANSCRIPTION_FINAL = 'transcription:final';
export const EVENT_TRANSCRIPTION_TERMINAL = 'transcription:terminal';
export const EVENT_RECORDING_STATUS = 'recording:status';
export const EVENT_RECORDING_INTENT_PROJECTION = 'recording:intent-projection';
export const EVENT_RECORDING_CAPTURE_READINESS = 'recording:capture-readiness';
export const EVENT_TRANSCRIPTION_ERROR = 'transcription:error';
export const EVENT_CONNECTION_QUALITY = 'connection:quality';
export const EVENT_TRANSLATION_DELTA = 'translation:delta';
export const EVENT_TRANSLATION_ERROR = 'translation:error';
export const EVENT_INCOMING_TRANSLATION_STATUS = 'incoming_translation:status';
export const EVENT_INCOMING_TRANSLATION_SOURCE_FINAL = 'incoming_translation:source-final';
export const EVENT_INCOMING_TRANSLATION_DELTA = 'incoming_translation:delta';
export const EVENT_INCOMING_TRANSLATION_ERROR = 'incoming_translation:error';
export const EVENT_INCOMING_TRANSLATION_PLAYBACK = 'incoming_translation:playback';
export const EVENT_ERROR = 'app:error';
export const EVENT_RECORDING_WINDOW_SHOWN = 'recording:window-shown';
export const EVENT_RECORDING_WINDOW_WILL_HIDE_FOR_HOTKEY_STOP = 'recording:window-will-hide-for-hotkey-stop';

// STT Configuration types
export enum SttProviderType {
  Mock = 'mock',
  AssemblyAI = 'assemblyai',
  Deepgram = 'deepgram',
  Backend = 'backend',
  WhisperLocal = 'whisperlocal',
  GoogleCloud = 'googlecloud',
  Azure = 'azure',
}

export enum BackendStreamingProviderType {
  Deepgram = 'deepgram',
  ElevenLabs = 'elevenlabs',
}

export interface SttConfig {
  provider: SttProviderType;
  backend_streaming_provider: BackendStreamingProviderType;
  language: string;
  auto_detect_language: boolean;
  enable_punctuation: boolean;
  filter_profanity: boolean;
  deepgram_api_key?: string;
  assemblyai_api_key?: string;
  model?: string;
}

// Whisper Model Management types
export interface WhisperModelInfo {
  name: string;
  size_bytes: number;
  size_human: string;
  download_url: string;
  description: string;
  speed_factor: number;
  quality_factor: number;
}

export interface WhisperModelDownloadProgress {
  model_name: string;
  downloaded: number;
  total: number;
  progress: number;
}

// Whisper events
export const EVENT_WHISPER_DOWNLOAD_STARTED = 'whisper-model:download-started';
export const EVENT_WHISPER_DOWNLOAD_PROGRESS = 'whisper-model:download-progress';
export const EVENT_WHISPER_DOWNLOAD_COMPLETED = 'whisper-model:download-completed';

// App update types/events
export interface AppUpdateInfo {
  version: string;
  body: string;
}

export interface AppUpdateDownloadProgress {
  version: string;
  downloaded: number;
  total: number | null;
  progress: number | null;
}

export const EVENT_UPDATE_AVAILABLE = 'update:available';
export const EVENT_UPDATE_DOWNLOAD_STARTED = 'update:download-started';
export const EVENT_UPDATE_DOWNLOAD_PROGRESS = 'update:download-progress';
export const EVENT_UPDATE_INSTALLING = 'update:installing';

// Settings focus events (между окнами)
export const EVENT_SETTINGS_FOCUS_UPDATES = 'settings:focus-updates';

export interface RecordingWindowLifecyclePayload {
  windowEpoch: number;
}
