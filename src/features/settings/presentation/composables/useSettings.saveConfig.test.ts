import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useSettings } from './useSettings';
import { useSettingsStore } from '../../store/settingsStore';
import { BackendStreamingProviderType } from '@/types';

const { localeRef, invokeMock, tauriSettingsServiceMock } = vi.hoisted(() => ({
  localeRef: { value: 'ru' },
  invokeMock: vi.fn(),
  tauriSettingsServiceMock: {
    getSttConfig: vi.fn(),
    updateSttConfig: vi.fn(),
    getAppConfig: vi.fn(),
    updateAppConfig: vi.fn(),
    getAudioDevices: vi.fn(),
    checkAccessibilityPermission: vi.fn(),
    requestAccessibilityPermission: vi.fn(),
  },
}));

vi.mock('vue-i18n', () => ({
  useI18n: () => ({
    locale: localeRef,
    t: (key: string) => key,
  }),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@/utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

vi.mock('../../infrastructure/adapters/TauriSettingsService', () => ({
  tauriSettingsService: tauriSettingsServiceMock,
}));

describe('useSettings saveConfig', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    (window as any).__TAURI__ = {};
    localStorage.clear();
    localeRef.value = 'ru';
    invokeMock.mockReset();
    tauriSettingsServiceMock.getSttConfig.mockReset();
    tauriSettingsServiceMock.updateSttConfig.mockReset();
    tauriSettingsServiceMock.getAppConfig.mockReset();
    tauriSettingsServiceMock.updateAppConfig.mockReset();
  });

  it('при изменении только keyterms использует актуальный backend language и не пишет app-config', async () => {
    const store = useSettingsStore();
    store.setLanguage('ru', { persist: false });
    store.setMicrophoneSensitivity(100, { persist: false });
    store.setRecordingHotkey('CmdOrCtrl+Shift+X');
    store.setAutoCopyToClipboard(false);
    store.setAutoPasteText(false);
    store.setHideRecordingWindowOnHotkey(false);
    store.setShowMiniRecordingWindow(false);
    store.setKeepRecordingUntilManualStop(false);
    store.setSelectedAudioDevice('');
    store.setStreamingKeyterms('', { persist: false });
    store.capturePersistedState();

    store.setStreamingKeyterms('Kubernetes, VoicetextAI', { persist: false });

    tauriSettingsServiceMock.getSttConfig
      .mockResolvedValueOnce({
        language: 'en',
        streaming_keyterms: null,
      })
      .mockResolvedValueOnce({
        language: 'en',
        streaming_keyterms: 'Kubernetes, VoicetextAI',
      });

    tauriSettingsServiceMock.getAppConfig.mockResolvedValueOnce({
      microphone_sensitivity: 100,
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      play_completion_sound: false,
      hide_recording_window_on_hotkey: false,
      show_mini_recording_window: false,
      keep_recording_until_manual_stop: false,
      selected_audio_device: null,
    });

    tauriSettingsServiceMock.updateSttConfig.mockResolvedValue(undefined);

    const { saveConfig } = useSettings();
    await expect(saveConfig()).resolves.toBe(true);

    expect(tauriSettingsServiceMock.updateSttConfig).toHaveBeenCalledWith({
      provider: 'backend',
      language: 'en',
      backendStreamingProvider: 'deepgram',
      streamingKeyterms: 'Kubernetes, VoicetextAI',
    });
    expect(tauriSettingsServiceMock.updateAppConfig).not.toHaveBeenCalled();
  });

  it('сохраняет переключение backend streaming provider без app-config write', async () => {
    const store = useSettingsStore();
    store.setLanguage('ru', { persist: false });
    store.setBackendStreamingProvider(BackendStreamingProviderType.Deepgram);
    store.setMicrophoneSensitivity(100, { persist: false });
    store.setRecordingHotkey('CmdOrCtrl+Shift+X');
    store.setAutoCopyToClipboard(false);
    store.setAutoPasteText(false);
    store.setHideRecordingWindowOnHotkey(false);
    store.setShowMiniRecordingWindow(false);
    store.setKeepRecordingUntilManualStop(false);
    store.setSelectedAudioDevice('');
    store.setStreamingKeyterms('', { persist: false });
    store.capturePersistedState();

    store.setBackendStreamingProvider(BackendStreamingProviderType.ElevenLabs);

    tauriSettingsServiceMock.getSttConfig
      .mockResolvedValueOnce({
        language: 'ru',
        backend_streaming_provider: 'deepgram',
        streaming_keyterms: null,
      })
      .mockResolvedValueOnce({
        language: 'ru',
        backend_streaming_provider: 'elevenlabs',
        streaming_keyterms: null,
      });

    tauriSettingsServiceMock.getAppConfig.mockResolvedValueOnce({
      microphone_sensitivity: 100,
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      play_completion_sound: false,
      hide_recording_window_on_hotkey: false,
      show_mini_recording_window: false,
      keep_recording_until_manual_stop: false,
      selected_audio_device: null,
    });

    tauriSettingsServiceMock.updateSttConfig.mockResolvedValue(undefined);

    const { saveConfig } = useSettings();
    await expect(saveConfig()).resolves.toBe(true);

    expect(tauriSettingsServiceMock.updateSttConfig).toHaveBeenCalledWith({
      provider: 'backend',
      language: 'ru',
      backendStreamingProvider: 'elevenlabs',
    });
    expect(tauriSettingsServiceMock.updateAppConfig).not.toHaveBeenCalled();
  });

  it('при изменении только sensitivity сохраняет только её и пропускает STT write', async () => {
    const store = useSettingsStore();
    store.setLanguage('ru', { persist: false });
    store.setMicrophoneSensitivity(100, { persist: false });
    store.setRecordingHotkey('CmdOrCtrl+Shift+X');
    store.setAutoCopyToClipboard(false);
    store.setAutoPasteText(false);
    store.setHideRecordingWindowOnHotkey(false);
    store.setShowMiniRecordingWindow(false);
    store.setKeepRecordingUntilManualStop(false);
    store.setSelectedAudioDevice('');
    store.setStreamingKeyterms('', { persist: false });
    store.capturePersistedState();

    store.setMicrophoneSensitivity(175, { persist: false });

    tauriSettingsServiceMock.getSttConfig.mockResolvedValueOnce({
      language: 'ru',
      streaming_keyterms: null,
    });

    tauriSettingsServiceMock.getAppConfig
      .mockResolvedValueOnce({
        microphone_sensitivity: 100,
        recording_hotkey: 'CmdOrCtrl+Shift+X',
        auto_copy_to_clipboard: false,
        auto_paste_text: false,
        play_completion_sound: false,
        hide_recording_window_on_hotkey: false,
        show_mini_recording_window: false,
        keep_recording_until_manual_stop: false,
        selected_audio_device: null,
      })
      .mockResolvedValueOnce({
        microphone_sensitivity: 175,
        recording_hotkey: 'CmdOrCtrl+Shift+X',
        auto_copy_to_clipboard: false,
        auto_paste_text: false,
        play_completion_sound: false,
        hide_recording_window_on_hotkey: false,
        show_mini_recording_window: false,
        keep_recording_until_manual_stop: false,
        selected_audio_device: null,
      });

    tauriSettingsServiceMock.updateAppConfig.mockResolvedValue(undefined);

    const { saveConfig } = useSettings();
    await expect(saveConfig()).resolves.toBe(true);

    expect(tauriSettingsServiceMock.updateSttConfig).not.toHaveBeenCalled();
    expect(tauriSettingsServiceMock.updateAppConfig).toHaveBeenCalledWith({
      microphone_sensitivity: 175,
    });
  });

  it('при изменении только режима окна для хоткея сохраняет только его', async () => {
    const store = useSettingsStore();
    store.setLanguage('ru', { persist: false });
    store.setMicrophoneSensitivity(100, { persist: false });
    store.setRecordingHotkey('CmdOrCtrl+Shift+X');
    store.setAutoCopyToClipboard(false);
    store.setAutoPasteText(false);
    store.setPlayCompletionSound(false);
    store.setHideRecordingWindowOnHotkey(false);
    store.setShowMiniRecordingWindow(false);
    store.setKeepRecordingUntilManualStop(false);
    store.setSelectedAudioDevice('');
    store.setStreamingKeyterms('', { persist: false });
    store.capturePersistedState();

    store.setHideRecordingWindowOnHotkey(true);

    tauriSettingsServiceMock.getSttConfig.mockResolvedValueOnce({
      language: 'ru',
      streaming_keyterms: null,
    });

    tauriSettingsServiceMock.getAppConfig.mockResolvedValueOnce({
      microphone_sensitivity: 100,
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      play_completion_sound: false,
      hide_recording_window_on_hotkey: false,
      show_mini_recording_window: false,
      keep_recording_until_manual_stop: false,
      selected_audio_device: null,
    });

    tauriSettingsServiceMock.updateAppConfig.mockResolvedValue(undefined);

    const { saveConfig } = useSettings();
    await expect(saveConfig()).resolves.toBe(true);

    expect(tauriSettingsServiceMock.updateSttConfig).not.toHaveBeenCalled();
    expect(tauriSettingsServiceMock.updateAppConfig).toHaveBeenCalledWith({
      hide_recording_window_on_hotkey: true,
    });
  });

  it('сохраняет только новые режимы окна и ручной остановки', async () => {
    const store = useSettingsStore();
    store.setLanguage('ru', { persist: false });
    store.setMicrophoneSensitivity(100, { persist: false });
    store.setRecordingHotkey('CmdOrCtrl+Shift+X');
    store.setAutoCopyToClipboard(false);
    store.setAutoPasteText(false);
    store.setPlayCompletionSound(false);
    store.setHideRecordingWindowOnHotkey(false);
    store.setShowMiniRecordingWindow(false);
    store.setKeepRecordingUntilManualStop(false);
    store.setSelectedAudioDevice('');
    store.setStreamingKeyterms('', { persist: false });
    store.capturePersistedState();

    store.setShowMiniRecordingWindow(true);
    store.setKeepRecordingUntilManualStop(true);

    tauriSettingsServiceMock.getSttConfig.mockResolvedValueOnce({
      language: 'ru',
      streaming_keyterms: null,
    });

    tauriSettingsServiceMock.getAppConfig.mockResolvedValueOnce({
      microphone_sensitivity: 100,
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      play_completion_sound: false,
      hide_recording_window_on_hotkey: false,
      show_mini_recording_window: false,
      keep_recording_until_manual_stop: false,
      selected_audio_device: null,
    });

    tauriSettingsServiceMock.updateAppConfig.mockResolvedValue(undefined);

    const { saveConfig } = useSettings();
    await expect(saveConfig()).resolves.toBe(true);

    expect(tauriSettingsServiceMock.updateSttConfig).not.toHaveBeenCalled();
    expect(tauriSettingsServiceMock.updateAppConfig).toHaveBeenCalledWith({
      show_mini_recording_window: true,
      keep_recording_until_manual_stop: true,
    });
  });

  it('сохраняет переключение auto-copy и auto-paste', async () => {
    const store = useSettingsStore();
    store.setLanguage('ru', { persist: false });
    store.setMicrophoneSensitivity(100, { persist: false });
    store.setRecordingHotkey('CmdOrCtrl+Shift+X');
    store.setAutoCopyToClipboard(false);
    store.setAutoPasteText(false);
    store.setPlayCompletionSound(false);
    store.setHideRecordingWindowOnHotkey(false);
    store.setShowMiniRecordingWindow(false);
    store.setKeepRecordingUntilManualStop(false);
    store.setSelectedAudioDevice('');
    store.setStreamingKeyterms('', { persist: false });
    store.capturePersistedState();

    store.setAutoCopyToClipboard(true);
    store.setAutoPasteText(true);

    tauriSettingsServiceMock.getSttConfig.mockResolvedValueOnce({
      language: 'ru',
      streaming_keyterms: null,
    });

    tauriSettingsServiceMock.getAppConfig.mockResolvedValueOnce({
      microphone_sensitivity: 100,
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      play_completion_sound: false,
      hide_recording_window_on_hotkey: false,
      show_mini_recording_window: false,
      keep_recording_until_manual_stop: false,
      selected_audio_device: null,
    });

    tauriSettingsServiceMock.updateAppConfig.mockResolvedValue(undefined);

    const { saveConfig } = useSettings();
    await expect(saveConfig()).resolves.toBe(true);

    expect(tauriSettingsServiceMock.updateSttConfig).not.toHaveBeenCalled();
    expect(tauriSettingsServiceMock.updateAppConfig).toHaveBeenCalledWith({
      auto_copy_to_clipboard: true,
      auto_paste_text: true,
    });
  });
});
