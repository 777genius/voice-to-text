import { describe, expect, it, vi, beforeEach } from 'vitest';

import { createUiPreferencesSync } from './uiPreferencesSync';
import { CMD_GET_UI_PREFERENCES_SNAPSHOT, CMD_UPDATE_UI_PREFERENCES } from './tauri';
import type { TauriInvoke, TauriListen } from '@statesync/tauri';
import {
  UI_PREFS_LOCALE_KEY,
  UI_PREFS_MIGRATED_TO_RUST_KEY,
  UI_PREFS_THEME_KEY,
  UI_PREFS_USE_SYSTEM_THEME_KEY,
} from './uiPreferences';

describe('createUiPreferencesSync', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('применяет snapshot (нормализуя мусор)', async () => {
    const applyTheme = vi.fn();
    const applyLocale = vi.fn();
    const applyUseSystemTheme = vi.fn();

    const listen: TauriListen = vi.fn(async (_eventName: string, _handler: any) => () => {}) as unknown as TauriListen;
    const invoke: TauriInvoke = vi.fn(async (cmd: string) => {
      expect(cmd).toBe(CMD_GET_UI_PREFERENCES_SNAPSHOT);
      return { revision: '1', data: { theme: 'neon', locale: 'zz', use_system_theme: true } };
    }) as unknown as TauriInvoke;

    const handle = createUiPreferencesSync({
      listen,
      invoke,
      applyTheme,
      applyLocale,
      applyUseSystemTheme,
    });

    await handle.start();

    expect(applyTheme).toHaveBeenCalledWith('dark');
    expect(applyLocale).toHaveBeenCalledWith('ru');
    expect(applyUseSystemTheme).toHaveBeenCalledWith(true);

    handle.stop();
  });

  it('делает миграцию localStorage → Rust только если Rust на дефолте', async () => {
    localStorage.setItem(UI_PREFS_THEME_KEY, 'light');
    localStorage.setItem(UI_PREFS_LOCALE_KEY, 'en');
    localStorage.setItem(UI_PREFS_USE_SYSTEM_THEME_KEY, '1');

    const applyTheme = vi.fn();
    const applyLocale = vi.fn();
    const applyUseSystemTheme = vi.fn();

    const listen: TauriListen = vi.fn(async (_eventName: string, _handler: any) => () => {}) as unknown as TauriListen;

    const invoke: TauriInvoke = vi.fn(async (cmd: string, args?: any) => {
      if (cmd === CMD_GET_UI_PREFERENCES_SNAPSHOT) {
        return { revision: '5', data: { theme: 'dark', locale: 'ru', use_system_theme: false } };
      }
      if (cmd === CMD_UPDATE_UI_PREFERENCES) {
        expect(args).toEqual({ theme: 'light', locale: 'en', use_system_theme: true });
        return null;
      }
      throw new Error(`unexpected invoke: ${cmd}`);
    }) as unknown as TauriInvoke;

    const handle = createUiPreferencesSync({
      listen,
      invoke,
      applyTheme,
      applyLocale,
      applyUseSystemTheme,
    });

    await handle.start();

    // Применяем локальные значения (чтобы пользователь не потерял настройки)
    expect(applyTheme).toHaveBeenCalledWith('light');
    expect(applyLocale).toHaveBeenCalledWith('en');
    expect(applyUseSystemTheme).toHaveBeenCalledWith(true);

    expect(localStorage.getItem(UI_PREFS_MIGRATED_TO_RUST_KEY)).toBe('1');

    handle.stop();
  });

  it('не ставит флаг миграции если update_ui_preferences упал', async () => {
    localStorage.setItem(UI_PREFS_THEME_KEY, 'light');
    localStorage.setItem(UI_PREFS_LOCALE_KEY, 'en');
    localStorage.setItem(UI_PREFS_USE_SYSTEM_THEME_KEY, '1');

    const applyTheme = vi.fn();
    const applyLocale = vi.fn();
    const applyUseSystemTheme = vi.fn();
    const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    const listen: TauriListen = vi.fn(async (_eventName: string, _handler: any) => () => {}) as unknown as TauriListen;

    const invoke: TauriInvoke = vi.fn(async (cmd: string) => {
      if (cmd === CMD_GET_UI_PREFERENCES_SNAPSHOT) {
        return { revision: '2', data: { theme: 'dark', locale: 'ru', use_system_theme: false } };
      }
      if (cmd === CMD_UPDATE_UI_PREFERENCES) {
        throw new Error('IPC error');
      }
      throw new Error(`unexpected invoke: ${cmd}`);
    }) as unknown as TauriInvoke;

    const handle = createUiPreferencesSync({
      listen,
      invoke,
      applyTheme,
      applyLocale,
      applyUseSystemTheme,
    });

    await handle.start();

    expect(consoleSpy).toHaveBeenCalled();

    // Флаг не должен быть установлен
    expect(localStorage.getItem(UI_PREFS_MIGRATED_TO_RUST_KEY)).toBeNull();

    // И при этом пользовательские значения применены
    expect(applyTheme).toHaveBeenCalledWith('light');
    expect(applyLocale).toHaveBeenCalledWith('en');
    expect(applyUseSystemTheme).toHaveBeenCalledWith(true);

    consoleSpy.mockRestore();
    handle.stop();
  });
});
