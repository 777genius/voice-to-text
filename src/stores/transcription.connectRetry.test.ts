import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useTranscriptionStore } from './transcription';

const invokeMock = vi.fn();
const listenMock = vi.fn();
let consoleSpies: Array<{ mockRestore: () => void }> = [];

const tokenRepoMock = vi.hoisted(() => ({
  get: vi.fn(),
  clear: vi.fn(),
}));

const authStoreMock = vi.hoisted(() => ({
  isAuthenticated: true,
  session: { user: { id: 'u1' } },
  accessToken: 'access_old',
  reset: vi.fn(),
  setAuthenticated: vi.fn(),
  setSessionExpired: vi.fn(),
}));

const authContainerMock = vi.hoisted(() => ({
  refreshTokensUseCase: {
    execute: vi.fn(),
  },
}));

const appConfigMock = vi.hoisted(() => ({
  autoCopyToClipboard: false,
  autoPasteText: false,
  playCompletionSound: false,
  hideRecordingWindowOnHotkey: false,
  showMiniRecordingWindow: false,
  keepRecordingUntilManualStop: false,
  doubleSpaceHotkeyEnabled: false,
  recordingMode: 'dictation' as 'dictation' | 'live_translation',
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function liveTranslationHealthCheckOk() {
  return {
    ok: true,
    checked_at_ms: 123,
    items: [
      {
        id: 'openai',
        label: 'OpenAI key',
        ok: true,
        required: true,
        message: 'OpenAI probe succeeded',
      },
    ],
  };
}

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

vi.mock('../utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

vi.mock('./appConfig', () => ({
  useAppConfigStore: () => appConfigMock,
}));

vi.mock('../features/auth/infrastructure/repositories/TokenRepository', () => ({
  getTokenRepository: () => tokenRepoMock,
}));

vi.mock('../features/auth/infrastructure/di/authContainer', () => ({
  getAuthContainer: () => authContainerMock,
}));

vi.mock('../features/auth/store/authStore', () => ({
  useAuthStore: () => authStoreMock,
}));

vi.mock('../features/auth/domain/entities/Session', () => ({
  canRefreshSession: () => true,
  isAccessTokenExpired: () => false,
}));

describe('transcription connect-retry reliability', () => {
  beforeEach(() => {
    consoleSpies = (['log', 'info', 'warn', 'error'] as const).map((method) =>
      vi.spyOn(console, method).mockImplementation(() => {})
    );

    setActivePinia(createPinia());

    invokeMock.mockReset();
    listenMock.mockReset();
    tokenRepoMock.get.mockReset();
    tokenRepoMock.clear.mockReset();
    authStoreMock.reset.mockReset();
    authStoreMock.setAuthenticated.mockReset();
    authContainerMock.refreshTokensUseCase.execute.mockReset();
    appConfigMock.autoCopyToClipboard = false;
    appConfigMock.autoPasteText = false;
    appConfigMock.playCompletionSound = false;
    appConfigMock.hideRecordingWindowOnHotkey = false;
    appConfigMock.showMiniRecordingWindow = false;
    appConfigMock.keepRecordingUntilManualStop = false;
    appConfigMock.recordingMode = 'dictation';

    // initialize() не вызываем, но пусть listen будет безопасным.
    listenMock.mockResolvedValue(() => {});

    tokenRepoMock.get.mockResolvedValue({
      refreshToken: 'refresh',
      accessToken: 'access_old',
      refreshExpiresAt: new Date('2999-01-01'),
      accessExpiresAt: new Date('2999-01-01'),
      user: { id: 'u1' },
    });

    authContainerMock.refreshTokensUseCase.execute.mockResolvedValue({
      accessToken: 'access_new',
    });
  });

  afterEach(() => {
    for (const spy of consoleSpies) {
      spy.mockRestore();
    }
    consoleSpies = [];
  });

  it('cleanup отписывает listener, если initialize listen завершился после cleanup', async () => {
    const pendingListen = deferred<() => void>();
    const unlisten = vi.fn();
    listenMock.mockReturnValueOnce(pendingListen.promise);
    const store = useTranscriptionStore();

    const initialize = store.initialize();
    for (let i = 0; i < 20 && listenMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }
    expect(listenMock).toHaveBeenCalledTimes(1);

    store.cleanup();
    pendingListen.resolve(unlisten);
    await initialize;

    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(listenMock).toHaveBeenCalledTimes(1);
  });

  it('cleanup отписывает уже зарегистрированные listeners, если initialize упал на следующем listen', async () => {
    const unlistenFirst = vi.fn();
    listenMock
      .mockResolvedValueOnce(unlistenFirst)
      .mockRejectedValueOnce(new Error('listen unavailable'));

    const store = useTranscriptionStore();

    await store.initialize();

    expect(unlistenFirst).toHaveBeenCalledTimes(1);
    expect(store.error).toContain('listen unavailable');
  });

  it('не залипает на "Подключение..." при 401 даже после refresh', async () => {
    let startRecordingCalls = 0;

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') {
        startRecordingCalls++;
        return Promise.reject(
          'Authentication error: 401 Unauthorized. Токен недействителен/истёк — попробуй перелогиниться.'
        );
      }
      // set_authenticated / show_auth_window / stop_recording и т.п.
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();

    await store.startRecording();

    expect(startRecordingCalls).toBeGreaterThanOrEqual(2);
    expect(store.isConnecting).toBe(false);
    expect(store.status).toBe('Idle');
    expect(authStoreMock.reset).toHaveBeenCalled();

    const calledShowAuth = invokeMock.mock.calls.some((c) => c[0] === 'show_auth_window');
    expect(calledShowAuth).toBe(true);
  });

  it('не запускает STT auth/logout flow для OpenAI auth error в live translation', async () => {
    appConfigMock.recordingMode = 'live_translation';
    let startRecordingCalls = 0;

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') {
        startRecordingCalls++;
        return Promise.reject('Authentication: HTTP 401 during WS handshake');
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();

    await store.startRecording();

    expect(startRecordingCalls).toBe(1);
    expect(authContainerMock.refreshTokensUseCase.execute).not.toHaveBeenCalled();
    expect(authStoreMock.reset).not.toHaveBeenCalled();
    expect(store.status).toBe('Error');
    expect(store.errorType).toBe('authentication');
  });

  it('не показывает "Подключение..." для ожидаемого warm-start после hotkey', () => {
    const store = useTranscriptionStore();
    store.finalText = 'старый текст';
    store.accumulatedText = 'старый хвост';
    store.partialText = 'старый partial';
    appConfigMock.recordingMode = 'live_translation';

    store.prepareForRustHotkeyStart(true);

    expect(store.status).toBe('Recording');
    expect(store.isRecording).toBe(true);
    expect(store.isStarting).toBe(false);
    expect(store.isConnecting).toBe(false);
    expect(store.sessionId).toBeNull();
    expect(store.hasVisibleTranscriptionText).toBe(false);
    expect(store.visibleFinalText).toBe('');
    expect(store.finalText).toBe('старый текст');
    expect(store.activeRecordingMode).toBe('live_translation');
  });

  it('отменяет отложенный 429 retry при новом Rust hotkey start', async () => {
    vi.useFakeTimers();
    const handlers = new Map<string, any>();

    try {
      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });
      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 81, status: 'Starting', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:error')({
        payload: {
          session_id: 81,
          error: 'Too many active sessions',
          error_type: 'connection',
          error_details: {
            category: 'rate_limited',
            httpStatus: 429,
            serverCode: 'TOO_MANY_SESSIONS',
          },
        },
      });

      store.prepareForRustHotkeyStart(false);
      await vi.advanceTimersByTimeAsync(2_100);

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'start_recording')).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('принимает translation delta как live mode fallback если status event потерялся', async () => {
    const handlers = new Map<string, any>();
    appConfigMock.recordingMode = 'live_translation';

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();
    store.prepareForRustHotkeyStart(true);

    await handlers.get('translation:delta')({
      payload: { session_id: 71, text: 'Hello', is_final: false },
    });
    await handlers.get('translation:delta')({
      payload: { session_id: 71, text: ' world', is_final: false },
    });

    expect(store.sessionId).toBe(71);
    expect(store.status).toBe('Recording');
    expect(store.activeRecordingMode).toBe('live_translation');
    expect(store.translationText).toBe('Hello world');
    expect(store.displayText).toBe('Hello world');
  });

  it('не переключает текущую dictation-сессию в live mode из stale translation event', async () => {
    const handlers = new Map<string, any>();
    appConfigMock.recordingMode = 'dictation';

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 72, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('translation:delta')({
      payload: { session_id: 71, text: 'late live translation', is_final: false },
    });

    expect(store.sessionId).toBe(72);
    expect(store.status).toBe('Recording');
    expect(store.activeRecordingMode).toBe('dictation');
    expect(store.translationText).toBe('');
  });

  it('завершает live translation connect-loop сразу по translation:error', async () => {
    const handlers = new Map<string, any>();
    const start = deferred<string>();
    appConfigMock.recordingMode = 'live_translation';

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') return start.promise;
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    const startPromise = store.startRecording();
    await flushMicrotasks();

    await handlers.get('translation:error')({
      payload: {
        session_id: 91,
        error: 'Authentication: HTTP 401 during WS handshake',
        error_type: 'authentication',
      },
    });
    start.resolve('LiveTranslation started');
    await startPromise;

    expect(store.isConnecting).toBe(false);
    expect(store.status).toBe('Error');
    expect(store.errorType).toBe('authentication');
    expect(authContainerMock.refreshTokensUseCase.execute).not.toHaveBeenCalled();
    expect(authStoreMock.reset).not.toHaveBeenCalled();
  });

  it('live translation terminal error закрывает session от поздних delta/status events', async () => {
    const handlers = new Map<string, any>();
    appConfigMock.recordingMode = 'live_translation';

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();
    store.prepareForRustHotkeyStart(true);

    await handlers.get('translation:delta')({
      payload: { session_id: 91, text: 'before error', is_final: false },
    });
    await handlers.get('translation:error')({
      payload: {
        session_id: 91,
        error: 'Authentication: HTTP 401 during WS handshake',
        error_type: 'authentication',
      },
    });
    await handlers.get('translation:delta')({
      payload: { session_id: 91, text: ' late delta', is_final: false },
    });
    await handlers.get('recording:status')({
      payload: {
        session_id: 91,
        status: 'Recording',
        stopped_via_hotkey: false,
        mode: 'live_translation',
      },
    });

    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeGreaterThanOrEqual(91);
    expect(store.translationText).toBe('before error');
  });

  it('toggle incoming translation вызывает явные start/stop команды и показывает invoke error', async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_incoming_translation') return Promise.resolve('Incoming translation started');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.toggleIncomingTranslation();

    expect(invokeMock).toHaveBeenCalledWith('start_incoming_translation');
    expect(store.incomingTranslationError).toBeNull();

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_incoming_translation') return Promise.reject('screen audio permission denied');
      return Promise.resolve(null);
    });

    await store.toggleIncomingTranslation();

    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingTranslationError).toContain('screen audio permission denied');
  });

  it('incoming translation игнорирует повторный toggle пока команда выполняется', async () => {
    const pendingStart = deferred<string>();

    listenMock.mockResolvedValue(() => {});
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_incoming_translation') return pendingStart.promise;
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    const firstToggle = store.toggleIncomingTranslation();
    await store.toggleIncomingTranslation();

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('start_incoming_translation');

    pendingStart.resolve('Incoming translation started');
    await firstToggle;
  });

  it('incoming translation после terminal error повторно запускается через start, а не backend toggle/stop', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue('ok');

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:error')({
      payload: { session_id: 610, error: 'OpenAI API key missing', error_type: 'authentication' },
    });

    await store.toggleIncomingTranslation();

    expect(invokeMock).toHaveBeenCalledWith('start_incoming_translation');
    expect(invokeMock).not.toHaveBeenCalledWith('toggle_incoming_translation');
    expect(invokeMock).not.toHaveBeenCalledWith('stop_incoming_translation');
    expect(store.incomingTranslationError).toBeNull();
  });

  it('incoming translation active toggle останавливает явной stop командой', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue('ok');

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 611, status: 'Recording' },
    });

    await store.toggleIncomingTranslation();

    expect(invokeMock).toHaveBeenCalledWith('stop_incoming_translation');
    expect(invokeMock).not.toHaveBeenCalledWith('toggle_incoming_translation');
  });

  it('показывает incoming subtitles из source-final и translated delta events', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 201, status: 'Starting' },
    });
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 201, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 201, text: 'hello from zoom', timestamp: 1 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 201, text: 'привет из zoom', timestamp: 2 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 201, text: 'как дела', timestamp: 3 },
    });

    expect(store.incomingTranslationSessionId).toBe(201);
    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.isIncomingTranslationActive).toBe(true);
    expect(store.incomingSourceText).toBe('hello from zoom');
    expect(store.incomingTranslationText).toBe('привет из zoom как дела');
    expect(store.hasIncomingTranslationText).toBe(true);
    expect(store.incomingTranslationError).toBeNull();
  });

  it('запускает live translation health-check и сохраняет checklist', async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'run_live_translation_health_check') {
        return Promise.resolve(liveTranslationHealthCheckOk());
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();

    const pending = store.runLiveTranslationHealthCheck();
    expect(store.liveTranslationHealthCheckLoading).toBe(true);
    await pending;

    expect(invokeMock).toHaveBeenCalledWith('run_live_translation_health_check');
    expect(store.liveTranslationHealthCheck?.ok).toBe(true);
    expect(store.liveTranslationHealthCheckSummary).toMatch(/Ready|Готово/);
    expect(store.liveTranslationHealthCheckError).toBeNull();
    expect(store.liveTranslationHealthCheckLoading).toBe(false);
  });

  it('показывает ошибку live translation health-check', async () => {
    invokeMock.mockRejectedValue('system audio permission denied');
    const store = useTranscriptionStore();

    await store.runLiveTranslationHealthCheck();

    expect(store.liveTranslationHealthCheck).toBeNull();
    expect(store.liveTranslationHealthCheckError).toContain('system audio permission denied');
    expect(store.liveTranslationHealthCheckSummary).toContain('system audio permission denied');
    expect(store.liveTranslationHealthCheckLoading).toBe(false);
  });

  it('показывает live translation startup configuration error без ручного health-check', async () => {
    appConfigMock.recordingMode = 'live_translation';
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') {
        return Promise.reject(
          'configuration: Virtual microphone output: BlackHole is not ready'
        );
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();

    await store.startRecording();

    expect(invokeMock).toHaveBeenCalledWith('start_recording');
    expect(invokeMock).not.toHaveBeenCalledWith('run_live_translation_health_check');
    expect(store.status).toBe('Error');
    expect(store.errorType).toBe('configuration');
    expect(store.error).toContain('BlackHole');
  });

  it('изолирует incoming subtitles sessions и игнорирует поздние events старой сессии', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 301, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 301, text: 'старый перевод', timestamp: 1 },
    });

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 302, status: 'Starting' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 301, text: 'поздний старый текст', timestamp: 2 },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 302, text: 'new call audio', timestamp: 3 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 302, text: 'новый перевод', timestamp: 4 },
    });

    expect(store.incomingTranslationSessionId).toBe(302);
    expect(store.incomingSourceText).toBe('new call audio');
    expect(store.incomingTranslationText).toBe('новый перевод');

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 302, status: 'Idle' },
    });

    expect(store.incomingTranslationStatus).toBe('Idle');
    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.hasIncomingTranslationText).toBe(true);
  });

  it('incoming translation игнорирует поздние events после Idle закрытой сессии', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 501, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 501, text: 'первый перевод', timestamp: 1 },
    });
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 501, status: 'Idle' },
    });

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 501, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 501, text: 'late source', timestamp: 2 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 501, text: 'поздний перевод', timestamp: 3 },
    });
    await handlers.get('incoming_translation:error')({
      payload: { session_id: 501, error: 'late auth error', error_type: 'authentication' },
    });

    expect(store.incomingTranslationStatus).toBe('Idle');
    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.incomingSourceText).toBe('');
    expect(store.incomingTranslationText).toBe('первый перевод');
    expect(store.incomingTranslationError).toBeNull();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 502, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 502, text: 'новая сессия', timestamp: 4 },
    });

    expect(store.incomingTranslationSessionId).toBe(502);
    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationText).toBe('новая сессия');
  });

  it('incoming translation закрывает exact session id, а не весь диапазон ниже него', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 900, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 900, text: 'synthetic session', timestamp: 1 },
    });
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 900, status: 'Idle' },
    });

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 1, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 1, text: 'real backend session', timestamp: 2 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 900, text: 'late synthetic leak', timestamp: 3 },
    });

    expect(store.incomingTranslationSessionId).toBe(1);
    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationText).toBe('real backend session');
    expect(store.incomingTranslationText).not.toContain('late synthetic leak');
  });

  it('incoming translation не оживляет terminal Error поздними status/delta events', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 601, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 601, text: 'first source', timestamp: 1 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 601, text: 'первый перевод', timestamp: 2 },
    });
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 601, status: 'Error' },
    });
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 601, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 601, text: 'late source', timestamp: 3 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 601, text: 'поздний перевод', timestamp: 4 },
    });

    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingSourceText).toBe('first source');
    expect(store.incomingTranslationText).toBe('первый перевод');

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 602, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 602, text: 'новая сессия', timestamp: 5 },
    });

    expect(store.incomingTranslationSessionId).toBe(602);
    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationText).toBe('новая сессия');
  });

  it('incoming translation transient errors не ломают subtitles, но terminal status показывает последнюю ошибку', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 401, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 401, text: 'первый перевод', timestamp: 1 },
    });
    await handlers.get('incoming_translation:error')({
      payload: { session_id: 401, error: 'temporary network blip', error_type: 'connection' },
    });

    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationError).toBeNull();
    expect(store.incomingTranslationText).toBe('первый перевод');

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 401, status: 'Error' },
    });

    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingTranslationError).toContain('temporary network blip');
    expect(store.isIncomingTranslationActive).toBe(false);

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 402, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 402, text: 'новый перевод', timestamp: 2 },
    });

    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationError).toBeNull();
    expect(store.incomingTranslationText).toBe('новый перевод');

    await handlers.get('incoming_translation:error')({
      payload: { session_id: 402, error: 'OpenAI API key missing', error_type: 'authentication' },
    });

    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingTranslationError).toContain('OpenAI API key missing');
  });

  it('очищает скрытый старый текст, когда приходит новая recording session', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();
    store.sessionId = 61;
    store.finalText = 'старый текст';

    store.prepareForRustHotkeyStart(true);
    expect(store.finalText).toBe('старый текст');
    expect(store.hasVisibleTranscriptionText).toBe(false);

    await handlers.get('recording:status')({
      payload: { session_id: 62, status: 'Recording', stopped_via_hotkey: false },
    });

    expect(store.sessionId).toBe(62);
    expect(store.finalText).toBe('');
    expect(store.hasVisibleTranscriptionText).toBe(false);
  });

  it('не помечает текущую сессию закрытой при reconcile race (Idle во время старта)', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    // reconcileBackendStatus() внутри вызовет get_recording_status → вернём Idle (race)
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'get_recording_status') return Promise.resolve('Idle');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    const statusHandler = handlers.get('recording:status');
    expect(typeof statusHandler).toBe('function');

    // Сначала прилетел Starting с session_id=32 (мы в start flow)
    await statusHandler({ payload: { session_id: 32, status: 'Starting', stopped_via_hotkey: false } });
    expect(store.status).toBe('Starting');

    // Затем window_shown / reconcile успевает увидеть Idle (race) — НЕ должны закрыть session 32
    await store.reconcileBackendStatus('test_race');
    expect(store.status).toBe('Starting');

    // Потом прилетает Recording для той же сессии — обязаны принять и перейти в Recording
    await statusHandler({ payload: { session_id: 32, status: 'Recording', stopped_via_hotkey: false } });
    expect(store.status).toBe('Recording');
  });

  it('показывает понятную причину когда микрофон недоступен', async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') {
        return Promise.reject(
          'Internal error: Failed to start audio capture: Capture error: Failed to build audio stream: The requested device is no longer available. For example, it has been unplugged. (type: processing)'
        );
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.startRecording();

    expect(store.isConnecting).toBe(false);
    expect(store.status).toBe('Error');
    expect(store.errorType).toBe('processing');
    expect(store.error).toContain('Микрофон недоступен');
  });

  it('не залипает в Processing если stop завершился, но Idle event не дошёл', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'stop_recording') return Promise.resolve('Recording stopped');
      if (cmd === 'get_recording_status') return Promise.resolve('Idle');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 21, status: 'Recording', stopped_via_hotkey: false },
    });
    expect(store.status).toBe('Recording');

    await store.stopRecording();

    expect(store.status).toBe('Idle');
    expect(store.error).toBeNull();
  });

  it('не показывает ложную stop-ошибку если backend уже восстановился в Idle', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'stop_recording') return Promise.reject('Failed to stop audio capture');
      if (cmd === 'get_recording_status') return Promise.resolve('Idle');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 22, status: 'Recording', stopped_via_hotkey: false },
    });

    await store.stopRecording();

    expect(store.status).toBe('Idle');
    expect(store.error).toBeNull();
  });

  it('не показывает finalized и cumulative interim дублем', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 12, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 12,
        text: 'Ты слышишь, что',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 1.1,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 12,
        text: 'Ты слышишь, что я говорю?',
        timestamp: 2,
        is_segment_final: false,
        start: 1.1,
        duration: 1.5,
      },
    });

    expect(store.displayText).toBe('Ты слышишь, что я говорю?');
  });

  it('не схлопывает короткие повторы в live отображении', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 13, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 13,
        text: 'да',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 0.3,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 13,
        text: 'да',
        timestamp: 2,
        is_segment_final: false,
        start: 0.3,
        duration: 0.2,
      },
    });

    expect(store.displayText).toBe('да да');
  });

  it('не удаляет повторяющиеся слова на границе finalized segment и live partial', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 17, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 17,
        text: 'two two',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 3.26,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 17,
        text: 'two two three three',
        timestamp: 2,
        is_segment_final: false,
        start: 3.26,
        duration: 2.24,
      },
    });

    expect(store.displayText).toBe('two two two two three three');
  });

  it('не переносит is_final=false partial в stable text при смене start', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 16, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 16,
        text: 'первая часть',
        timestamp: 1,
        is_segment_final: false,
        start: 0,
        duration: 0.8,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 16,
        text: 'вторая часть',
        timestamp: 2,
        is_segment_final: false,
        start: 0.8,
        duration: 0.9,
      },
    });

    expect(store.finalText).toBe('');
    expect(store.displayText).toBe('вторая часть');

    await handlers.get('transcription:final')({
      payload: {
        session_id: 16,
        text: '',
        timestamp: 3,
      },
    });

    expect(store.finalText).toBe('вторая часть');
  });

  it('auto-paste не коммитит устаревший interim при corrected segment-final', async () => {
    const handlers = new Map<string, any>();
    appConfigMock.autoPasteText = true;

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 18, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 18,
        text: 'Ты уверен, что так будет надёжно фокусировать',
        timestamp: 1,
        is_segment_final: false,
        start: 0,
        duration: 2.1,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 18,
        text: 'Ты уверен, что так будет надёжно,',
        timestamp: 2,
        is_segment_final: false,
        start: 2.1,
        duration: 0.4,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 18,
        text: 'Ты уверен, что так будет надёжно,',
        timestamp: 3,
        is_segment_final: true,
        start: 0,
        duration: 2.5,
      },
    });

    await handlers.get('transcription:final')({
      payload: {
        session_id: 18,
        text: 'фокусироваться и не сломается?',
        timestamp: 4,
        start: 2.5,
        duration: 2.53,
      },
    });

    const pasteCalls = invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text');
    expect(pasteCalls).toEqual([
      ['auto_paste_text', { text: 'Ты уверен, что так будет надёжно,' }],
      ['auto_paste_text', { text: ' фокусироваться и не сломается?' }],
    ]);
    expect(store.finalText).toBe('Ты уверен, что так будет надёжно, фокусироваться и не сломается?');
  });

  it('append-ит finalized chunks по Deepgram, даже если слова повторяются на границе', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 14, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 14,
        text: 'two two',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 3.26,
      },
    });

    await handlers.get('transcription:final')({
      payload: {
        session_id: 14,
        text: 'two two three three',
        timestamp: 2,
        start: 3.26,
        duration: 2.24,
      },
    });

    expect(store.finalText).toBe('two two two two three three');
  });

  it('не дублирует speech_final с тем же finalized audio range', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 15, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 15,
        text: 'готово',
        timestamp: 1,
        is_segment_final: true,
        start: 1,
        duration: 0.7,
      },
    });

    await handlers.get('transcription:final')({
      payload: {
        session_id: 15,
        text: 'готово',
        timestamp: 2,
        start: 1,
        duration: 0.7,
      },
    });

    await handlers.get('transcription:final')({
      payload: {
        session_id: 15,
        text: 'готово',
        timestamp: 3,
        start: 1,
        duration: 0.7,
      },
    });

    expect(store.finalText).toBe('готово');
  });

  it('не останавливает запись на frontend по speech_final timeout', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 19, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:final')({
        payload: {
          session_id: 19,
          text: 'первая фраза',
          timestamp: 1,
          start: 0,
          duration: 1,
        },
      });

      await vi.advanceTimersByTimeAsync(6_000);

      expect(store.status).toBe('Recording');
      expect(invokeMock.mock.calls.some((call) => call[0] === 'stop_recording')).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('auto-copy копирует весь видимый текст при остановке записи', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoCopyToClipboard = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 31, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 31,
          text: 'первый ответ',
          timestamp: 1,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('transcription:final')({
        payload: {
          session_id: 31,
          text: 'второй ответ',
          timestamp: 2,
          start: 1,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 31, status: 'Idle', stopped_via_hotkey: false },
      });

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'copy_to_clipboard_native')).toEqual([]);

      await vi.advanceTimersByTimeAsync(500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'copy_to_clipboard_native')).toEqual([
        ['copy_to_clipboard_native', { text: 'первый ответ второй ответ' }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('auto-paste вставляет segment-final сразу и не дублирует его на speech-final/Idle', async () => {
    const handlers = new Map<string, any>();
    appConfigMock.autoPasteText = true;

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 32, status: 'Recording', stopped_via_hotkey: false },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 32,
        text: 'Первый кусок',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 1,
      },
    });

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 32,
        text: 'второй кусок',
        timestamp: 2,
        is_segment_final: true,
        start: 1,
        duration: 1,
      },
    });

    await handlers.get('transcription:final')({
      payload: {
        session_id: 32,
        text: '',
        timestamp: 3,
      },
    });

    await handlers.get('recording:status')({
      payload: { session_id: 32, status: 'Idle', stopped_via_hotkey: false },
    });

    const pasteCalls = invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text');
    expect(pasteCalls).toEqual([
      ['auto_paste_text', { text: 'Первый кусок' }],
      ['auto_paste_text', { text: ' второй кусок' }],
    ]);
  });

  it('auto-paste сериализует segment-final события, если первая вставка еще идет', async () => {
    const handlers = new Map<string, any>();
    const firstPaste = deferred<null>();
    appConfigMock.autoPasteText = true;

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    let pasteCallCount = 0;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'auto_paste_text') {
        pasteCallCount++;
        return pasteCallCount === 1 ? firstPaste.promise : Promise.resolve(null);
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 33, status: 'Recording', stopped_via_hotkey: false },
    });

    const firstPartial = handlers.get('transcription:partial')({
      payload: {
        session_id: 33,
        text: 'Первый кусок',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 1,
      },
    });
    await flushMicrotasks();

    expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
      ['auto_paste_text', { text: 'Первый кусок' }],
    ]);

    const secondPartial = handlers.get('transcription:partial')({
      payload: {
        session_id: 33,
        text: 'второй кусок',
        timestamp: 2,
        is_segment_final: true,
        start: 1,
        duration: 1,
      },
    });
    await flushMicrotasks();

    expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toHaveLength(1);

    firstPaste.resolve(null);
    await Promise.all([firstPartial, secondPartial]);

    const pasteCalls = invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text');
    expect(pasteCalls).toEqual([
      ['auto_paste_text', { text: 'Первый кусок' }],
      ['auto_paste_text', { text: ' второй кусок' }],
    ]);
  });

  it('auto-paste не переносит baseline старой вставки в новую сессию', async () => {
    const handlers = new Map<string, any>();
    const oldPaste = deferred<null>();
    appConfigMock.autoPasteText = true;

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    let pasteCallCount = 0;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'auto_paste_text') {
        pasteCallCount++;
        return pasteCallCount === 1 ? oldPaste.promise : Promise.resolve(null);
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 34, status: 'Recording', stopped_via_hotkey: false },
    });

    const oldPartial = handlers.get('transcription:partial')({
      payload: {
        session_id: 34,
        text: 'Старый текст',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 1,
      },
    });
    await flushMicrotasks();

    await handlers.get('recording:status')({
      payload: { session_id: 35, status: 'Recording', stopped_via_hotkey: false },
    });

    oldPaste.resolve(null);
    await oldPartial;

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 35,
        text: 'Новый текст',
        timestamp: 2,
        is_segment_final: true,
        start: 0,
        duration: 1,
      },
    });

    const pasteCalls = invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text');
    expect(pasteCalls).toEqual([
      ['auto_paste_text', { text: 'Старый текст' }],
      ['auto_paste_text', { text: 'Новый текст' }],
    ]);
  });

  it('hotkey stop не вставляет stale partial, если late speech-final пришел пока paste queue занята', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      const firstPaste = deferred<null>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      let pasteCallCount = 0;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === 'auto_paste_text') {
          pasteCallCount++;
          return pasteCallCount === 1 ? firstPaste.promise : Promise.resolve(null);
        }
        return Promise.resolve(null);
      });

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 36, status: 'Recording', stopped_via_hotkey: false },
      });

      const firstSegment = handlers.get('transcription:partial')({
        payload: {
          session_id: 36,
          text: 'Первый кусок',
          timestamp: 1,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });
      await flushMicrotasks();

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 36,
          text: 'сырой хвост',
          timestamp: 2,
          is_segment_final: false,
          start: 1,
          duration: 1,
        },
      });

      const idleStop = handlers.get('recording:status')({
        payload: { session_id: 36, status: 'Idle', stopped_via_hotkey: true },
      });
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'Первый кусок' }],
      ]);

      const lateFinal = handlers.get('transcription:final')({
        payload: {
          session_id: 36,
          text: 'чистовой хвост',
          timestamp: 3,
          start: 1,
          duration: 1,
        },
      });
      await flushMicrotasks();

      firstPaste.resolve(null);
      await Promise.all([firstSegment, idleStop, lateFinal]);

      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      const pasteCalls = invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text');
      expect(pasteCalls).toEqual([
        ['auto_paste_text', { text: 'Первый кусок' }],
        ['auto_paste_text', { text: ' чистовой хвост' }],
      ]);
      expect(pasteCalls).not.toContainEqual(['auto_paste_text', { text: ' сырой хвост' }]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('hotkey stop вставляет partial после grace, если late final не пришел', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 37, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 37,
          text: 'последний распознанный текст',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 37, status: 'Idle', stopped_via_hotkey: true },
      });

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([]);

      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'последний распознанный текст' }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('hotkey stop закрывает session даже если delayed post-stop processing неожиданно упал', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 137, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 137,
          text: 'текст перед неожиданной ошибкой',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 137, status: 'Idle', stopped_via_hotkey: true },
      });

      expect(store.sessionId).toBe(137);
      vi.mocked(console.log).mockImplementationOnce(() => {
        throw new Error('console down during stop processing');
      });

      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      expect(store.sessionId).toBeNull();
      expect(store.partialText).toBe('');
      expect(store.closedSessionIdFloor).toBeGreaterThanOrEqual(137);
      expect(console.error).toHaveBeenCalledWith(
        '[STT] Failed to process text after stop:',
        expect.any(Error)
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it('hotkey stop auto-copy ждёт late speech-final и копирует чистовой текст', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoCopyToClipboard = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 38, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 38,
          text: 'сырой текст',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 38, status: 'Idle', stopped_via_hotkey: true },
      });

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'copy_to_clipboard_native')).toEqual([]);

      await handlers.get('transcription:final')({
        payload: {
          session_id: 38,
          text: 'чистовой текст',
          timestamp: 2,
          start: 0,
          duration: 1,
        },
      });

      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'copy_to_clipboard_native')).toEqual([
        ['copy_to_clipboard_native', { text: 'чистовой текст' }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('non-hotkey Idle вставляет текущий partial после короткого grace, если late final не пришел', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 39, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 39,
          text: 'текст перед vad stop',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 39, status: 'Idle', stopped_via_hotkey: false },
      });

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([]);

      await vi.advanceTimersByTimeAsync(500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'текст перед vad stop' }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('non-hotkey Idle ждёт late speech-final и вставляет чистовой текст вместо partial', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 39, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 39,
          text: 'сырой vad текст',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 39, status: 'Idle', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:final')({
        payload: {
          session_id: 39,
          text: 'чистовой vad текст',
          timestamp: 2,
          start: 0,
          duration: 1,
        },
      });

      await vi.advanceTimersByTimeAsync(500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'чистовой vad текст' }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('hotkey stop grace не дублирует уже вставленный segment-final', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });

      invokeMock.mockResolvedValue(null);

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 40, status: 'Recording', stopped_via_hotkey: false },
      });

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 40,
          text: 'готовый сегмент',
          timestamp: 1,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 40, status: 'Idle', stopped_via_hotkey: true },
      });

      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'готовый сегмент' }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('не переводит UI в Idle от позднего Idle старой сессии после нового Recording', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 41, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('recording:status')({
      payload: { session_id: 42, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('recording:status')({
      payload: { session_id: 41, status: 'Idle', stopped_via_hotkey: true },
    });

    expect(store.sessionId).toBe(42);
    expect(store.status).toBe('Recording');
  });

  it('window_shown reconcile не закрывает новую сессию, если get_recording_status вернул устаревший Idle', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'get_recording_status') return Promise.resolve('Idle');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 51, status: 'Recording', stopped_via_hotkey: false },
    });

    await store.reconcileBackendStatus('window_shown');

    expect(store.sessionId).toBe(51);
    expect(store.closedSessionIdFloor).toBeLessThan(51);
    expect(store.status).toBe('Recording');
  });
});
