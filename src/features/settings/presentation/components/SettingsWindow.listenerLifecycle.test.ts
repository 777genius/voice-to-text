import { createApp, nextTick } from 'vue';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsWindow from './SettingsWindow.vue';

const {
  appConfigMock,
  hideWindowMock,
  invokeMock,
  listenMock,
  loadConfigMock,
  localeRef,
  settingsStoreMock,
  sttConfigMock,
} = vi.hoisted(() => ({
  appConfigMock: {
    startSync: vi.fn(),
    stopSync: vi.fn(),
    refresh: vi.fn(),
  },
  hideWindowMock: vi.fn(),
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
  loadConfigMock: vi.fn(),
  localeRef: { value: 'en' },
  settingsStoreMock: {
    provider: 'backend',
    backendStreamingProvider: 'deepgram',
    language: 'en',
    deepgramApiKey: '',
    assemblyaiApiKey: '',
    openaiApiKey: '',
    whisperModel: 'base',
    theme: 'auto',
    useSystemTheme: true,
    recordingHotkey: 'CmdOrCtrl+Shift+X',
    microphoneSensitivity: 100,
    selectedAudioDevice: '',
    autoCopyToClipboard: false,
    autoPasteText: false,
    playCompletionSound: false,
    hideRecordingWindowOnHotkey: false,
    showMiniRecordingWindow: true,
    keepRecordingUntilManualStop: false,
    holdToRecord: false,
    doubleSpaceHotkeyEnabled: false,
    streamingKeyterms: '',
    recordingMode: 'dictation',
    applyState: vi.fn(),
  },
  sttConfigMock: {
    startSync: vi.fn(),
    stopSync: vi.fn(),
    refresh: vi.fn(),
  },
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    hide: hideWindowMock,
  }),
}));

vi.mock('vue-i18n', () => ({
  useI18n: () => ({
    locale: localeRef,
    t: (key: string) => key,
  }),
}));

vi.mock('@/utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

vi.mock('@/stores/appConfig', () => ({
  useAppConfigStore: () => appConfigMock,
}));

vi.mock('@/stores/sttConfig', () => ({
  useSttConfigStore: () => sttConfigMock,
}));

vi.mock('../../store/settingsStore', () => ({
  useSettingsStore: () => settingsStoreMock,
}));

vi.mock('../composables/useSettings', () => ({
  useSettings: () => ({
    loadConfig: loadConfigMock,
    saveConfig: vi.fn(),
    isSaving: { value: false },
    isLoading: { value: false },
    errorMessage: { value: '' },
    clearError: vi.fn(),
  }),
}));

vi.mock('../composables/useSettingsTheme', () => ({
  useSettingsTheme: () => ({
    initializeTheme: vi.fn(),
  }),
}));

vi.mock('@/composables/useUpdater', () => ({
  useUpdater: () => ({
    openUpdateWindow: vi.fn(),
  }),
}));

vi.mock('@/windowing/stateSync', () => ({
  invokeUpdateAppConfig: vi.fn(),
}));

vi.mock('@/presentation/components/UpdateDialog.vue', () => ({
  default: { name: 'UpdateDialog', template: '<div />' },
}));
vi.mock('./dialogs/UnsavedChangesDialog.vue', () => ({
  default: { name: 'UnsavedChangesDialog', template: '<div />' },
}));
vi.mock('./sections/LanguageSection.vue', () => ({
  default: { name: 'LanguageSection', template: '<div />' },
}));
vi.mock('./sections/StreamingProviderSection.vue', () => ({
  default: { name: 'StreamingProviderSection', template: '<div />' },
}));
vi.mock('./sections/KeytermsSection.vue', () => ({
  default: { name: 'KeytermsSection', template: '<div />' },
}));
vi.mock('./sections/ThemeSection.vue', () => ({
  default: { name: 'ThemeSection', template: '<div />' },
}));
vi.mock('./sections/HotkeySection.vue', () => ({
  default: { name: 'HotkeySection', template: '<div />' },
}));
vi.mock('./sections/RecordingModeSection.vue', () => ({
  default: { name: 'RecordingModeSection', template: '<div />' },
}));
vi.mock('./sections/AutoActionsSection.vue', () => ({
  default: { name: 'AutoActionsSection', template: '<div />' },
}));
vi.mock('./sections/AudioDeviceSection.vue', () => ({
  default: { name: 'AudioDeviceSection', template: '<div />' },
}));
vi.mock('./sections/MicTestSection.vue', () => ({
  default: { name: 'MicTestSection', template: '<div />' },
}));
vi.mock('./sections/UpdatesSection.vue', () => ({
  default: { name: 'UpdatesSection', template: '<div />' },
}));

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
  await nextTick();
}

function mountSettingsWindow() {
  const root = document.createElement('div');
  const app = createApp(SettingsWindow);
  for (const name of ['v-alert', 'v-btn', 'v-progress-circular', 'v-spacer']) {
    app.component(name, { template: '<div><slot /></div>' });
  }
  document.body.appendChild(root);
  app.mount(root);

  return {
    unmount() {
      app.unmount();
      root.remove();
    },
  };
}

describe('SettingsWindow listener lifecycle', () => {
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    appConfigMock.startSync.mockReset();
    appConfigMock.stopSync.mockReset();
    appConfigMock.refresh.mockReset();
    sttConfigMock.startSync.mockReset();
    sttConfigMock.stopSync.mockReset();
    sttConfigMock.refresh.mockReset();
    listenMock.mockReset();
    loadConfigMock.mockReset();
    invokeMock.mockReset();
    hideWindowMock.mockReset();
    settingsStoreMock.applyState.mockReset();
    localeRef.value = 'en';
    document.documentElement.dataset.uiLocale = '';
    consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    appConfigMock.startSync.mockResolvedValue(true);
    sttConfigMock.startSync.mockResolvedValue(true);
    loadConfigMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
    document.body.innerHTML = '';
  });

  it('останавливает settings sync и загружает конфиг, если settings-window listener не поднялся', async () => {
    listenMock.mockRejectedValueOnce(new Error('settings listener failed'));

    const wrapper = mountSettingsWindow();
    for (let i = 0; i < 20 && loadConfigMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }
    await flushMicrotasks();

    expect(appConfigMock.startSync).toHaveBeenCalledTimes(1);
    expect(sttConfigMock.startSync).toHaveBeenCalledTimes(1);
    expect(appConfigMock.stopSync).toHaveBeenCalledTimes(1);
    expect(sttConfigMock.stopSync).toHaveBeenCalledTimes(1);
    expect(loadConfigMock).toHaveBeenCalledTimes(1);
    expect(consoleErrorSpy).toHaveBeenCalledWith(
      'Failed to listen settings window open event:',
      expect.any(Error)
    );

    wrapper.unmount();
  });
});
