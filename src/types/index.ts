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
  text: string;
  timestamp: number;
  is_segment_final: boolean; // true когда сегмент финализирован (но речь продолжается)
  start: number; // start время utterance в секундах (от Deepgram)
  duration: number; // длительность utterance в секундах (от Deepgram)
}

export interface FinalTranscriptionPayload {
  text: string;
  confidence?: number;
  language?: string;
  timestamp: number;
}

export interface RecordingStatusPayload {
  status: RecordingStatus;
  stopped_via_hotkey?: boolean;
}

export interface ErrorPayload {
  message: string;
  code?: string;
}

export interface TranscriptionErrorPayload {
  error: string;
  error_type: 'connection' | 'configuration' | 'processing' | 'timeout' | 'authentication';
}

export enum ConnectionQuality {
  Good = 'Good',
  Poor = 'Poor',
  Recovering = 'Recovering',
}

export interface ConnectionQualityPayload {
  quality: ConnectionQuality;
  reason?: string;
}

// Event names (must match Rust backend)
export const EVENT_TRANSCRIPTION_PARTIAL = 'transcription:partial';
export const EVENT_TRANSCRIPTION_FINAL = 'transcription:final';
export const EVENT_RECORDING_STATUS = 'recording:status';
export const EVENT_TRANSCRIPTION_ERROR = 'transcription:error';
export const EVENT_CONNECTION_QUALITY = 'connection:quality';
export const EVENT_ERROR = 'app:error';

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

export interface SttConfig {
  provider: SttProviderType;
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
