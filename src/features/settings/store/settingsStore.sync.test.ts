import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useSettingsStore } from './settingsStore';
import { CMD_UPDATE_UI_PREFERENCES } from '@/windowing/stateSync';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

describe('settingsStore cross-window UI sync', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    (window as any).__TAURI__ = {};
    invokeMock.mockReset();
    localStorage.clear();
  });

  it('setTheme вызывает update_ui_preferences через invoke', () => {
    const store = useSettingsStore();
    store.setTheme('light');

    expect(localStorage.getItem('uiTheme')).toBe('light');
    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_UI_PREFERENCES, {
      theme: 'light',
      locale: 'ru',
      use_system_theme: false,
    });
  });
});
