import { beforeEach, describe, expect, it, vi } from 'vitest';

import { CMD_UPDATE_STT_CONFIG } from './tauri';
import { invokeUpdateSttConfig } from './sttConfigWrite';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

describe('invokeUpdateSttConfig', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('passes backendStreamingProvider to Tauri in camelCase args', async () => {
    invokeMock.mockResolvedValue(undefined);

    await invokeUpdateSttConfig({
      provider: 'backend',
      language: 'en',
      backendStreamingProvider: 'elevenlabs',
      deepgramKeyterms: 'VoicetextAI, ElevenLabs',
    });

    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_STT_CONFIG, {
      provider: 'backend',
      language: 'en',
      backendStreamingProvider: 'elevenlabs',
      deepgramKeyterms: 'VoicetextAI, ElevenLabs',
    });
  });

  it('rejects snake_case backend_streaming_provider before invoking Tauri', async () => {
    await expect(
      invokeUpdateSttConfig({
        provider: 'backend',
        language: 'en',
        backend_streaming_provider: 'elevenlabs',
      } as any),
    ).rejects.toThrow('snake_case');

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('rejects unknown provider keys before invoking Tauri', async () => {
    await expect(
      invokeUpdateSttConfig({
        provider: 'backend',
        language: 'en',
        backendProvider: 'elevenlabs',
      } as any),
    ).rejects.toThrow('Неожиданный ключ');

    expect(invokeMock).not.toHaveBeenCalled();
  });
});
