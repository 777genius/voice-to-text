import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import type { TauriInvoke } from '@statesync/tauri';

import type { UiLocale, UiTheme } from '@/i18n.locales';
import { CMD_UPDATE_UI_PREFERENCES } from './tauri';

export type UpdateUiPreferencesInvokeArgs = {
  theme: UiTheme;
  locale: UiLocale;
  useSystemTheme: boolean;
};

const ALLOWED_KEYS = new Set(['theme', 'locale', 'useSystemTheme']);

function assertValidUpdateUiPreferencesArgs(args: Record<string, unknown>): void {
  for (const key of Object.keys(args)) {
    if (key.includes('_')) {
      throw new Error(
        `[update_ui_preferences] Cannot use snake_case invoke args: "${key}". Expected camelCase.`,
      );
    }
    if (!ALLOWED_KEYS.has(key)) {
      throw new Error(
        `[update_ui_preferences] Unexpected key "${key}". Allowed: ${Array.from(ALLOWED_KEYS).join(', ')}`,
      );
    }
  }

  if (args.theme !== 'dark' && args.theme !== 'light') {
    throw new Error(`[update_ui_preferences] "theme" must be 'dark' | 'light', got: ${String(args.theme)}`);
  }
  if (typeof args.locale !== 'string' || !args.locale.trim()) {
    throw new Error('[update_ui_preferences] "locale" must be a non-empty string');
  }
  if (typeof args.useSystemTheme !== 'boolean') {
    throw new Error(`[update_ui_preferences] "useSystemTheme" must be boolean, got: ${String(args.useSystemTheme)}`);
  }
}

export function invokeUpdateUiPreferences(
  next: UpdateUiPreferencesInvokeArgs,
  invoke: TauriInvoke = tauriInvoke as TauriInvoke,
): Promise<void> {
  const args: Record<string, unknown> = { ...next };
  try {
    assertValidUpdateUiPreferencesArgs(args);
  } catch (err) {
    return Promise.reject(err);
  }
  return invoke(CMD_UPDATE_UI_PREFERENCES, args) as Promise<void>;
}
