// Non-Tauri fallback: source of truth = localStorage (+ локальная ревизия).
//
// Это нужно для web preview/юнит-тестов, где нет Rust SoT,
// но мы всё равно хотим строгий контракт "prefs + revision".

import { normalizeUiLocale, normalizeUiTheme } from '@/i18n.locales';
import type { UiLocale, UiTheme } from '@/i18n.locales';

export type UiPreferences = {
  theme: UiTheme;
  locale: UiLocale;
};

export const UI_PREFS_THEME_KEY = 'uiTheme';
export const UI_PREFS_LOCALE_KEY = 'uiLocale';
export const UI_PREFS_REVISION_KEY = 'uiPrefsRevision';
// Флаг разовой миграции localStorage → Rust (в Tauri режиме).
export const UI_PREFS_MIGRATED_TO_RUST_KEY = 'uiPrefs:migratedToRust';

function readRevisionRaw(): string {
  return localStorage.getItem(UI_PREFS_REVISION_KEY) ?? '0';
}

export function getUiPrefsRevision(): string {
  const raw = readRevisionRaw().trim();
  // Защита от мусора в localStorage
  if (!raw || !/^[0-9]+$/.test(raw)) return '0';
  // Убираем ведущие нули (кроме "0")
  if (raw.length > 1 && raw.startsWith('0')) return String(BigInt(raw));
  return raw;
}

export function bumpUiPrefsRevision(): string {
  const prev = BigInt(getUiPrefsRevision());
  const next = prev + BigInt(1);
  const nextStr = next.toString();
  localStorage.setItem(UI_PREFS_REVISION_KEY, nextStr);
  return nextStr;
}

export function readUiPreferencesFromStorage(): UiPreferences {
  return {
    theme: normalizeUiTheme(localStorage.getItem(UI_PREFS_THEME_KEY)),
    locale: normalizeUiLocale(localStorage.getItem(UI_PREFS_LOCALE_KEY)),
  };
}

/**
 * Пишем в localStorage как в кэш (без bump ревизии).
 * Важно для Tauri режима: source of truth — Rust, localStorage лишь "последнее применённое".
 */
export function writeUiPreferencesCacheToStorage(next: UiPreferences): UiPreferences {
  const normalized: UiPreferences = {
    theme: normalizeUiTheme(next.theme),
    locale: normalizeUiLocale(next.locale),
  };

  localStorage.setItem(UI_PREFS_THEME_KEY, normalized.theme);
  localStorage.setItem(UI_PREFS_LOCALE_KEY, normalized.locale);
  return normalized;
}

export function writeUiPreferencesToStorage(next: UiPreferences): { revision: string; data: UiPreferences } {
  const normalized: UiPreferences = {
    theme: normalizeUiTheme(next.theme),
    locale: normalizeUiLocale(next.locale),
  };

  const prevTheme = normalizeUiTheme(localStorage.getItem(UI_PREFS_THEME_KEY));
  const prevLocale = normalizeUiLocale(localStorage.getItem(UI_PREFS_LOCALE_KEY));
  const changed = prevTheme !== normalized.theme || prevLocale !== normalized.locale;

  // Всегда пишем значения (чтобы не держать "дырки" при отсутствии ключей),
  // но ревизию увеличиваем только если реально что-то поменялось.
  localStorage.setItem(UI_PREFS_THEME_KEY, normalized.theme);
  localStorage.setItem(UI_PREFS_LOCALE_KEY, normalized.locale);

  const revision = changed ? bumpUiPrefsRevision() : getUiPrefsRevision();
  return { revision, data: normalized };
}

