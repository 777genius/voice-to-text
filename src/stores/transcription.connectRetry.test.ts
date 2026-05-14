import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useTranscriptionStore } from './transcription';

const invokeMock = vi.fn();
const listenMock = vi.fn();

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

    expect(store.finalText).toBe('готово');
  });

  it('auto-paste вставляет segment-final сразу и не дублирует его на speech-final', async () => {
    const handlers = new Map<string, any>();
    appConfigMock.autoPasteText = true;

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });

    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    const statusHandler = handlers.get('recording:status');
    const partialHandler = handlers.get('transcription:partial');
    const finalHandler = handlers.get('transcription:final');

    await statusHandler({
      payload: { session_id: 7, status: 'Recording', stopped_via_hotkey: false },
    });

    await partialHandler({
      payload: {
        session_id: 7,
        text: 'Первый кусок',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 1,
      },
    });

    await partialHandler({
      payload: {
        session_id: 7,
        text: 'второй кусок',
        timestamp: 2,
        is_segment_final: true,
        start: 1,
        duration: 1,
      },
    });

    await finalHandler({
      payload: {
        session_id: 7,
        text: '',
        timestamp: 3,
      },
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

    const statusHandler = handlers.get('recording:status');
    const partialHandler = handlers.get('transcription:partial');

    await statusHandler({
      payload: { session_id: 8, status: 'Recording', stopped_via_hotkey: false },
    });

    const firstPartial = partialHandler({
      payload: {
        session_id: 8,
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

    const secondPartial = partialHandler({
      payload: {
        session_id: 8,
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

    const statusHandler = handlers.get('recording:status');
    const partialHandler = handlers.get('transcription:partial');

    await statusHandler({
      payload: { session_id: 10, status: 'Recording', stopped_via_hotkey: false },
    });

    const oldPartial = partialHandler({
      payload: {
        session_id: 10,
        text: 'Старый текст',
        timestamp: 1,
        is_segment_final: true,
        start: 0,
        duration: 1,
      },
    });
    await flushMicrotasks();

    await statusHandler({
      payload: { session_id: 11, status: 'Recording', stopped_via_hotkey: false },
    });

    oldPaste.resolve(null);
    await oldPartial;

    await partialHandler({
      payload: {
        session_id: 11,
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
});
