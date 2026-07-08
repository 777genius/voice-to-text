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

  it('uses an injected invoke implementation', async () => {
    const customInvoke = vi.fn().mockResolvedValue(undefined);

    await invokeUpdateUiPreferences(
      {
        theme: 'dark',
        locale: 'ru',
        useSystemTheme: false,
      },
      customInvoke,
    );

    expect(customInvoke).toHaveBeenCalledWith(CMD_UPDATE_UI_PREFERENCES, {
      theme: 'dark',
      locale: 'ru',
      useSystemTheme: false,
    });
    expect(invokeMock).not.toHaveBeenCalled();
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

  it('rejects invalid theme before invoke', async () => {
    await expect(
      invokeUpdateUiPreferences({
        theme: 'purple',
        locale: 'en',
        useSystemTheme: true,
      } as any),
    ).rejects.toThrow(/theme/);

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('rejects empty locale before invoke', async () => {
    await expect(
      invokeUpdateUiPreferences({
        theme: 'dark',
        locale: '  ',
        useSystemTheme: false,
      } as any),
    ).rejects.toThrow(/locale/);

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('rejects non-boolean useSystemTheme before invoke', async () => {
    await expect(
      invokeUpdateUiPreferences({
        theme: 'dark',
        locale: 'en',
        useSystemTheme: 'yes',
      } as any),
    ).rejects.toThrow(/useSystemTheme/);

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('rejects unexpected keys before invoke', async () => {
    await expect(
      invokeUpdateUiPreferences({
        theme: 'dark',
        locale: 'en',
        useSystemTheme: true,
        extra: 'nope',
      } as any),
    ).rejects.toThrow(/Unexpected key/);

    expect(invokeMock).not.toHaveBeenCalled();
  });
});
