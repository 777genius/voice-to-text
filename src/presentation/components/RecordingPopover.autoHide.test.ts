import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, nextTick } from 'vue';
import { createI18n } from 'vue-i18n';
import { createPinia, setActivePinia } from 'pinia';
import RecordingPopover from './RecordingPopover.vue';
import { RecordingStatus } from '../../types';
import { useTranscriptionStore } from '../../stores/transcription';

type TauriEventHandler = (event: { payload: any }) => unknown | Promise<unknown>;

const tauriEventMock = vi.hoisted(() => ({
  handlers: new Map<string, TauriEventHandler[]>(),
  listen: vi.fn(),
}));

const invokeMock = vi.hoisted(() => vi.fn());
const hideWindowMock = vi.hoisted(() => vi.fn());
const cursorOverRecordingWindowMock = vi.hoisted(() => ({ value: false }));

const appConfigMock = vi.hoisted(() => ({
  autoCopyToClipboard: false,
  autoPasteText: false,
  playCompletionSound: false,
  hideRecordingWindowOnHotkey: false,
  showMiniRecordingWindow: true,
  keepRecordingUntilManualStop: false,
  recordingHotkey: 'CmdOrCtrl+Shift+X',
  recordingMode: 'dictation' as 'dictation' | 'live_translation',
  startSync: vi.fn(),
  stopSync: vi.fn(),
  refresh: vi.fn(),
}));

const sttConfigMock = vi.hoisted(() => ({
  startSync: vi.fn(),
  stopSync: vi.fn(),
}));

const authMock = vi.hoisted(() => ({
  initialize: vi.fn(),
}));

const authStoreMock = vi.hoisted(() => ({
  isAuthenticated: false,
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => tauriEventMock.listen(...args),
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  getCurrentWebviewWindow: () => ({
    hide: hideWindowMock,
    innerSize: vi.fn().mockResolvedValue({ width: 248, height: 62 }),
    outerPosition: vi.fn().mockResolvedValue({ x: 100, y: 100 }),
    outerSize: vi.fn().mockResolvedValue({ width: 248, height: 62 }),
    startDragging: vi.fn(),
  }),
}));

vi.mock('@tauri-apps/api/window', () => ({
  currentMonitor: vi.fn().mockResolvedValue({
    position: { x: 0, y: 0 },
    size: { width: 1440, height: 900 },
  }),
}));

vi.mock('@tauri-apps/api/app', () => ({
  getVersion: vi.fn().mockResolvedValue('0.11.1-test'),
}));

