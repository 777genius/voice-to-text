import { beforeEach, describe, expect, it, vi } from 'vitest';

import { assertSafeExternalAuthUrl, TauriUrlOpener } from './TauriUrlOpener';

const openMock = vi.fn();

vi.mock('@tauri-apps/plugin-shell', () => ({
  open: (...args: any[]) => openMock(...args),
}));

describe('TauriUrlOpener', () => {
  beforeEach(() => {
    openMock.mockReset();
    openMock.mockResolvedValue(undefined);
  });

  it('opens HTTPS OAuth URLs', async () => {
    const opener = new TauriUrlOpener();

    await opener.open('https://accounts.google.com/o/oauth2/v2/auth?client_id=client');

    expect(openMock).toHaveBeenCalledWith(
      'https://accounts.google.com/o/oauth2/v2/auth?client_id=client',
    );
  });

  it('allows HTTPS redirect hosts without hardcoding providers', () => {
    expect(assertSafeExternalAuthUrl('https://api.voicetext.site/api/v1/auth/oauth/google/start')).toBe(
      'https://api.voicetext.site/api/v1/auth/oauth/google/start',
    );
    expect(assertSafeExternalAuthUrl('https://auth.example.com/oauth/start')).toBe(
      'https://auth.example.com/oauth/start',
    );
  });

  it('allows localhost HTTP for development callbacks', () => {
    expect(assertSafeExternalAuthUrl('http://localhost:1420/oauth/start')).toBe(
      'http://localhost:1420/oauth/start',
    );
    expect(assertSafeExternalAuthUrl('http://127.0.0.1:1420/oauth/start')).toBe(
      'http://127.0.0.1:1420/oauth/start',
    );
  });

  it('blocks non-web schemes and non-loopback HTTP before shell open', async () => {
    const opener = new TauriUrlOpener();

    await expect(opener.open('javascript:alert(1)')).rejects.toThrow('Blocked unsafe auth URL');
    await expect(opener.open('file:///etc/passwd')).rejects.toThrow('Blocked unsafe auth URL');
    await expect(opener.open('http://accounts.google.com/oauth')).rejects.toThrow(
      'Blocked unsafe auth URL',
    );
    await expect(opener.open('http://127.evil.com/oauth')).rejects.toThrow(
      'Blocked unsafe auth URL',
    );

    expect(openMock).not.toHaveBeenCalled();
  });

  it('blocks URLs with credentials before shell open', async () => {
    const opener = new TauriUrlOpener();

    await expect(opener.open('https://user:pass@accounts.google.com/oauth')).rejects.toThrow(
      'Unsafe auth URL credentials',
    );

    expect(openMock).not.toHaveBeenCalled();
  });
});
