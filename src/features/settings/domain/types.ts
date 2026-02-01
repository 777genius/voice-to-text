/**
 * Типы для модуля настроек
 */

import { SttProviderType } from '@/types';

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
  language: string;
  deepgramApiKey: string | null;
  assemblyaiApiKey: string | null;
  model: string | null;
}

// Конфигурация приложения (соответствует бэкенду)
export interface AppConfigData {
  microphone_sensitivity: number;
  recording_hotkey: string;
  auto_copy_to_clipboard: boolean;
  auto_paste_text: boolean;
  selected_audio_device: string | null;
}

// Полная конфигурация настроек для UI
export interface SettingsState {
  // Провайдер STT
  provider: SttProviderType;
  language: string;

  // API ключи
  deepgramApiKey: string;
  assemblyaiApiKey: string;

  // Whisper
  whisperModel: string;

  // Тема
  theme: AppTheme;

  // Горячая клавиша
  recordingHotkey: string;

  // Микрофон
  microphoneSensitivity: number;
  selectedAudioDevice: string;

  // Автоматические действия
  autoCopyToClipboard: boolean;
  autoPasteText: boolean;
}

// Статус сохранения
export type SaveStatus = 'idle' | 'saving' | 'success' | 'error';