vi.mock('../../utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

vi.mock('../../utils/sound', () => ({
  playShowSound: vi.fn(),
  playDoneSound: vi.fn(),
  preloadUiSounds: vi.fn(),
}));

vi.mock('../../stores/appConfig', () => ({
  useAppConfigStore: () => appConfigMock,
}));

vi.mock('../../stores/sttConfig', () => ({
  useSttConfigStore: () => sttConfigMock,
}));

vi.mock('../../features/settings', () => ({
  SettingsPanel: { name: 'SettingsPanelStub', render: () => null },
  useSettingsStore: () => ({ pendingScrollToSection: null }),
}));

vi.mock('../../features/auth/store/authStore', () => ({
  useAuthStore: () => authStoreMock,
}));

vi.mock('../../features/auth', () => ({
  useAuth: () => authMock,
}));

vi.mock('../../composables/useUpdater', () => ({
  useUpdater: () => ({ openUpdateWindow: vi.fn().mockResolvedValue(false) }),
}));

vi.mock('./ProfilePopover.vue', () => ({
  default: { name: 'ProfilePopoverStub', render: () => null },
}));

vi.mock('./UpdateIndicator.vue', () => ({
  default: { name: 'UpdateIndicatorStub', render: () => null },
}));

vi.mock('./UpdateDialog.vue', () => ({
  default: { name: 'UpdateDialogStub', render: () => null },
}));

vi.mock('./AudioVisualizer.vue', () => ({
  default: { name: 'AudioVisualizerStub', render: () => null },
}));

function flushMicrotasks() {
  return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve());
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

async function waitForListenerCount(eventName: string, count: number) {
  for (let i = 0; i < 20; i++) {
    await flushMicrotasks();
    await nextTick();
    if ((tauriEventMock.handlers.get(eventName)?.length ?? 0) >= count) return;
  }

  throw new Error(`listener count did not reach ${count} for ${eventName}`);
}

async function emitTauriEvent(eventName: string, payload: any) {
  const handlers = [...(tauriEventMock.handlers.get(eventName) ?? [])];
  for (const handler of handlers) {
    await handler({ payload });
  }
  await flushMicrotasks();
  await nextTick();
}

function mountRecordingPopover() {
  const pinia = createPinia();
  setActivePinia(pinia);

  const root = document.createElement('div');
  document.body.appendChild(root);

  const app = createApp(RecordingPopover);
  app.use(pinia);
  app.use(createI18n({
    legacy: false,
    locale: 'en',
    messages: {
      en: {
        main: {
          miniHotkeyPrompt: 'Press {hotkey}',
          errorGeneric: 'Error',
          connecting: 'Connecting',
          listening: 'Listening',
          incomingTranslationEmpty: 'Incoming subtitles will appear here',
          minimize: 'Minimize',
          close: 'Close',
          settings: 'Settings',
        },
        profile: {
          title: 'Profile',
        },
        errors: {
          actions: {
            reconnect: 'Reconnect',
            showDetails: 'Details',
            openSettingsForDevice: 'Open settings',
            activateLicense: 'Activate license',
          },
        },
      },
    },
  }));
  app.directive('ripple', {});
  app.mount(root);

  return {
    unmount: () => {
      app.unmount();
      root.remove();
    },
  };
}

describe('RecordingPopover mini auto-hide e2e', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    tauriEventMock.handlers.clear();
    tauriEventMock.listen.mockReset();
    tauriEventMock.listen.mockImplementation(async (eventName: string, handler: TauriEventHandler) => {
      const handlers = tauriEventMock.handlers.get(eventName) ?? [];
      handlers.push(handler);
      tauriEventMock.handlers.set(eventName, handlers);

      return () => {
        const current = tauriEventMock.handlers.get(eventName) ?? [];
        tauriEventMock.handlers.set(
          eventName,
          current.filter((item) => item !== handler),
        );
      };
    });

    invokeMock.mockReset();
    cursorOverRecordingWindowMock.value = false;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'is_cursor_over_recording_window') {
        return cursorOverRecordingWindowMock.value;
      }
      return null;
    });
    hideWindowMock.mockReset();
    hideWindowMock.mockResolvedValue(undefined);
    appConfigMock.showMiniRecordingWindow = true;
    appConfigMock.playCompletionSound = false;
    appConfigMock.recordingMode = 'dictation';
    appConfigMock.startSync.mockReset();
    appConfigMock.stopSync.mockReset();
    appConfigMock.refresh.mockReset();
    appConfigMock.startSync.mockResolvedValue(undefined);
    appConfigMock.refresh.mockResolvedValue(undefined);
    sttConfigMock.startSync.mockReset();
    sttConfigMock.stopSync.mockReset();
    sttConfigMock.startSync.mockResolvedValue(undefined);
    authMock.initialize.mockReset();
    authMock.initialize.mockResolvedValue(undefined);
    authStoreMock.isAuthenticated = false;

    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
      return window.setTimeout(() => callback(Date.now()), 0);
    });
    vi.spyOn(window, 'cancelAnimationFrame').mockImplementation((id) => {
      window.clearTimeout(id);
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
    document.body.innerHTML = '';
  });

  it('shows mini action buttons only when native cursor is over the mini window', async () => {
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();

    const miniContent = document.querySelector<HTMLElement>('.mini-popover-content');
    expect(miniContent).not.toBeNull();
    expect(document.querySelector('.mini-actions')).not.toBeNull();
    expect(miniContent!.className).toBe('mini-popover-content');

    cursorOverRecordingWindowMock.value = true;
    await vi.advanceTimersByTimeAsync(80);
    await flushMicrotasks();
    await nextTick();
    expect(miniContent!.classList.contains('mini-actions-visible')).toBe(true);

    cursorOverRecordingWindowMock.value = false;
    await vi.advanceTimersByTimeAsync(80);
    await flushMicrotasks();
    await nextTick();
    expect(miniContent!.classList.contains('mini-actions-visible')).toBe(false);

    wrapper.unmount();
  });

  it('cleans pending hotkey debounce timer on unmount', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);

    await emitTauriEvent('hotkey:toggle-recording', {});
    expect(invokeMock).toHaveBeenCalledWith('toggle_recording_with_window');

    wrapper.unmount();

    expect(vi.getTimerCount()).toBe(0);
  });

  it('cleans pending mini open animation frame on unmount', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:window-shown', 1);

    await emitTauriEvent('recording:window-shown', {});
    await flushMicrotasks();
    await nextTick();

    wrapper.unmount();

    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('disposes recording listener if listen resolves after unmount', async () => {
    const pendingListen = deferred<() => void>();
    const unlisten = vi.fn();
    tauriEventMock.listen.mockImplementation((eventName: string, handler: TauriEventHandler) => {
      if (eventName === 'recording:window-shown') {
        return pendingListen.promise;
      }

      const handlers = tauriEventMock.handlers.get(eventName) ?? [];
      handlers.push(handler);
      tauriEventMock.handlers.set(eventName, handlers);

      return Promise.resolve(() => {
        const current = tauriEventMock.handlers.get(eventName) ?? [];
        tauriEventMock.handlers.set(
          eventName,
          current.filter((item) => item !== handler),
        );
      });
    });

    const wrapper = mountRecordingPopover();
    for (
      let i = 0;
      i < 20 && !tauriEventMock.listen.mock.calls.some((call) => call[0] === 'recording:window-shown');
      i++
    ) {
      await flushMicrotasks();
      await nextTick();
    }
    expect(tauriEventMock.listen.mock.calls.some((call) => call[0] === 'recording:window-shown')).toBe(true);

    wrapper.unmount();
    pendingListen.resolve(unlisten);
    await flushMicrotasks();
    await nextTick();

    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(
      tauriEventMock.listen.mock.calls.filter((call) => call[0] === 'recording:window-shown'),
    ).toHaveLength(1);
  });

  it('blurs mini action buttons after click so focus does not stick across reopen', async () => {
    authStoreMock.isAuthenticated = true;
    cursorOverRecordingWindowMock.value = true;

    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await vi.advanceTimersByTimeAsync(80);
    await nextTick();

    const profileButton = document.querySelector<HTMLElement>('.mini-actions .mini-icon-button');
    expect(profileButton).not.toBeNull();
    profileButton!.focus();
    expect(document.activeElement).toBe(profileButton);

    profileButton!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await flushMicrotasks();
    await nextTick();

    expect(document.activeElement).not.toBe(profileButton);
    expect(invokeMock).toHaveBeenCalledWith('show_profile_window', { initialSection: 'none' });
    wrapper.unmount();
  });

  it('keeps the full latest mini transcript and lets overflow fade hide the left edge', async () => {
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();

    const store = useTranscriptionStore();
    const textEl = document.querySelector<HTMLElement>('.mini-transcription-text');
    const textInner = document.querySelector<HTMLElement>('.mini-transcription-text-inner');
    expect(textEl).not.toBeNull();
    expect(textInner).not.toBeNull();

    Object.defineProperty(textEl!, 'scrollWidth', { configurable: true, value: 900 });
    Object.defineProperty(textEl!, 'clientWidth', { configurable: true, value: 120 });

    store.finalText = 'Первое длинное предложение уже распознано и должно оставаться частью мини текста.';
    store.accumulatedText = 'Вторая часть тоже не должна пропадать после обновления сегмента.';
    store.partialText = 'А должно показывать весь последний текст без программного обрезания до восемнадцати слов.';

    await nextTick();
    await vi.advanceTimersByTimeAsync(0);
    await nextTick();

    const visibleText = textInner!.textContent?.trim() ?? '';
    expect(visibleText).toContain('Первое длинное предложение');
    expect(visibleText).toContain('Вторая часть тоже не должна пропадать');
    expect(visibleText).toContain('А должно показывать весь последний текст');
    expect(visibleText.split(/\s+/).length).toBeGreaterThan(18);
    expect(textEl!.classList.contains('overflowing')).toBe(true);

    wrapper.unmount();
  });

  it('shows incoming subtitles in mini mode instead of the hotkey prompt', async () => {
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();

    const store = useTranscriptionStore();
    const textInner = document.querySelector<HTMLElement>('.mini-transcription-text-inner');
    const statusDot = document.querySelector<HTMLElement>('.mini-status-dot');
    expect(textInner).not.toBeNull();
    expect(statusDot).not.toBeNull();

    store.incomingTranslationStatus = RecordingStatus.Recording;
    await nextTick();
    expect(textInner!.textContent?.trim()).toBe('Incoming subtitles will appear here');
    expect(textInner!.textContent).not.toContain('Press');
    expect(statusDot!.classList.contains('recording')).toBe(true);

    store.incomingTranslationText = 'перевод собеседника';
    await nextTick();
    expect(textInner!.textContent?.trim()).toBe('перевод собеседника');

    store.incomingTranslationError = 'temporary incoming translation failure';
    await nextTick();
    expect(textInner!.textContent?.trim()).toBe('temporary incoming translation failure');
    expect(statusDot!.classList.contains('error')).toBe(true);

    wrapper.unmount();
  });

  it('keeps the mini window visible when dictation stops while incoming subtitles are active', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 2);

    const store = useTranscriptionStore();
    store.incomingTranslationStatus = RecordingStatus.Recording;
    await nextTick();

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    hideWindowMock.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).not.toHaveBeenCalled();
    expect(document.querySelector('.mini-transcription-text-inner')?.textContent).toContain(
      'Incoming subtitles will appear here',
    );

    wrapper.unmount();
  });

  it('cancels pending mini hide if incoming subtitles become visible before the timeout fires', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 2);

    await emitTauriEvent('recording:status', {
      session_id: 51,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    hideWindowMock.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 51,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });

    const store = useTranscriptionStore();
    store.incomingTranslationStatus = RecordingStatus.Recording;
    store.incomingTranslationText = 'late incoming subtitle';
    await nextTick();
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).not.toHaveBeenCalled();
    expect(document.querySelector('.mini-transcription-text-inner')?.textContent).toContain(
      'late incoming subtitle',
    );

    wrapper.unmount();
  });

  it('shows mini error text with retry and details actions', async () => {
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();

    const store = useTranscriptionStore();
    const textEl = document.querySelector<HTMLElement>('.mini-transcription-text');
    const textInner = document.querySelector<HTMLElement>('.mini-transcription-text-inner');
    expect(textEl).not.toBeNull();
    expect(textInner).not.toBeNull();

    Object.defineProperty(textEl!, 'scrollWidth', { configurable: true, value: 720 });
    Object.defineProperty(textEl!, 'clientWidth', { configurable: true, value: 110 });

    const message = 'Connection problem. Check your internet and try again.';
    store.status = RecordingStatus.Error;
    store.error = message;
    store.errorType = 'connection';
    await nextTick();
    await vi.advanceTimersByTimeAsync(0);
    await nextTick();

    expect(textInner!.textContent?.trim()).toBe(message);
    expect(textEl!.classList.contains('error')).toBe(true);
    expect(textEl!.classList.contains('placeholder')).toBe(false);
    expect(textEl!.classList.contains('overflowing')).toBe(true);
    expect(textEl!.scrollLeft).toBe(0);
    expect(document.querySelector('.mini-popover-content')?.classList.contains('mini-actions-visible')).toBe(true);

    invokeMock.mockClear();
    document.querySelector<HTMLElement>('[data-testid="mini-error-details"]')!.click();
    await flushMicrotasks();

    expect(invokeMock).toHaveBeenCalledWith('show_error_details_window', {
      summary: message,
      details: expect.stringContaining('Type: connection'),
    });

    const reconnectSpy = vi.spyOn(store, 'reconnect').mockResolvedValue(undefined);
    document.querySelector<HTMLButtonElement>('[data-testid="mini-error-retry"]')!.click();
    await flushMicrotasks();

    expect(reconnectSpy).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('shows the listening placeholder immediately for a Rust-owned hotkey start', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:start-requested', 1);

    const store = useTranscriptionStore();
    store.finalText = 'Old transcript that must not flash when the mini window opens again.';
    await nextTick();

    expect(document.querySelector('.mini-transcription-text-inner')?.textContent).toContain(
      'Old transcript',
    );

    await emitTauriEvent('recording:start-requested', {
      source: 'hotkey',
      warmStartExpected: false,
    });

    const miniText = document.querySelector('.mini-transcription-text-inner')?.textContent ?? '';
    const statusDot = document.querySelector<HTMLElement>('.mini-status-dot');
    expect(miniText).toContain('Listening');
    expect(miniText).not.toContain('Old transcript');
    expect(store.isStarting).toBe(true);
    expect(statusDot?.classList.contains('recording')).toBe(true);
    expect(statusDot?.classList.contains('starting')).toBe(false);

    wrapper.unmount();
  });

  it('does not hide the mini window from a stale Idle after a newer Recording session', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 2);

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    hideWindowMock.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 41,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).not.toHaveBeenCalled();

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('does not hide the mini window from an Idle event without a valid session id', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 2);

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    hideWindowMock.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 0,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).not.toHaveBeenCalled();

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('suppresses an old Idle while Rust-owned hotkey start is still pending', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 2);
    await waitForListenerCount('recording:start-requested', 1);

    await emitTauriEvent('recording:start-requested', {
      source: 'hotkey',
      warmStartExpected: false,
    });

    hideWindowMock.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 7,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).not.toHaveBeenCalled();

    await emitTauriEvent('recording:status', {
      session_id: 8,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await emitTauriEvent('recording:status', {
      session_id: 8,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });
});
