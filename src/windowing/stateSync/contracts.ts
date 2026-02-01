/**
 * Контракты данных для state-sync (Tauri snapshot'ы).
 *
 * Здесь только типы (без логики), чтобы:
 * - не дублировать shape снапшотов по проекту
 * - уменьшить риск дрейфа между Rust и TS
 */

import type { SttProviderType } from '@/types';
import type { UiLocale, UiTheme } from '@/i18n.locales';
import type { SnapshotEnvelope } from 'state-sync';

/**
 * Публичный снапшот app-config, который можно безопасно раздавать во все окна.
 * Соответствует Rust `AppConfigSnapshotData`.
 */
export type AppConfigSnapshotData = {
  microphone_sensitivity: number;
  recording_hotkey: string;
  auto_copy_to_clipboard: boolean;
  auto_paste_text: boolean;
  selected_audio_device: string | null;
};

/**
 * Публичный снапшот stt-config, который можно безопасно раздавать во все окна.
 * Соответствует Rust `SttConfigSnapshotData`.
 */
export type SttConfigSnapshotData = {
  provider: SttProviderType;
  language: string;

  auto_detect_language: boolean;
  enable_punctuation: boolean;
  filter_profanity: boolean;

  deepgram_api_key: string | null;
  assemblyai_api_key: string | null;
  model: string | null;

  keep_connection_alive: boolean;
};

/** Соответствует Rust `AuthStateData`. */
export type AuthStateSnapshotData = {
  is_authenticated: boolean;
};

/**
 * UI preferences (Rust SoT в Tauri).
 * Соответствует Rust `UiPreferences`.
 */
export type UiPreferencesSnapshotData = {
  theme: UiTheme;
  locale: UiLocale;
};

// Переиспользуем envelope из state-sync, чтобы не плодить дубли.
export type TauriSnapshotEnvelope<T> = SnapshotEnvelope<T>;

