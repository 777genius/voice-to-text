/**
 * Типы для модуля настроек
 */

import { BackendStreamingProviderType, SttProviderType } from '@/types';

// Языки для распознавания речи
export interface LanguageOption {
  value: string;
  label: string;
}

// Опция провайдера STT
export interface ProviderOption {
  value: SttProviderType;
  label: string;
}

// Опция аудио устройства
export interface AudioDeviceOption {
  value: string;
  label: string;
}

// Модель Whisper
export interface WhisperModelOption {
  value: string;
  label: string;
}

// Тема приложения
export type AppTheme = 'dark' | 'light';

// Конфигурация STT (соответствует бэкенду)
export interface SttConfigData {
  provider: SttProviderType;
  backendStreamingProvider: BackendStreamingProviderType;
  language: string;
  deepgramApiKey: string | null;
  assemblyaiApiKey: string | null;
  model: string | null;
  streamingKeyterms: string | null;
}

// Конфигурация приложения (соответствует бэкенду)
export interface AppConfigData {
  microphone_sensitivity: number;
  recording_hotkey: string;
  auto_copy_to_clipboard: boolean;
  auto_paste_text: boolean;
  play_completion_sound: boolean;
  hide_recording_window_on_hotkey: boolean;
  show_mini_recording_window: boolean;
  keep_recording_until_manual_stop: boolean;
  selected_audio_device: string | null;
}

// Полная конфигурация настроек для UI
export interface SettingsState {
  // Провайдер STT
  provider: SttProviderType;
  backendStreamingProvider: BackendStreamingProviderType;
  language: string;

  // API ключи
  deepgramApiKey: string;
  assemblyaiApiKey: string;

  // Whisper
  whisperModel: string;

  // Тема
  theme: AppTheme;
  useSystemTheme: boolean;

  // Горячая клавиша
  recordingHotkey: string;

  // Микрофон
  microphoneSensitivity: number;
  selectedAudioDevice: string;

  // Автоматические действия
  autoCopyToClipboard: boolean;
  autoPasteText: boolean;
  playCompletionSound: boolean;
  hideRecordingWindowOnHotkey: boolean;
  showMiniRecordingWindow: boolean;
  keepRecordingUntilManualStop: boolean;

  // Streaming keyterms
  streamingKeyterms: string;
}

// Статус сохранения
export type SaveStatus = 'idle' | 'saving' | 'success' | 'error';
