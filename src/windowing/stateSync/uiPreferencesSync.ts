/**
 * Синхронизация ui-preferences между окнами (Tauri).
 *
 * Важные правила:
 * - Source of truth в Tauri режиме: Rust.
 * - localStorage: кэш "последнее применённое".
 * - Разовая миграция localStorage → Rust допустима только если Rust на дефолте.
 */

import type { RevisionSyncHandle, SnapshotEnvelope } from '@statesync/core';
import type { TauriInvoke, TauriListen } from '@statesync/tauri';

import { createTauriRevisionSync } from '@statesync/tauri';

import {
  CMD_GET_UI_PREFERENCES_SNAPSHOT,
  CMD_UPDATE_UI_PREFERENCES,
  STATE_SYNC_INVALIDATION_EVENT,
} from './tauri';
import { TOPIC_UI_PREFERENCES } from './topics';
import type { UiPreferencesSnapshotData } from './contracts';
import {
  UI_PREFS_MIGRATED_TO_RUST_KEY,
  UI_PREFS_THEME_KEY,
  UI_PREFS_LOCALE_KEY,
  UI_PREFS_USE_SYSTEM_THEME_KEY,
  readUiPreferencesFromStorage,
} from './uiPreferences';
import { normalizeUiLocale, normalizeUiTheme } from '@/i18n.locales';

export interface CreateUiPreferencesSyncOptions {
  listen: TauriListen;
  invoke: TauriInvoke;

  applyTheme: (theme: UiPreferencesSnapshotData['theme']) => void;
  applyLocale: (locale: UiPreferencesSnapshotData['locale']) => void;
  applyUseSystemTheme: (useSystemTheme: UiPreferencesSnapshotData['use_system_theme']) => void;

  onError?: (ctx: { phase: string; error: unknown }) => void;
}

export function createUiPreferencesSync(
  options: CreateUiPreferencesSyncOptions,
): RevisionSyncHandle {
  const { listen, invoke, applyTheme, applyLocale, applyUseSystemTheme, onError } = options;

  return createTauriRevisionSync<UiPreferencesSnapshotData>({
    topic: TOPIC_UI_PREFERENCES,
    listen,
    invoke,
    eventName: STATE_SYNC_INVALIDATION_EVENT,
    commandName: CMD_GET_UI_PREFERENCES_SNAPSHOT,
    applier: {
      async apply(snapshot: SnapshotEnvelope<UiPreferencesSnapshotData>) {
        const data: UiPreferencesSnapshotData = {
          theme: normalizeUiTheme(snapshot.data.theme),
          locale: normalizeUiLocale(snapshot.data.locale),
          use_system_theme: Boolean(snapshot.data.use_system_theme),
        };

        // Разовая миграция localStorage → Rust (один раз на устройство)
        const migrated = localStorage.getItem(UI_PREFS_MIGRATED_TO_RUST_KEY);
        if (!migrated) {
          const storedThemeRaw = localStorage.getItem(UI_PREFS_THEME_KEY);
          const storedLocaleRaw = localStorage.getItem(UI_PREFS_LOCALE_KEY);
          const storedUseSystemThemeRaw = localStorage.getItem(UI_PREFS_USE_SYSTEM_THEME_KEY);
          const stored = readUiPreferencesFromStorage();

          // Мигрируем только если Rust на дефолтах —
          // иначе Rust уже настроен из другого окна, и localStorage устарел
          const rustIsDefault = data.theme === 'dark' && data.locale === 'ru';
          const hasLocalDiff =
            (storedThemeRaw && stored.theme !== data.theme) ||
            (storedLocaleRaw && stored.locale !== data.locale) ||
            (storedUseSystemThemeRaw && stored.useSystemTheme !== data.use_system_theme);

          if (rustIsDefault && hasLocalDiff) {
            // Применяем ЛОКАЛЬНЫЕ значения, а не дефолтный snapshot —
            // чтобы при ошибке invoke localStorage не потерялся
            applyTheme(stored.theme);
            applyLocale(stored.locale);
            applyUseSystemTheme(stored.useSystemTheme);

            try {
              await invoke(CMD_UPDATE_UI_PREFERENCES, {
                theme: stored.theme,
                locale: stored.locale,
                use_system_theme: stored.useSystemTheme,
              });
              localStorage.setItem(UI_PREFS_MIGRATED_TO_RUST_KEY, '1');
            } catch (err: unknown) {
              console.error('[ui-preferences] migration failed:', err);
              // Не ставим флаг — попробуем ещё раз при следующем старте.
            }
            return;
          }

          localStorage.setItem(UI_PREFS_MIGRATED_TO_RUST_KEY, '1');
        }

        // Rust — source of truth
        applyTheme(data.theme);
        applyLocale(data.locale);
        applyUseSystemTheme(data.use_system_theme);
      },
    },
    onError: (ctx) => {
      console.error(`[ui-preferences] sync error (${ctx.phase}):`, ctx.error);
      onError?.({ phase: ctx.phase, error: ctx.error });
    },
  });
}

