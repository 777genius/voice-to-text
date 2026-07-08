import { describe, expect, it, vi, beforeEach } from 'vitest';

import { CMD_UPDATE_UI_PREFERENCES } from './tauri';
import { invokeUpdateUiPreferences } from './uiPreferencesWrite';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

describe('invokeUpdateUiPreferences', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
  });

  it('uses camelCase Tauri args', async () => {
    await invokeUpdateUiPreferences({
      theme: 'light',
      locale: 'en',
      useSystemTheme: true,
    });

    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_UI_PREFERENCES, {
      theme: 'light',
      locale: 'en',
      useSystemTheme: true,
    });
  });

  it('rejects snake_case args before invoke', async () => {
    await expect(
      invokeUpdateUiPreferences({
        theme: 'light',
        locale: 'en',
        use_system_theme: true,
      } as any),
    ).rejects.toThrow(/snake_case/);

    expect(invokeMock).not.toHaveBeenCalled();
  });
});
