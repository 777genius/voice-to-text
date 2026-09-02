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
const openExternalUrlMock = vi.hoisted(() => vi.fn());
const hideWindowMock = vi.hoisted(() => vi.fn());
const outerPositionMock = vi.hoisted(() => vi.fn());
const nativeWindowEpoch = vi.hoisted(() => ({ value: 1 }));
const cursorOverRecordingWindowMock = vi.hoisted(() => ({ value: false }));
const windowInnerSizeMock = vi.hoisted(() => ({ width: 248, height: 62 }));
const resizeObserverMock = vi.hoisted(() => ({
  callbacks: [] as ResizeObserverCallback[],
}));

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

vi.mock('@tauri-apps/plugin-shell', () => ({
  open: (...args: any[]) => openExternalUrlMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => tauriEventMock.listen(...args),
}));

vi.mock('@tauri-apps/api/webviewWindow', () => ({
  getCurrentWebviewWindow: () => ({
    hide: hideWindowMock,
    innerSize: vi.fn().mockImplementation(async () => ({ ...windowInnerSizeMock })),
    outerPosition: outerPositionMock,
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
  if (['recording:start-requested', 'recording:window-shown', 'recording:window-will-hide-for-hotkey-stop'].includes(eventName)) {
    payload = { windowEpoch: nativeWindowEpoch.value, ...payload };
  }
  const handlers = [...(tauriEventMock.handlers.get(eventName) ?? [])];
  for (const handler of handlers) {
    await handler({ payload });
  }
  await flushMicrotasks();
  await nextTick();
}

async function emitFullContentResize() {
  const resizeCallCount = invokeMock.mock.calls.filter(
    ([command]) => command === 'set_recording_window_size',
  ).length;
  for (const callback of resizeObserverMock.callbacks) {
    callback([], {} as ResizeObserver);
  }
  for (let attempt = 0; attempt < 20; attempt++) {
    await flushMicrotasks();
    await nextTick();
    const nextCount = invokeMock.mock.calls.filter(
      ([command]) => command === 'set_recording_window_size',
    ).length;
    if (nextCount > resizeCallCount) return;
  }
}

function lastRecordingWindowResizeCall() {
  const calls = invokeMock.mock.calls.filter(
    ([command]) => command === 'set_recording_window_size',
  );
  return calls[calls.length - 1];
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
          app: {
            title: 'VoicetextAI',
          },
          main: {
          support: 'Support',
          miniHotkeyPrompt: 'Press {hotkey}',
          errorGeneric: 'Error',
          connecting: 'Connecting',
          listening: 'Listening',
          incomingTranslationEmpty: 'Incoming subtitles will appear here',
          incomingTranslation: 'Incoming translation',
          incomingTranslationMute: 'Mute translated audio',
            incomingTranslationUnmute: 'Unmute translated audio',
            incomingDuplexHeadsetWarning: 'Use headphones while translating both sides of a call.',
            incomingTranslationStart: 'Start incoming translation',
            incomingTranslationStop: 'Stop incoming translation',
            healthCheckStart: 'Run health check',
            healthCheck: 'Health check',
            hotkeyHint: 'Hotkey: {hotkey}',
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
    resizeObserverMock.callbacks.length = 0;
    vi.stubGlobal('ResizeObserver', class {
      constructor(callback: ResizeObserverCallback) {
        resizeObserverMock.callbacks.push(callback);
      }

      observe() {}
      unobserve() {}
      disconnect() {}
    });
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
    openExternalUrlMock.mockReset();
    openExternalUrlMock.mockResolvedValue(undefined);
    nativeWindowEpoch.value = 1;
    cursorOverRecordingWindowMock.value = false;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'get_recording_window_epoch') return nativeWindowEpoch.value;
      if (command === 'hide_recording_window_if_current') {
        await hideWindowMock();
        return true;
      }
      if (command === 'is_cursor_over_recording_window') {
        return cursorOverRecordingWindowMock.value;
      }
      return null;
    });
    outerPositionMock.mockReset();
    outerPositionMock.mockResolvedValue({ x: 100, y: 100 });
    hideWindowMock.mockReset();
    hideWindowMock.mockResolvedValue(undefined);
    appConfigMock.showMiniRecordingWindow = true;
    windowInnerSizeMock.width = 248;
    windowInnerSizeMock.height = 62;
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
    vi.unstubAllGlobals();
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

  it('opens the support issue tracker from the mini actions', async () => {
    cursorOverRecordingWindowMock.value = true;

    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await vi.advanceTimersByTimeAsync(80);
    await nextTick();

    const supportButton = document.querySelector<HTMLButtonElement>('[data-testid="mini-support"]');
    expect(supportButton).not.toBeNull();
    supportButton!.click();
    await flushMicrotasks();

    expect(openExternalUrlMock).toHaveBeenCalledOnce();
    expect(openExternalUrlMock).toHaveBeenCalledWith('https://github.com/777genius/voice-to-text/issues');
    wrapper.unmount();
  });

  it('does not replay the mini opening animation for a duplicate shown event in the same epoch', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:window-shown', 1);

    await emitTauriEvent('recording:window-shown', {});
    await vi.advanceTimersByTimeAsync(520);
    expect(document.querySelector('.mini-opening')).toBeNull();

    await emitTauriEvent('recording:window-shown', {});
    await vi.advanceTimersByTimeAsync(1);
    expect(document.querySelector('.mini-opening')).toBeNull();

    nativeWindowEpoch.value = 2;
    await emitTauriEvent('recording:window-shown', {});
    await vi.advanceTimersByTimeAsync(1);
    expect(document.querySelector('.mini-opening')).not.toBeNull();
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

  it('shows a session-scoped mute control only for active spoken incoming translation', async () => {
    appConfigMock.showMiniRecordingWindow = false;
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === 'set_incoming_translation_muted') {
        expect(args).toEqual({ muted: true });
        return { session_id: 901, state: 'playing', muted: true };
      }
      if (command === 'is_cursor_over_recording_window') return false;
      return null;
    });
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();
    const store = useTranscriptionStore();

    store.incomingTranslationStatus = RecordingStatus.Recording;
    store.incomingTranslationSessionId = 901;
    store.incomingTranslationDelivery = 'captions_only';
    await nextTick();
    expect(document.querySelector('[data-testid="incoming-translation-mute"]')).toBeNull();

    store.incomingTranslationDelivery = 'text_and_audio';
    await nextTick();
    const mute = document.querySelector<HTMLButtonElement>(
      '[data-testid="incoming-translation-mute"]',
    );
    expect(mute).not.toBeNull();
    expect(mute!.title).toBe('Mute translated audio');
    mute!.click();
    await flushMicrotasks();
    await nextTick();

    expect(invokeMock).toHaveBeenCalledWith('set_incoming_translation_muted', { muted: true });
    expect(store.incomingTranslationMuted).toBe(true);
    expect(mute!.querySelector('.mdi')?.classList.contains('mdi-volume-off')).toBe(true);
    wrapper.unmount();
  });

  it('shows the headset warning only while spoken incoming and outgoing translation overlap', async () => {
    appConfigMock.showMiniRecordingWindow = false;
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();
    const store = useTranscriptionStore();

    store.incomingTranslationStatus = RecordingStatus.Recording;
    store.incomingTranslationDelivery = 'text_and_audio';
    store.activeRecordingMode = 'live_translation';
    store.status = RecordingStatus.Recording;
    await nextTick();
    expect(document.querySelector('[data-testid="incoming-duplex-headset-warning"]')).not.toBeNull();

    store.status = RecordingStatus.Idle;
    await nextTick();
    expect(document.querySelector('[data-testid="incoming-duplex-headset-warning"]')).toBeNull();
    wrapper.unmount();
  });

  it('resizes full layout from the complete translation stack and caps oversized content', async () => {
    appConfigMock.showMiniRecordingWindow = false;
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();
    const store = useTranscriptionStore();

    store.activeRecordingMode = 'live_translation';
    store.translationText = 'Outgoing translation';
    store.incomingTranslationStatus = RecordingStatus.Recording;
    store.incomingTranslationText = 'Incoming translated sentence';
    store.liveTranslationHealthCheck = {
      ok: true,
      checked_at_ms: Date.now(),
      items: [{ id: 'route', label: 'Audio route', ok: true, required: true, message: 'Ready' }],
    };
    await nextTick();

    const stack = document.querySelector<HTMLElement>('.full-transcription-stack');
    expect(stack).not.toBeNull();
    expect(document.querySelector('[data-testid="incoming-translation-panel"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="translation-health-panel"]')).not.toBeNull();

    Object.defineProperty(stack!, 'scrollHeight', { configurable: true, value: 260 });
    invokeMock.mockClear();
    await emitFullContentResize();
    expect(lastRecordingWindowResizeCall())
      .toEqual(['set_recording_window_size', { width: 460, height: 476 }]);

    Object.defineProperty(stack!, 'scrollHeight', { configurable: true, value: 900 });
    await emitFullContentResize();
    expect(lastRecordingWindowResizeCall())
      .toEqual(['set_recording_window_size', { width: 460, height: 700 }]);

    store.incomingTranslationStatus = RecordingStatus.Idle;
    store.incomingTranslationText = '';
    store.liveTranslationHealthCheck = null;
    await nextTick();
    Object.defineProperty(stack!, 'scrollHeight', { configurable: true, value: 80 });
    await emitFullContentResize();
    expect(lastRecordingWindowResizeCall())
      .toEqual(['set_recording_window_size', { width: 460, height: 330 }]);
    wrapper.unmount();
  });

  it('keeps mini layout fixed while both translation directions have content', async () => {
    windowInnerSizeMock.width = 460;
    windowInnerSizeMock.height = 330;
    const wrapper = mountRecordingPopover();
    await flushMicrotasks();
    await nextTick();
    const store = useTranscriptionStore();

    store.activeRecordingMode = 'live_translation';
    store.translationText = 'Outgoing translation';
    store.incomingTranslationStatus = RecordingStatus.Recording;
    store.incomingTranslationText = 'Incoming translation';
    store.status = RecordingStatus.Recording;
    await nextTick();
    await flushMicrotasks();

    const resizeCalls = invokeMock.mock.calls.filter(
      ([command]) => command === 'set_recording_window_size',
    );
    expect(resizeCalls.length).toBeGreaterThan(0);
    expect(resizeCalls.every(([, size]) => size.width === 248 && size.height === 62)).toBe(true);
    expect(document.querySelector('.full-transcription-stack')).toBeNull();
    wrapper.unmount();
  });

  it('keeps the mini window visible when dictation stops while incoming subtitles are active', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);

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
    await waitForListenerCount('hotkey:toggle-recording', 1);

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
    await waitForListenerCount('hotkey:toggle-recording', 1);

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
    await waitForListenerCount('hotkey:toggle-recording', 1);

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

  it('starts finalizing and hides only for Processing from the current session', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    hideWindowMock.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 41,
      status: RecordingStatus.Processing,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).not.toHaveBeenCalled();
    expect(useTranscriptionStore().status).toBe(RecordingStatus.Recording);

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Processing,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    expect(useTranscriptionStore().isProcessing).toBe(true);
    expect(document.querySelector('.mini-status-dot')?.classList.contains('processing')).toBe(true);

    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('does not let stale start-like events cancel the current session hide', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Processing,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    await emitTauriEvent('recording:status', {
      session_id: 41,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('retries on Idle when the Processing hide failed', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    hideWindowMock.mockRejectedValueOnce(new Error('temporary hide failure'));

    await emitTauriEvent('recording:status', {
      session_id: 62,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await emitTauriEvent('recording:status', {
      session_id: 62,
      status: RecordingStatus.Processing,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).toHaveBeenCalledTimes(1);

    await emitTauriEvent('recording:status', {
      session_id: 62,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it('accepts one late final during Processing and does not hide again on Idle', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await waitForListenerCount('transcription:final', 1);

    await emitTauriEvent('recording:status', {
      session_id: 52,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await emitTauriEvent('recording:status', {
      session_id: 52,
      status: RecordingStatus.Processing,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).toHaveBeenCalledTimes(1);

    const lateFinal = {
      session_id: 52,
      text: 'late clean final',
      timestamp: 3,
      start: 1,
      duration: 0.8,
    };
    await emitTauriEvent('transcription:final', lateFinal);
    await emitTauriEvent('transcription:final', { ...lateFinal, timestamp: 4 });

    expect(useTranscriptionStore().finalText).toBe('late clean final');

    await emitTauriEvent('recording:status', {
      session_id: 52,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('hides the mini window before suppressing completed text', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);

    const store = useTranscriptionStore();
    const suppressSpy = vi.spyOn(store, 'suppressPreviousTranscriptionDisplay');
    store.finalText = 'Completed phrase that must stay visible until the window is hidden.';

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Recording,
      stopped_via_hotkey: false,
      mode: 'dictation',
    });

    hideWindowMock.mockClear();
    suppressSpy.mockClear();

    await emitTauriEvent('recording:status', {
      session_id: 42,
      status: RecordingStatus.Idle,
      stopped_via_hotkey: false,
      mode: null,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    expect(suppressSpy).toHaveBeenCalledWith('auto_hide:mini window recording stopped');
    expect(hideWindowMock.mock.invocationCallOrder[0]).toBeLessThan(
      suppressSpy.mock.invocationCallOrder[0],
    );
    wrapper.unmount();
  });

  it('does not hide the mini window from an Idle event without a valid session id', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);

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
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await waitForListenerCount('hotkey:toggle-recording', 1);

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
  it.each(['recording:start-requested', 'recording:window-shown'])(
    'discards delayed close geometry after %s', async (restartEvent) => {
      const wrapper = mountRecordingPopover();
      await waitForListenerCount('hotkey:toggle-recording', 1);
      const geometry = deferred<{ x: number; y: number }>();
      outerPositionMock.mockReturnValueOnce(geometry.promise);
      await emitTauriEvent('recording:status', { session_id: 80, status: 'Recording' });
      await emitTauriEvent('recording:status', { session_id: 80, status: 'Processing' });
      await emitTauriEvent(restartEvent, {});
      geometry.resolve({ x: 100, y: 100 });
      await flushMicrotasks();
      await nextTick();
      expect(document.querySelector('.mini-closing')).toBeNull();
      await vi.advanceTimersByTimeAsync(500);
      expect(hideWindowMock).not.toHaveBeenCalled();
      wrapper.unmount();
    },
  );

  it('does not suppress new transcript when an old hide IPC completes', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const pendingHide = deferred<void>();
    hideWindowMock.mockReturnValueOnce(pendingHide.promise);
    await emitTauriEvent('recording:status', { session_id: 80, status: 'Recording' });
    await emitTauriEvent('recording:status', { session_id: 80, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    await emitTauriEvent('recording:start-requested', {});
    await emitTauriEvent('recording:status', { session_id: 81, status: 'Recording' });
    const store = useTranscriptionStore();
    store.finalText = 'New session speech';
    const suppress = vi.spyOn(store, 'suppressPreviousTranscriptionDisplay');
    pendingHide.resolve();
    await flushMicrotasks();
    await nextTick();
    expect(suppress).not.toHaveBeenCalled();
    expect(document.querySelector('.mini-transcription-text-inner')?.textContent).toContain('New session speech');
    wrapper.unmount();
  });

  it.each(['Processing', 'Idle'])('ignores old %s throughout a pending start, including after five seconds', async (status) => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:start-requested', {});
    await vi.advanceTimersByTimeAsync(6000);
    await emitTauriEvent('recording:status', { session_id: 80, status });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).not.toHaveBeenCalled();
    expect(useTranscriptionStore().isStarting).toBe(true);
    wrapper.unmount();
  });

  it('keeps terminally closed session statuses from hiding the error window', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 80, status: 'Recording' });
    await emitTauriEvent('transcription:error', {
      session_id: 80, error: 'Invalid configuration', error_type: 'configuration',
    });
    await emitTauriEvent('recording:status', { session_id: 80, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).not.toHaveBeenCalled();
    expect(useTranscriptionStore().hasError).toBe(true);
    wrapper.unmount();
  });

  it.each(['before-shown', 'during-epoch-validation', 'during-status-snapshot'])(
    'keeps the current failed-start error visible when it arrives %s', async (order) => {
      const wrapper = mountRecordingPopover();
      await waitForListenerCount('hotkey:toggle-recording', 1);
      const store = useTranscriptionStore();
      const epoch = deferred<number>();
      const snapshot = deferred<string>();
      const defaultInvoke = invokeMock.getMockImplementation()!;
      let statusRequested = false;
      invokeMock.mockImplementation((command: string, ...args: any[]) => {
        if (command === 'get_recording_window_epoch' && order === 'during-epoch-validation') return epoch.promise;
        if (command === 'get_recording_status') {
          statusRequested = true;
          return order === 'during-status-snapshot' ? snapshot.promise : Promise.resolve('Idle');
        }
        return defaultInvoke(command, ...args);
      });
      const failStart = async () => {
        await emitTauriEvent('recording:status', { session_id: 31, status: 'Starting' });
        await emitTauriEvent('transcription:error', {
          session_id: 31, error: 'Connection error: Native fixture failed start', error_type: 'connection',
        });
        expect(store.status).toBe('Error');
        expect(store.errorFullText).toContain('Connection error: Native fixture failed start');
      };
      if (order === 'before-shown') await failStart();
      const shown = tauriEventMock.handlers.get('recording:window-shown')![0]({ payload: { windowEpoch: 1 } });
      if (order === 'during-status-snapshot') {
        for (let i = 0; i < 20 && !statusRequested; i++) await flushMicrotasks();
        expect(statusRequested).toBe(true);
      }
      if (order !== 'before-shown') await failStart();
      epoch.resolve(1);
      snapshot.resolve('Idle');
      await shown;
      await nextTick();
      await vi.advanceTimersByTimeAsync(500);
      expect(store.status).toBe('Error');
      expect(store.errorFullText).toContain('Connection error: Native fixture failed start');
      expect(document.querySelector('[data-testid="mini-error-retry"]')).not.toBeNull();
      expect(hideWindowMock).not.toHaveBeenCalled();
      wrapper.unmount();
    },
  );

  it('clears a previous error when the next native recording actually starts', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 31, status: 'Starting' });
    await emitTauriEvent('transcription:error', {
      session_id: 31, error: 'Connection error: Native fixture failed start', error_type: 'connection',
    });
    expect(useTranscriptionStore().hasError).toBe(true);
    nativeWindowEpoch.value = 2;
    await emitTauriEvent('recording:start-requested', {});
    await emitTauriEvent('recording:status', { session_id: 32, status: 'Recording' });
    expect(useTranscriptionStore().status).toBe('Recording');
    expect(useTranscriptionStore().error).toBeNull();
    expect(document.querySelector('[data-testid="mini-error-retry"]')).toBeNull();
    wrapper.unmount();
  });

  it('does not run an opening frame after a stop supersedes window shown', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 80, status: 'Recording' });
    await emitTauriEvent('recording:window-shown', {});
    const geometry = deferred<{ x: number; y: number }>();
    outerPositionMock.mockReturnValueOnce(geometry.promise);
    await emitTauriEvent('recording:status', { session_id: 80, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(1);
    expect(document.querySelector('.mini-opening')).toBeNull();
    wrapper.unmount();
    geometry.resolve({ x: 100, y: 100 });
    await flushMicrotasks();
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each(['recording:window-shown', 'recording:start-requested', 'recording:window-will-hide-for-hotkey-stop'])(
    'rejects a queued %s event from an earlier native window epoch', async (eventName) => {
      const wrapper = mountRecordingPopover();
      await waitForListenerCount('hotkey:toggle-recording', 1);
      await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
      const store = useTranscriptionStore();
      store.finalText = 'Current session';
      const suppress = vi.spyOn(store, 'suppressPreviousTranscriptionDisplay');
      nativeWindowEpoch.value = 2;
      await emitTauriEvent(eventName, { windowEpoch: 1 });
      await vi.advanceTimersByTimeAsync(500);
      expect(store.sessionId).toBe(90);
      expect(store.finalText).toBe('Current session');
      expect(suppress).not.toHaveBeenCalled();
      expect(document.querySelector('.mini-closing')).toBeNull();
      expect(hideWindowMock).not.toHaveBeenCalled();
      wrapper.unmount();
    },
  );

  it('does not suppress text when native rejects a hide for an obsolete window epoch', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    const store = useTranscriptionStore();
    const suppress = vi.spyOn(store, 'suppressPreviousTranscriptionDisplay');
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, args?: { windowEpoch?: number }) => {
      if (command === 'hide_recording_window_if_current') {
        expect(args).toEqual({ windowEpoch: 1 });
        return Promise.resolve(false);
      }
      return defaultInvoke(command, args);
    });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    nativeWindowEpoch.value = 2;
    await vi.advanceTimersByTimeAsync(500);
    expect(suppress).not.toHaveBeenCalled();
    expect(hideWindowMock).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('cancels pending hide when a direct UI start changes store state', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    useTranscriptionStore().status = RecordingStatus.Starting;
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).not.toHaveBeenCalled();
    expect(document.querySelector('.mini-closing')).toBeNull();
    wrapper.unmount();
  });

  it('cancels an old run hide while the backend owns a pending replacement start', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:intent-projection', 1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    expect(document.querySelector('.mini-closing')).not.toBeNull();

    await emitTauriEvent('recording:intent-projection', {
      runId: 90,
      intentRevision: 91,
      status: 'Processing',
      desiredOn: true,
      pendingStart: true,
      processingJobs: 0,
      shutdownRequested: false,
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(document.querySelector('.mini-closing')).toBeNull();
    expect(hideWindowMock).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('retains the current native epoch when Recording arrives before the start event query resolves', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const query = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return query.promise;
      return defaultInvoke(command, ...args);
    });
    const startHandler = tauriEventMock.handlers.get('recording:start-requested')![0];
    nativeWindowEpoch.value = 2;
    const start = startHandler({ payload: { windowEpoch: 2 } });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    useTranscriptionStore().finalText = 'Speech already arrived';
    query.resolve(2);
    await start;
    expect(useTranscriptionStore().sessionId).toBe(90);
    expect(useTranscriptionStore().finalText).toBe('Speech already arrived');
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(invokeMock).toHaveBeenCalledWith('hide_recording_window_if_current', { windowEpoch: 2 });
    wrapper.unmount();
  });

  it('never restores an older native epoch when window event queries complete out of order', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const oldQuery = deferred<number>();
    const newQuery = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    let queries = 0;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return ++queries === 1 ? oldQuery.promise : newQuery.promise;
      return defaultInvoke(command, ...args);
    });
    const handler = tauriEventMock.handlers.get('recording:start-requested')![0];
    const oldStart = handler({ payload: { windowEpoch: 2 } });
    const newStart = handler({ payload: { windowEpoch: 3 } });
    newQuery.resolve(3);
    await newStart;
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    useTranscriptionStore().finalText = 'Current recording';
    oldQuery.resolve(2);
    await oldStart;
    expect(useTranscriptionStore().sessionId).toBe(90);
    expect(useTranscriptionStore().finalText).toBe('Current recording');
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(invokeMock).toHaveBeenCalledWith('hide_recording_window_if_current', { windowEpoch: 3 });
    wrapper.unmount();
  });

  it.each(['shown-first', 'start-first'])('accepts compatible same-epoch start and shown events: %s', async (order) => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 89, status: 'Recording' });
    const startQuery = deferred<number>();
    const shownQuery = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    let queries = 0;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return ++queries === 1 ? startQuery.promise : shownQuery.promise;
      if (command === 'get_recording_status') return Promise.resolve('Idle');
      return defaultInvoke(command, ...args);
    });
    const start = tauriEventMock.handlers.get('recording:start-requested')![0]({ payload: { windowEpoch: 2 } });
    const shown = tauriEventMock.handlers.get('recording:window-shown')![0]({ payload: { windowEpoch: 2 } });
    if (order === 'shown-first') {
      shownQuery.resolve(2);
      await shown;
    } else {
      startQuery.resolve(2);
      await start;
    }
    await emitTauriEvent('recording:status', { session_id: 89, status: 'Idle' });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).not.toHaveBeenCalled();
    startQuery.resolve(2);
    shownQuery.resolve(2);
    await Promise.all([start, shown]);
    expect(useTranscriptionStore().isStarting).toBe(true);
    await emitTauriEvent('recording:status', { session_id: 89, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).not.toHaveBeenCalled();
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    expect(useTranscriptionStore().sessionId).toBe(90);
    wrapper.unmount();
  });

  it('keeps the current opening frame when local Starting cancels only pending hide', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:window-shown', {});
    useTranscriptionStore().status = RecordingStatus.Starting;
    await vi.advanceTimersByTimeAsync(1);
    await nextTick();
    expect(document.querySelector('.mini-animation-reset')).toBeNull();
    expect(document.querySelector('.mini-opening')).not.toBeNull();
    wrapper.unmount();
  });

  it.each(['Starting', 'Recording', 'same-epoch-start'])(
    'preserves the current shown opening through %s before its animation frame', async (event) => {
      const wrapper = mountRecordingPopover();
      await waitForListenerCount('hotkey:toggle-recording', 1);
      await emitTauriEvent('recording:window-shown', {});
      expect(document.querySelector('.mini-animation-reset')).not.toBeNull();
      if (event === 'same-epoch-start') {
        await emitTauriEvent('recording:start-requested', {});
      } else {
        await emitTauriEvent('recording:status', { session_id: 90, status: event });
      }
      await vi.advanceTimersByTimeAsync(1);
      await nextTick();
      expect(document.querySelector('.mini-animation-reset')).toBeNull();
      expect(document.querySelector('.mini-opening')).not.toBeNull();
      await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
      await nextTick();
      expect(document.querySelector('.mini-opening')).not.toBeNull();
      expect(document.querySelector('.mini-closing')).toBeNull();
      expect(hideWindowMock).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(520);
      expect(document.querySelector('.mini-opening')).toBeNull();
      wrapper.unmount();
      expect(vi.getTimerCount()).toBe(0);
    },
  );

  it('discards old close geometry when a newer visibility epoch arrives without a start', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    const geometry = deferred<{ x: number; y: number }>();
    outerPositionMock.mockReturnValueOnce(geometry.promise);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    nativeWindowEpoch.value = 2;
    await emitTauriEvent('recording:start-cancelled', { startWindowEpoch: 0, windowEpoch: 2 });
    geometry.resolve({ x: 100, y: 100 });
    await flushMicrotasks();
    await nextTick();
    expect(document.querySelector('.mini-closing')).toBeNull();
    wrapper.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('invalidates an older opening frame when a newer native start epoch is accepted', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:window-shown', {});
    nativeWindowEpoch.value = 2;
    await emitTauriEvent('recording:start-requested', {});
    await vi.advanceTimersByTimeAsync(1);
    expect(document.querySelector('.mini-opening')).toBeNull();
    expect(document.querySelector('.mini-animation-reset')).toBeNull();
    await emitTauriEvent('recording:window-shown', {});
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    await vi.advanceTimersByTimeAsync(1);
    expect(document.querySelector('.mini-opening')).not.toBeNull();
    wrapper.unmount();
  });

  it('does not reset an accepted session for a duplicate start event in the same epoch', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:start-requested', {});
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    useTranscriptionStore().finalText = 'Current speech';
    await emitTauriEvent('recording:start-requested', {});
    expect(useTranscriptionStore().sessionId).toBe(90);
    expect(useTranscriptionStore().finalText).toBe('Current speech');
    wrapper.unmount();
  });

  it.each([1, 2])('settles a released provisional start after window epoch advances to %s', async (windowEpoch) => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_status') return Promise.resolve('Idle');
      return defaultInvoke(command, ...args);
    });
    const registrations = tauriEventMock.listen.mock.calls.map(([event]) => event);
    expect(registrations.indexOf('recording:start-cancelled')).toBeLessThan(registrations.indexOf('recording:start-requested'));
    await emitTauriEvent('recording:start-requested', {});
    const store = useTranscriptionStore();
    expect(store.isStarting).toBe(true);
    nativeWindowEpoch.value = windowEpoch;
    await emitTauriEvent('recording:start-cancelled', { startWindowEpoch: 1, windowEpoch });
    expect(store.status).toBe('Idle');
    await store.reconcileBackendStatus('cancelled_start');
    expect(store.status).toBe('Idle');
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'stop_recording')).toBe(false);
    wrapper.unmount();
  });

  it('does not resurrect a start whose epoch validation completes after cancellation', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const query = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    let queries = 0;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch' && ++queries === 1) return query.promise;
      return defaultInvoke(command, ...args);
    });
    const start = tauriEventMock.handlers.get('recording:start-requested')![0]({ payload: { windowEpoch: 1 } });
    await emitTauriEvent('recording:start-cancelled', { startWindowEpoch: 1, windowEpoch: 1 });
    query.resolve(1);
    await start;
    expect(useTranscriptionStore().status).toBe('Idle');
    wrapper.unmount();
  });

  it.each(['native-start', 'ui-start', 'recording'])('ignores delayed provisional cancellation after a newer %s', async (successor) => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:start-requested', {});
    const store = useTranscriptionStore();
    const query = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    let queries = 0;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch' && ++queries === 1) return query.promise;
      if (command === 'start_recording') return new Promise(() => {});
      return defaultInvoke(command, ...args);
    });
    const cancellation = tauriEventMock.handlers.get('recording:start-cancelled')![0]({ payload: { startWindowEpoch: 1, windowEpoch: 1 } });
    if (successor === 'native-start') {
      nativeWindowEpoch.value = 2;
      await emitTauriEvent('recording:start-requested', {});
    } else if (successor === 'ui-start') {
      void store.startRecording();
      await flushMicrotasks();
    } else {
      await emitTauriEvent('recording:status', { session_id: 91, status: 'Recording' });
      store.finalText = 'Successor speech';
    }
    query.resolve(nativeWindowEpoch.value);
    await cancellation;
    expect(store.status).toBe(successor === 'recording' ? 'Recording' : 'Starting');
    if (successor === 'recording') expect(store.finalText).toBe('Successor speech');
    wrapper.unmount();
  });

  it('settles a cancelled start even when a later tray show advances only visibility', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_status') return Promise.resolve('Idle');
      return defaultInvoke(command, ...args);
    });
    await emitTauriEvent('recording:start-requested', {});
    nativeWindowEpoch.value = 2;
    await emitTauriEvent('recording:window-shown', {});
    expect(useTranscriptionStore().isStarting).toBe(true);
    await emitTauriEvent('recording:start-cancelled', { startWindowEpoch: 1, windowEpoch: 1 });
    expect(useTranscriptionStore().status).toBe('Idle');
    wrapper.unmount();
  });

  it('settles the original owner when a second provisional start is cancelled before validation', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:start-requested', {});
    // The second provisional show advances visibility before its start event can be accepted.
    nativeWindowEpoch.value = 3;
    await emitTauriEvent('recording:start-requested', { windowEpoch: 2 });
    await emitTauriEvent('recording:start-cancelled', { startWindowEpoch: 2, windowEpoch: 3 });
    expect(useTranscriptionStore().isStarting).toBe(true);
    await emitTauriEvent('recording:start-cancelled', { startWindowEpoch: 1, windowEpoch: 3 });
    expect(useTranscriptionStore().status).toBe('Idle');
    wrapper.unmount();
  });

  it('uses the synchronized duplicate-start epoch to hide the existing session window', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_status') return Promise.resolve('Recording');
      return defaultInvoke(command, ...args);
    });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    const store = useTranscriptionStore();
    store.finalText = 'Current speech';
    nativeWindowEpoch.value = 2;
    // Native's busy-start branch publishes existing status plus window synchronization.
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    await emitTauriEvent('recording:window-shown', {});
    expect(store.finalText).toBe('Current speech');
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(invokeMock).toHaveBeenCalledWith('hide_recording_window_if_current', { windowEpoch: 2 });
    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it('discards close geometry that resolves after the hide has already completed', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const geometry = deferred<{ x: number; y: number }>();
    outerPositionMock.mockReturnValueOnce(geometry.promise);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Processing' });
    await vi.advanceTimersByTimeAsync(500);
    expect(hideWindowMock).toHaveBeenCalledTimes(1);
    geometry.resolve({ x: 100, y: 100 });
    await flushMicrotasks();
    await nextTick();
    expect(document.querySelector('.mini-closing')).toBeNull();
    wrapper.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('lets a newer shown event reverse an older close that finishes validation first', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    const store = useTranscriptionStore();
    store.finalText = 'Current recording';
    const oldQuery = deferred<number>();
    const newQuery = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    let queries = 0;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return ++queries === 1 ? oldQuery.promise : newQuery.promise;
      if (command === 'get_recording_status') return Promise.resolve('Recording');
      return defaultInvoke(command, ...args);
    });
    const oldClose = tauriEventMock.handlers.get('recording:window-will-hide-for-hotkey-stop')![0]({ payload: { windowEpoch: 1 } });
    const newShow = tauriEventMock.handlers.get('recording:window-shown')![0]({ payload: { windowEpoch: 2 } });
    oldQuery.resolve(1);
    await oldClose;
    expect(store.displayText).not.toContain('Current recording');
    newQuery.resolve(2);
    await newShow;
    await nextTick();
    expect(document.querySelector('.mini-closing')).toBeNull();
    expect(store.displayText).toContain('Current recording');
    expect(hideWindowMock).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('cancels obsolete retry before a newer external start finishes epoch validation', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const query = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return query.promise;
      if (command === 'start_recording') return Promise.reject('Connection error: network unavailable');
      return defaultInvoke(command, ...args);
    });
    const store = useTranscriptionStore();
    const oldStart = store.startRecording();
    await flushMicrotasks();
    await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
    const incomingStart = tauriEventMock.handlers.get('recording:start-requested')![0]({ payload: { windowEpoch: 2 } });
    await vi.advanceTimersByTimeAsync(2000);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
    query.resolve(2);
    await Promise.all([incomingStart, oldStart]);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    expect(store.sessionId).toBe(90);
    expect(store.isConnecting).toBe(false);
    wrapper.unmount();
  });

  it('preserves a current retry when a locally equal external start proves obsolete in native state', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const query = deferred<number>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    let starts = 0;
    invokeMock.mockImplementation(async (command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return query.promise;
      if (command === 'start_recording') {
        starts++;
        if (starts === 1) throw new Error('Connection error: network unavailable');
        await emitTauriEvent('recording:status', { session_id: 91, status: 'Recording' });
        return 'started';
      }
      return defaultInvoke(command, ...args);
    });
    const store = useTranscriptionStore();
    const start = store.startRecording();
    await flushMicrotasks();
    await flushMicrotasks();
    const stale = tauriEventMock.handlers.get('recording:start-requested')![0]({ payload: { windowEpoch: 1 } });
    await vi.advanceTimersByTimeAsync(2000);
    expect(starts).toBe(1);
    query.resolve(2);
    await stale;
    await vi.advanceTimersByTimeAsync(2000);
    await start;
    expect(starts).toBe(2);
    expect(store.status).toBe('Recording');
    wrapper.unmount();
  });

  it('does not adopt a native successor into retry ownership during delayed settings initialization', async () => {
    const configReady = deferred<void>();
    appConfigMock.startSync.mockReturnValue(configReady.promise);
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 1);
    expect(tauriEventMock.handlers.get('recording:start-requested')?.length ?? 0).toBe(1);
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'start_recording') return Promise.reject('Connection error: network unavailable');
      return defaultInvoke(command, ...args);
    });
    const store = useTranscriptionStore();
    const oldStart = store.startRecording();
    await flushMicrotasks();
    await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Starting' });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    store.finalText = 'Native successor';
    await vi.advanceTimersByTimeAsync(2000);
    await oldStart;
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(0);
    expect(store.status).toBe('Recording');
    expect(store.sessionId).toBe(90);
    expect(store.finalText).toBe('Native successor');
    configReady.resolve();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    wrapper.unmount();
  });

  it('retires the old pending invoke when an accepted external start already delivered Starting', async () => {
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    const query = deferred<number>();
    const oldInvoke = deferred<void>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return query.promise;
      if (command === 'start_recording') return oldInvoke.promise.then(() => { throw new Error('Connection error: old invoke failed'); });
      return defaultInvoke(command, ...args);
    });
    const store = useTranscriptionStore();
    const oldStart = store.startRecording();
    await flushMicrotasks();
    await flushMicrotasks();
    const externalStart = tauriEventMock.handlers.get('recording:start-requested')![0]({ payload: { windowEpoch: 2 } });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Starting' });
    query.resolve(2);
    await externalStart;
    oldInvoke.resolve();
    await vi.advanceTimersByTimeAsync(2000);
    await oldStart;
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(0);
    expect(store.status).toBe('Starting');
    expect(store.sessionId).toBe(90);
    wrapper.unmount();
  });

  it('subscribes to start ownership and gates buttons before delayed settings initialization', async () => {
    appConfigMock.showMiniRecordingWindow = false;
    const configReady = deferred<void>();
    appConfigMock.startSync.mockReturnValue(configReady.promise);
    const wrapper = mountRecordingPopover();
    await waitForListenerCount('recording:status', 1);
    expect(tauriEventMock.handlers.get('recording:start-requested')).toHaveLength(1);
    const button = document.querySelector<HTMLButtonElement>('.record-button')!;
    const incomingButton = document.querySelector<HTMLButtonElement>(
      '[data-testid="incoming-translation-toggle"]',
    )!;
    expect(button.disabled).toBe(true);
    expect(incomingButton.disabled).toBe(true);
    button.click();
    incomingButton.click();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(0);
    expect(
      invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_incoming_translation'),
    ).toHaveLength(0);

    const query = deferred<number>();
    const oldInvoke = deferred<void>();
    const defaultInvoke = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, ...args: any[]) => {
      if (command === 'get_recording_window_epoch') return query.promise;
      if (command === 'start_recording') return oldInvoke.promise.then(() => { throw new Error('Connection error: old invoke failed'); });
      return defaultInvoke(command, ...args);
    });
    // Exercise the store directly too: native ownership is safe independently
    // of the button gate while the rest of the component is still initializing.
    const store = useTranscriptionStore();
    const oldStart = store.startRecording();
    await flushMicrotasks();
    await flushMicrotasks();
    const successor = tauriEventMock.handlers.get('recording:start-requested')![0]({ payload: { windowEpoch: 2 } });
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Starting' });
    query.resolve(2);
    await successor;
    oldInvoke.resolve();
    await vi.advanceTimersByTimeAsync(2000);
    await oldStart;
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(0);
    expect(store.sessionId).toBe(90);
    expect(store.status).toBe('Starting');
    configReady.resolve();
    await waitForListenerCount('hotkey:toggle-recording', 1);
    await emitTauriEvent('recording:status', { session_id: 90, status: 'Recording' });
    expect(button.disabled).toBe(false);
    expect(incomingButton.disabled).toBe(false);
    wrapper.unmount();
  });

});
