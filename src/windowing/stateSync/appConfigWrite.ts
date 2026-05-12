/**
 * Единая точка записи app-config в backend (Tauri).
 *
 * Важно: Tauri ожидает аргументы invoke() в camelCase, даже если в Rust параметры snake_case.
 * Если отправить snake_case (например microphone_sensitivity) — Rust получит None и ничего не сохранит,
 * а UI потом "откатится" на дефолты (95).
 *
 * Поэтому:
 * - принимаем строго camelCase
 * - делаем runtime-валидацию ключей (чтобы баг не вернулся тихо)
 */

import { invoke } from '@tauri-apps/api/core';
import { CMD_UPDATE_APP_CONFIG } from './tauri';

export type UpdateAppConfigInvokeArgs = Partial<{
  microphoneSensitivity: number;
  recordingHotkey: string;
  autoCopyToClipboard: boolean;
  autoPasteText: boolean;
  playCompletionSound: boolean;
  hideRecordingWindowOnHotkey: boolean;
  showMiniRecordingWindow: boolean;
  keepRecordingUntilManualStop: boolean;
  selectedAudioDevice: string | null;
}>;

const ALLOWED_KEYS = new Set([
  'microphoneSensitivity',
  'recordingHotkey',
  'autoCopyToClipboard',
  'autoPasteText',
  'playCompletionSound',
  'hideRecordingWindowOnHotkey',
  'showMiniRecordingWindow',
  'keepRecordingUntilManualStop',
  'selectedAudioDevice',
]);

function assertValidUpdateAppConfigArgs(args: Record<string, unknown>): void {
  for (const k of Object.keys(args)) {
    if (k.includes('_')) {
      throw new Error(
        `[update_app_config] Нельзя использовать snake_case ключи в invoke args: "${k}". Ожидается camelCase.`,
      );
    }
    if (!ALLOWED_KEYS.has(k)) {
      throw new Error(
        `[update_app_config] Неожиданный ключ "${k}". Разрешены: ${Array.from(ALLOWED_KEYS).join(', ')}`,
      );
    }

    const v = args[k];
    switch (k) {
      case 'microphoneSensitivity':
        if (typeof v !== 'number' || !Number.isFinite(v)) {
          throw new Error(`[update_app_config] "${k}" должен быть числом, получили: ${String(v)}`);
        }
        break;
      case 'recordingHotkey':
        if (typeof v !== 'string') {
          throw new Error(`[update_app_config] "${k}" должен быть строкой, получили: ${String(v)}`);
        }
        break;
      case 'autoCopyToClipboard':
      case 'autoPasteText':
      case 'playCompletionSound':
      case 'hideRecordingWindowOnHotkey':
      case 'showMiniRecordingWindow':
      case 'keepRecordingUntilManualStop':
        if (typeof v !== 'boolean') {
          throw new Error(`[update_app_config] "${k}" должен быть boolean, получили: ${String(v)}`);
        }
        break;
      case 'selectedAudioDevice':
        if (!(typeof v === 'string' || v === null)) {
          throw new Error(`[update_app_config] "${k}" должен быть string|null, получили: ${String(v)}`);
        }
        break;
    }
  }
}

export async function invokeUpdateAppConfig(next: UpdateAppConfigInvokeArgs): Promise<void> {
  // Не мутируем исходный объект
  const args: Record<string, unknown> = { ...next };
  assertValidUpdateAppConfigArgs(args);
  await invoke(CMD_UPDATE_APP_CONFIG, args);
}
