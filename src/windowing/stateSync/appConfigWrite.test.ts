import { beforeEach, describe, expect, it, vi } from 'vitest';

import { CMD_UPDATE_APP_CONFIG } from './tauri';
import { invokeUpdateAppConfig } from './appConfigWrite';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

describe('invokeUpdateAppConfig', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('passes holdToRecord to Tauri in camelCase args', async () => {
    invokeMock.mockResolvedValue(undefined);

    await invokeUpdateAppConfig({
      holdToRecord: true,
      keepRecordingUntilManualStop: true,
      doubleSpaceHotkeyEnabled: true,
    });

    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_APP_CONFIG, {
      holdToRecord: true,
      keepRecordingUntilManualStop: true,
      doubleSpaceHotkeyEnabled: true,
    });
  });

  it('rejects snake_case hold_to_record before invoking Tauri', async () => {
    await expect(
      invokeUpdateAppConfig({
        hold_to_record: true,
      } as any),
    ).rejects.toThrow('snake_case');

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('validates and forwards incoming translation settings', async () => {
    invokeMock.mockResolvedValue(undefined);

    await invokeUpdateAppConfig({
      incomingTranslationDelivery: 'text_and_audio',
      incomingTranslationVolume: 72,
    });

    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_APP_CONFIG, {
      incomingTranslationDelivery: 'text_and_audio',
      incomingTranslationVolume: 72,
    });
  });

  it('rejects an unknown incoming delivery mode', async () => {
    await expect(
      invokeUpdateAppConfig({ incomingTranslationDelivery: 'audio_only' } as any),
    ).rejects.toThrow('captions_only');
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
