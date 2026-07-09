import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useSettingsStore } from './settingsStore';
import { CMD_UPDATE_UI_PREFERENCES } from '@/windowing/stateSync';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

describe('settingsStore cross-window UI sync', () => {
  let consoleWarnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    setActivePinia(createPinia());
    (window as any).__TAURI__ = {};
    invokeMock.mockReset();
    localStorage.clear();
    consoleWarnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.useRealTimers();
    consoleWarnSpy.mockRestore();
  });

  it('создает новый store с продуктовыми дефолтами', () => {
    const store = useSettingsStore();

    expect(store.autoPasteText).toBe(true);
    expect(store.showMiniRecordingWindow).toBe(true);
    expect(store.holdToRecord).toBe(false);
    expect(store.doubleSpaceHotkeyEnabled).toBe(false);
  });

  it('setTheme вызывает update_ui_preferences через invoke', () => {
    const store = useSettingsStore();
    store.setTheme('light');

    expect(localStorage.getItem('uiTheme')).toBe('light');
    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_UI_PREFERENCES, {
      theme: 'light',
      locale: 'ru',
      useSystemTheme: false,
    });
  });

  it('setTheme ловит rejected update_ui_preferences без unhandled rejection', async () => {
    invokeMock.mockRejectedValueOnce(new Error('ui prefs ipc down'));
    const store = useSettingsStore();

    store.setTheme('light');
    await Promise.resolve();

    expect(consoleWarnSpy).toHaveBeenCalledWith(
      'Failed to persist UI preferences:',
      expect.any(Error)
    );
  });

  it('setUseSystemTheme ловит rejected update_ui_preferences без unhandled rejection', async () => {
    invokeMock.mockRejectedValueOnce(new Error('system theme ipc down'));
    const store = useSettingsStore();

    store.setUseSystemTheme(true);
    await Promise.resolve();

    expect(consoleWarnSpy).toHaveBeenCalledWith(
      'Failed to persist UI preferences:',
      expect.any(Error)
    );
  });

  it('setLanguage не считает failed debounce persist успешным и retry-ит тот же язык', async () => {
    vi.useFakeTimers();
    invokeMock
      .mockRejectedValueOnce(new Error('ipc down'))
      .mockResolvedValueOnce(undefined);
    const store = useSettingsStore();

    store.setLanguage('en');
    await vi.advanceTimersByTimeAsync(150);
    await Promise.resolve();

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(consoleWarnSpy).toHaveBeenCalledWith(
      'Failed to persist STT language:',
      expect.any(Error)
    );

    store.setLanguage('en');
    await vi.advanceTimersByTimeAsync(150);
    await Promise.resolve();

    expect(invokeMock).toHaveBeenCalledTimes(2);
  });
});
