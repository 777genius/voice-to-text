import { describe, expect, it, vi, beforeEach } from 'vitest';

import { tauriSettingsService } from './TauriSettingsService';

const invokeMock = vi.fn();
const listenMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

describe('TauriSettingsService', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
  });

  it('starts microphone test with camelCase deviceName arg', async () => {
    await tauriSettingsService.startMicrophoneTest(125, 'Studio Mic');

    expect(invokeMock).toHaveBeenCalledWith('start_microphone_test', {
      sensitivity: 125,
      deviceName: 'Studio Mic',
    });
  });

  it('checks Whisper model with camelCase modelName arg', async () => {
    invokeMock.mockResolvedValueOnce(true);

    await expect(tauriSettingsService.checkWhisperModel('small')).resolves.toBe(true);

    expect(invokeMock).toHaveBeenCalledWith('check_whisper_model', {
      modelName: 'small',
    });
  });

  it('clears selected audio device with an empty string arg', async () => {
    await tauriSettingsService.updateAppConfig({ selected_audio_device: null });

    expect(invokeMock).toHaveBeenCalledWith('update_app_config', {
      selectedAudioDevice: '',
    });
  });
});
