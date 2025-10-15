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

// Event names (must match Rust backend)
export const EVENT_TRANSCRIPTION_PARTIAL = 'transcription:partial';
export const EVENT_TRANSCRIPTION_FINAL = 'transcription:final';
export const EVENT_RECORDING_STATUS = 'recording:status';
export const EVENT_ERROR = 'app:error';

// STT Configuration types
export enum SttProviderType {
  Mock = 'mock',
  AssemblyAI = 'assemblyai',
  Deepgram = 'deepgram',
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
  api_key?: string;
  model?: string;
}
