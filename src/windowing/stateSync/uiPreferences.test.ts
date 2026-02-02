import { describe, expect, it, vi } from 'vitest';

import {
  UI_PREFS_LOCALE_KEY,
  UI_PREFS_THEME_KEY,
  UI_PREFS_USE_SYSTEM_THEME_KEY,
  UI_PREFS_REVISION_KEY,
  getUiPrefsRevision,
  readUiPreferencesFromStorage,
  writeUiPreferencesCacheToStorage,
  writeUiPreferencesToStorage,
} from './uiPreferences';

describe('stateSync/uiPreferences (storage helpers)', () => {
  it('readUiPreferencesFromStorage нормализует мусорные значения', () => {
    localStorage.setItem(UI_PREFS_THEME_KEY, 'neon');
    localStorage.setItem(UI_PREFS_LOCALE_KEY, 'zz');
    const prefs = readUiPreferencesFromStorage();
    expect(prefs.theme).toBe('dark');
    expect(prefs.locale).toBe('ru');
    expect(prefs.useSystemTheme).toBe(false);
  });

  it('writeUiPreferencesToStorage bump-ает ревизию только при реальных изменениях', () => {
    localStorage.clear();
    localStorage.setItem(UI_PREFS_REVISION_KEY, '0');

    const bumpSpy = vi.spyOn(Storage.prototype, 'setItem');

    const first = writeUiPreferencesToStorage({ theme: 'dark', locale: 'ru', useSystemTheme: false });
    expect(first.revision).toBe('0'); // дефолт уже совпадает, ревизия не растёт
    expect(getUiPrefsRevision()).toBe('0');

    const second = writeUiPreferencesToStorage({ theme: 'light', locale: 'ru', useSystemTheme: false });
    expect(second.revision).toBe('1');
    expect(getUiPrefsRevision()).toBe('1');

    // Повторяем то же самое — ревизия не должна расти
    const third = writeUiPreferencesToStorage({ theme: 'light', locale: 'ru', useSystemTheme: false });
    expect(third.revision).toBe('1');
    expect(getUiPrefsRevision()).toBe('1');

    bumpSpy.mockRestore();
  });

  it('writeUiPreferencesCacheToStorage пишет кэш без bump ревизии', () => {
    localStorage.clear();
    localStorage.setItem(UI_PREFS_REVISION_KEY, '10');

    writeUiPreferencesCacheToStorage({ theme: 'light', locale: 'en', useSystemTheme: true });
    expect(localStorage.getItem(UI_PREFS_THEME_KEY)).toBe('light');
    expect(localStorage.getItem(UI_PREFS_LOCALE_KEY)).toBe('en');
    expect(localStorage.getItem(UI_PREFS_USE_SYSTEM_THEME_KEY)).toBe('1');
    expect(getUiPrefsRevision()).toBe('10');
  });
});

