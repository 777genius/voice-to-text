/**
 * Контракты данных для state-sync (Tauri snapshot'ы).
 *
 * Здесь только типы (без логики), чтобы:
 * - не дублировать shape снапшотов по проекту
 * - уменьшить риск дрейфа между Rust и TS
 */

import type { SttProviderType } from '@/types';
import type { UiLocale, UiTheme } from '@/i18n.locales';
import type { SnapshotEnvelope } from '@statesync/core';

/**
 * Публичный снапшот app-config, который можно безопасно раздавать во все окна.
 * Соответствует Rust `AppConfigSnapshotData`.
 */
export type AppConfigSnapshotData = {
  microphone_sensitivity: number;
  recording_hotkey: string;
  auto_copy_to_clipboard: boolean;
  auto_paste_text: boolean;
  play_completion_sound: boolean;
  hide_recording_window_on_hotkey: boolean;
  show_mini_recording_window: boolean;
  keep_recording_until_manual_stop: boolean;
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
  deepgram_keyterms: string | null;
};

/** Соответствует Rust `AuthStateData`. */
export type AuthStateSnapshotData = {
  is_authenticated: boolean;
};

/**
 * Полный снапшот auth-session (device_id + tokens).
 * Соответствует Rust `AuthSessionSnapshotData`.
 *
 * Важно: содержит секреты (access/refresh), поэтому использовать только внутри приложения.
 */
export type AuthSessionSnapshotData = {
  device_id: string;
  session: null | {
    access_token: string;
    refresh_token: string | null;
    access_expires_at: string;
    refresh_expires_at: string | null;
    user: null | {
      id: string;
      email: string;
      email_verified: boolean;
    };
  };
};

/**
 * UI preferences (Rust SoT в Tauri).
 * Соответствует Rust `UiPreferences`.
 */
export type UiPreferencesSnapshotData = {
  theme: UiTheme;
  locale: UiLocale;
  use_system_theme: boolean;
};

// Переиспользуем envelope из state-sync, чтобы не плодить дубли.
export type TauriSnapshotEnvelope<T> = SnapshotEnvelope<T>;
