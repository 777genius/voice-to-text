/**
 * Единая точка записи stt-config в backend (Tauri).
 *
 * Важно: Tauri ожидает аргументы invoke() в camelCase, даже если в Rust параметры snake_case.
 * Поэтому не используем deepgram_api_key/assemblyai_api_key в JS — только deepgramApiKey/assemblyaiApiKey.
 */

import { invoke } from '@tauri-apps/api/core';
import { CMD_UPDATE_STT_CONFIG } from './tauri';

export type UpdateSttConfigInvokeArgs = {
  provider: string;
  language: string;

  backendStreamingProvider?: string;
  deepgramApiKey?: string | null;
  assemblyaiApiKey?: string | null;
  model?: string | null;
  streamingKeyterms?: string | null;
};

const ALLOWED_KEYS = new Set([
  'provider',
  'language',
  'backendStreamingProvider',
  'deepgramApiKey',
  'assemblyaiApiKey',
  'model',
  'streamingKeyterms',
]);

function assertValidUpdateSttConfigArgs(args: Record<string, unknown>): void {
  for (const k of Object.keys(args)) {
    if (k.includes('_')) {
      throw new Error(
        `[update_stt_config] Нельзя использовать snake_case ключи в invoke args: "${k}". Ожидается camelCase.`,
      );
    }
    if (!ALLOWED_KEYS.has(k)) {
      throw new Error(
        `[update_stt_config] Неожиданный ключ "${k}". Разрешены: ${Array.from(ALLOWED_KEYS).join(', ')}`,
      );
    }
  }

  if (typeof args.provider !== 'string' || !args.provider.trim()) {
    throw new Error('[update_stt_config] provider обязателен и должен быть непустой строкой');
  }
  if (typeof args.language !== 'string' || !args.language.trim()) {
    throw new Error('[update_stt_config] language обязателен и должен быть непустой строкой');
  }
}

export async function invokeUpdateSttConfig(next: UpdateSttConfigInvokeArgs): Promise<void> {
  // Не мутируем исходный объект
  const args: Record<string, unknown> = { ...next };
  assertValidUpdateSttConfigArgs(args);
  await invoke(CMD_UPDATE_STT_CONFIG, args);
}
