import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useTranscriptionStore } from './transcription';
import { reconcilePartialAnimation } from './transcriptionReconciliation';

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

const apiClientMock = vi.hoisted(() => ({
  get: vi.fn(),
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
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

async function initializeStoreWithHandlers() {
  const handlers = new Map<string, any>();
  listenMock.mockImplementation(async (eventName: string, handler: any) => {
    handlers.set(eventName, handler);
    return () => {};
  });
  const store = useTranscriptionStore();
  await store.initialize();
  return { handlers, store };
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

vi.mock('../features/auth/infrastructure/api/apiClient', () => ({
  api: apiClientMock,
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
    apiClientMock.get.mockReset();
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
    apiClientMock.get.mockResolvedValue({ licenses: [] });
  });

  it.each(['confirmed', 'false', 'ipc_error'])('serializes terminal paste, gate, copy and %s acknowledgement without replay', async (ackOutcome) => {
    appConfigMock.autoPasteText = true;
    appConfigMock.autoCopyToClipboard = true;
    const paste = deferred<any>();
    const copy = deferred<any>();
    const ack = deferred<boolean>();
    invokeMock.mockImplementation((cmd: string, args: any) => {
      if (cmd === 'auto_paste_continuation_text') return paste.promise;
      if (cmd === 'copy_continuation_text') return args.text ? copy.promise : Promise.resolve({status: 'confirmed', revision: 1});
      if (cmd === 'finish_continuation_delivery') return ack.promise;
      return Promise.resolve();
    });
    const {handlers, store} = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    const stable = handlers.get('transcription:final')({payload: {session_id: 1, delivery_seq: 1, text: 'stable', timestamp: 0, start: 0, duration: 0}});
    await flushMicrotasks();
    const terminal = {session_id: 1, stable_snapshot: 'stable', delivery_complete: true, report: null, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: terminal});
    const effects = () => invokeMock.mock.calls.filter(([cmd]) => ['auto_paste_continuation_text', 'copy_continuation_text', 'finish_continuation_delivery', 'auto_paste_text', 'copy_to_clipboard_native'].includes(cmd));
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(effects().map(([cmd]) => cmd)).toEqual(['auto_paste_continuation_text']);
    paste.resolve({status: 'confirmed', revision: 1});
    await stable;
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(effects()).toEqual([
      ['auto_paste_continuation_text', {sessionId: 1, deliverySeq: 1, text: 'stable'}],
      ['copy_continuation_text', {sessionId: 1, deliverySeq: 2, text: ''}],
      ['copy_continuation_text', {sessionId: 1, deliverySeq: 3, text: 'stable'}],
    ]);
    copy.resolve({status: 'confirmed', revision: 1});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(effects()[effects().length - 1]).toEqual(['finish_continuation_delivery', {sessionId: 1, deliverySeq: 3}]);
    // B's delivery must wait for A's acknowledgement result, too.
    await handlers.get('recording:status')({payload: {session_id: 2, status: 'Recording'}});
    const next = handlers.get('transcription:final')({payload: {session_id: 2, delivery_seq: 1, text: 'next', timestamp: 0, start: 0, duration: 0}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(effects()).toHaveLength(4);
    if (ackOutcome === 'ipc_error') ack.reject(new Error('lost acknowledgement'));
    else ack.resolve(ackOutcome === 'confirmed');
    await next;
    handlers.get('transcription:terminal')({payload: terminal});
    await handlers.get('transcription:final')({payload: {session_id: 1, delivery_seq: 2, text: 'late', timestamp: 0, start: 0, duration: 0}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(effects()).toHaveLength(5);
    expect(effects()[effects().length - 1]).toEqual(['auto_paste_text', {sessionId: 2, text: 'next'}]);
    expect(store.deliveryRecovery.filter(item => item.sessionId === 1)).toEqual([]);
    expect(store.finalText).toBe('next');
    store.cleanup();
  });

  it.each([false, true])('acknowledges empty terminal behind pending work (prior paste=%s)', async (priorPaste) => {
    appConfigMock.autoPasteText = true;
    appConfigMock.autoCopyToClipboard = true;
    const paste = deferred<any>();
    invokeMock.mockImplementation((cmd: string) => cmd === 'auto_paste_continuation_text' ? paste.promise : Promise.resolve(true));
    const {handlers, store} = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    if (priorPaste) handlers.get('transcription:final')({payload: {session_id: 1, delivery_seq: 1, text: 'earlier', timestamp: 0, start: 0, duration: 0}});
    await flushMicrotasks();
    const terminal = {session_id: 1, continuation_delivery: false, stable_snapshot: '', delivery_complete: true, report: null, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: terminal});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    if (priorPaste) expect(invokeMock).not.toHaveBeenCalledWith('finish_continuation_delivery', expect.anything());
    paste.resolve({status: 'confirmed', revision: 1});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([
      ['finish_continuation_delivery', {sessionId: 1, deliverySeq: priorPaste ? 1 : 0}],
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_continuation_text' || cmd === 'copy_to_clipboard_native')).toEqual([]);
    expect(store.finalText).toBe('');
    store.cleanup();
  });

  it('acknowledges a nonempty no-op terminal with sequence zero and preserves text', async () => {
    invokeMock.mockResolvedValue(false);
    const {handlers, store} = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    const terminal = {session_id: 1, stable_snapshot: 'retained', delivery_complete: true, report: null, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    handlers.get('transcription:terminal')({payload: terminal});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([
      ['finish_continuation_delivery', {sessionId: 1, deliverySeq: 0}],
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => ['copy_continuation_text', 'auto_paste_continuation_text', 'copy_to_clipboard_native', 'auto_paste_text'].includes(cmd))).toEqual([]);
    expect(store.finalText).toBe('retained');
    store.cleanup();
  });

  it.each([null, 'deepgram'])('does not acknowledge legacy/DG terminal (%s)', async (provider) => {
    const {handlers, store} = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('transcription:terminal')({payload: {session_id: 1, continuation_delivery: false, stable_snapshot: 'text', delivery_complete: true, report: provider ? {provider} : null, error: null}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([]);
    store.cleanup();
  });

  it.each(['context_mismatch', 'unavailable', 'uncertain', 'ipc_error'])('continuation %s suppresses already queued paste and terminal copy', async (outcome) => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    const pending = deferred<any>();
    invokeMock.mockImplementation((command: string) => command === 'auto_paste_continuation_text' ? pending.promise : Promise.resolve());
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    handlers.get('recording:intent-projection')({ payload: {
      intentRevision: 1, runId: 1, logicalRunId: 1, captureEpisodeId: 1,
      continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording', processingJobs: 0, shutdownRequested: false,
    } });
    const stable = (seq: number, text: string) => handlers.get('transcription:final')({ payload: {
      session_id: 1, delivery_seq: seq, text, timestamp: 0, start: 0, duration: 0, timing_known: false,
    } });
    const a = stable(1, 'first');
    await flushMicrotasks();
    const b = stable(2, 'tail');
    handlers.get('transcription:terminal')({ payload: { session_id: 1, stable_snapshot: 'first tail', delivery_complete: true, report: null, error: null } });
    if (outcome === 'ipc_error') pending.reject(new Error('lost reply'));
    else pending.resolve({ status: outcome });
    await Promise.all([a, b]);
    await flushMicrotasks();
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([
      ['finish_continuation_delivery', {sessionId: 1, deliverySeq: 1}],
    ]);
    expect(store.finalText).toBe('first tail');
    expect(store.deliveryRecovery).toEqual([{sessionId: 1, transcript: 'first tail', unconfirmedText: 'first tail'}]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text')).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text' || cmd === 'copy_to_clipboard_native')).toHaveLength(0);
    await store.copyRecoveryText(store.deliveryRecovery[0].unconfirmedText);
    expect(invokeMock).toHaveBeenCalledWith('copy_to_clipboard_native', {text: 'first tail'});
    store.cleanup();
  });

  it('retains only the unconfirmed suffix after a confirmed prefix and uncertain native insert', async () => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    const uncertain = deferred<any>();
    let pasteAttempt = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command !== 'auto_paste_continuation_text') return Promise.resolve(true);
      pasteAttempt++;
      return pasteAttempt === 1
        ? Promise.resolve({ status: 'confirmed', revision: 1 })
        : uncertain.promise;
    });
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    handlers.get('recording:intent-projection')({ payload: {
      intentRevision: 1, runId: 1, logicalRunId: 1, captureEpisodeId: 1,
      continuationPhase: 'active', desiredOn: true, pendingStart: false,
      status: 'Recording', processingJobs: 0, shutdownRequested: false,
    } });
    const stable = (seq: number, text: string) => handlers.get('transcription:final')({ payload: {
      session_id: 1, delivery_seq: seq, text, timestamp: 0, start: 0,
      duration: 0, timing_known: false,
    } });

    await stable(1, 'known');
    await flushMicrotasks();
    const tail = stable(2, 'unknown tail');
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, stable_snapshot: 'known unknown tail',
      delivery_complete: true, report: null, error: null,
    } });
    uncertain.resolve({ status: 'uncertain' });
    await tail;
    for (let i = 0; i < 10; i++) await flushMicrotasks();

    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text')).toEqual([
      ['auto_paste_continuation_text', { text: 'known', sessionId: 1, deliverySeq: 1 }],
      ['auto_paste_continuation_text', { text: ' unknown tail', sessionId: 1, deliverySeq: 2 }],
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([
      ['finish_continuation_delivery', { sessionId: 1, deliverySeq: 2 }],
    ]);
    expect(store.finalText).toBe('known unknown tail');
    expect(store.deliveryRecovery).toEqual([{
      sessionId: 1,
      transcript: 'known unknown tail',
      unconfirmedText: 'unknown tail',
    }]);
    expect(invokeMock.mock.calls.filter(([cmd]) =>
      cmd === 'auto_paste_text' || cmd === 'copy_to_clipboard_native')).toHaveLength(0);
    await store.copyRecoveryText(store.deliveryRecovery[0].unconfirmedText);
    expect(invokeMock).toHaveBeenCalledWith('copy_to_clipboard_native', { text: 'unknown tail' });
    store.cleanup();
  });

  it('preserves native Toggle versus explicit Stop for a pending continuation', async () => {
    invokeMock.mockResolvedValue('Recording stop requested');
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('recording:intent-projection')({payload: {
      intentRevision: 1, runId: 2, logicalRunId: 1, captureEpisodeId: 2,
      continuationPhase: 'continue_pending', desiredOn: true, pendingStart: true, status: 'Processing',
    }});
    await store.toggleRecording();
    await store.toggleRecording();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'toggle_recording_with_window')).toHaveLength(2);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording' || cmd === 'stop_recording')).toHaveLength(0);
    await store.stopRecording('manual');
    expect(invokeMock).toHaveBeenCalledWith('stop_recording', {expectedSessionId: 2});
    handlers.get('recording:intent-projection')({payload: {intentRevision: 2, runId: 2, logicalRunId: 1, continuationPhase: 'paused_reclaimable', desiredOn: false, pendingStart: false, status: 'Processing'}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([]);
    store.cleanup();
  });

  it('does not retry an unknown native continuation toggle', async () => {
    invokeMock.mockImplementation((cmd: string) => cmd === 'toggle_recording_with_window'
      ? Promise.reject(new Error('toggle reply lost')) : Promise.resolve());
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'paused_reclaimable', desiredOn: false, pendingStart: false, status: 'Processing'}});
    await store.toggleRecording();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'toggle_recording_with_window')).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(0);
    expect(store.error).toContain('toggle reply lost');
    store.cleanup();
  });

  it.each([true, false])('native-only terminal refusal suppresses clipboard with autoPaste=%s', async (autoPaste) => {
    appConfigMock.autoPasteText = autoPaste;
    appConfigMock.autoCopyToClipboard = true;
    let refused = false;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(
      cmd === 'auto_paste_continuation_text' ? {status: 'confirmed', revision: 1} :
      cmd === 'copy_continuation_text' ? {status: refused ? 'context_mismatch' : 'confirmed', revision: 1} : undefined));
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    await handlers.get('transcription:final')({payload: {session_id: 1, delivery_seq: 1, text: 'stable', timestamp: 0, start: 0, duration: 0}});
    // Continue validation has independently invalidated native state; no failed paste event.
    refused = true;
    const terminal = {session_id: 1, stable_snapshot: 'stable', delivery_complete: true, report: null, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: terminal});
    for (let turn = 0; turn < 10; turn++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_continuation_text')).toEqual([
      ['copy_continuation_text', {sessionId: 1, deliverySeq: autoPaste ? 2 : 1, text: autoPaste ? '' : 'stable'}],
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native')).toHaveLength(0);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toEqual([
      ['finish_continuation_delivery', {sessionId: 1, deliverySeq: autoPaste ? 2 : 1}],
    ]);
    expect(store.finalText).toBe('stable');
    expect(store.deliveryRecovery).toHaveLength(1);
    store.cleanup();
  });

  it('without auto-paste, continuation keeps one terminal copy and never invokes native paste', async () => {
    appConfigMock.autoPasteText = false;
    appConfigMock.autoCopyToClipboard = true;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(cmd === 'copy_continuation_text' ? {status: 'confirmed', revision: 0} : undefined));
    const { handlers, store } = await initializeStoreWithHandlers();
    const status = (value: string) => handlers.get('recording:status')({payload: {session_id: 1, status: value}});
    await status('Recording');
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    const stable = (delivery_seq: number, text: string) => handlers.get('transcription:final')({payload: {session_id: 1, delivery_seq, text, timestamp: 0, start: 0, duration: 0}});
    await stable(1, 'first');
    await status('Idle');
    await status('Recording');
    await stable(2, 'second');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native')).toHaveLength(0);
    const terminal = {session_id: 1, stable_snapshot: 'first second', delivery_complete: true, report: null, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: terminal});
    for (let turn = 0; turn < 10; turn++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_continuation_text')).toEqual([
      ['copy_continuation_text', {text: 'first second', sessionId: 1, deliverySeq: 1}],
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text' || cmd === 'auto_paste_text')).toHaveLength(0);
    store.cleanup();
  });

  it.each([true, false])('routes the first stable delivery before projection using explicit continuation mode %s', async (continuation) => {
    appConfigMock.autoPasteText = true;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(cmd === 'auto_paste_continuation_text' ? {status: 'confirmed', revision: 1} : undefined));
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    await handlers.get('transcription:final')({payload: {
      session_id: 1, delivery_seq: 1, text: 'first', timestamp: 0, start: 0, duration: 0,
      completion_v1: true, continuation_delivery: continuation,
    }});
    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text' || cmd === 'auto_paste_text');
    expect(calls).toHaveLength(1);
    expect(calls[0][0]).toBe(continuation ? 'auto_paste_continuation_text' : 'auto_paste_text');
    if (continuation) {
      handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toHaveLength(0);
    }
    store.cleanup();
  });

  it.each([true, false, null, undefined])('recovers terminal-first text with mode %s before effects', async (mode) => {
    appConfigMock.autoPasteText = true;
    appConfigMock.autoCopyToClipboard = true;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(
      cmd === 'auto_paste_continuation_text' || cmd === 'copy_continuation_text'
        ? {status: 'confirmed', revision: 1} : true));
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    const terminal = {session_id: 1, continuation_delivery: mode, stable_snapshot: 'recovered',
      report: {run_id: 1, provider: {stable_snapshot: 'recovered'}}, delivery_complete: false, error: 'deadline'};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: {...terminal, continuation_delivery: !mode}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.finalText).toBe('recovered');
    const commands = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(commands.filter(cmd => cmd === 'auto_paste_continuation_text')).toHaveLength(mode === true ? 1 : 0);
    expect(commands.filter(cmd => cmd === 'copy_continuation_text')).toHaveLength(mode === true ? 1 : 0);
    expect(commands.filter(cmd => cmd === 'auto_paste_text')).toHaveLength(mode === false ? 1 : 0);
    expect(commands.filter(cmd => cmd === 'copy_to_clipboard_native')).toHaveLength(mode === false ? 1 : 0);
    expect(commands.filter(cmd => cmd === 'finish_continuation_delivery')).toHaveLength(mode === false ? 0 : 1);
    store.cleanup();
  });

  it('rejects mismatched terminal reports and keeps old-run mode out of the active ledger', async () => {
    appConfigMock.autoPasteText = true;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(
      cmd === 'auto_paste_continuation_text' ? {status: 'context_mismatch'} : true));
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    await handlers.get('recording:status')({payload: {session_id: 2, status: 'Recording'}});
    const terminal = {session_id: 2, continuation_delivery: true, stable_snapshot: 'wrong',
      report: {run_id: 1}, delivery_complete: true, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: {...terminal, session_id: 99, report: null}});
    handlers.get('transcription:terminal')({payload: {...terminal, session_id: 1, stable_snapshot: 'old'}});
    handlers.get('transcription:terminal')({payload: {...terminal, continuation_delivery: false,
      stable_snapshot: 'active', report: {run_id: 2}}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.finalText).toBe('active');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args))
      .toEqual([{text: 'active', sessionId: 2}]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text').map(([, args]) => args.sessionId))
      .toEqual([1]);
    store.cleanup();
  });

  it('refuses conflicting terminal mode without downgrading an established continuation run', async () => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    await handlers.get('transcription:partial')({payload: {session_id: 1, text: '',
      completion_v1: true, continuation_delivery: true, is_segment_final: false}});
    handlers.get('transcription:terminal')({payload: {session_id: 1, continuation_delivery: false,
      stable_snapshot: 'recover manually', report: null, error: null, delivery_complete: true}});
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.finalText).toBe('recover manually');
    expect(invokeMock.mock.calls.filter(([cmd]) => ['auto_paste_text', 'copy_to_clipboard_native',
      'auto_paste_continuation_text', 'copy_continuation_text'].includes(cmd))).toHaveLength(0);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'finish_continuation_delivery')).toHaveLength(1);
    store.cleanup();
  });

  it('retains negotiated logical text across a new capture episode and confirms deltas once', async () => {
    appConfigMock.autoPasteText = true;
    appConfigMock.autoCopyToClipboard = true;
    let revision = 0;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(cmd === 'auto_paste_continuation_text' ? {status: 'confirmed', revision: ++revision} : cmd === 'copy_continuation_text' ? {status: 'confirmed', revision} : undefined));
    const { handlers, store } = await initializeStoreWithHandlers();
    const status = (value: string) => handlers.get('recording:status')({payload: {session_id: 1, status: value}});
    await status('Recording');
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    const stable = (seq: number, text: string) => handlers.get('transcription:final')({payload: {session_id: 1, delivery_seq: seq, text, timestamp: 0, start: 0, duration: 0}});
    await stable(1, 'hello');
    await status('Idle');
    store.prepareForRustHotkeyStart();
    handlers.get('recording:intent-projection')({payload: {intentRevision: 2, runId: 2, captureEpisodeId: 2, logicalRunId: 1, continuationPhase: 'active_awaiting_audio', desiredOn: true, pendingStart: false, status: 'Recording'}});
    await status('Recording');
    expect(store.finalText).toBe('hello');
    await stable(2, 'world');
    await stable(2, 'world');
    expect(store.finalText).toBe('hello world');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native')).toHaveLength(0);
    const terminal = {session_id: 1, stable_snapshot: 'hello world', delivery_complete: true, report: null, error: null};
    handlers.get('transcription:terminal')({payload: terminal});
    handlers.get('transcription:terminal')({payload: terminal});
    // Terminal handler schedules delivery without awaiting its queue.
    for (let turn = 0; turn < 10; turn++) await flushMicrotasks();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_continuation_text')).toEqual([
      ['copy_continuation_text', {text: '', sessionId: 1, deliverySeq: 3}],
      ['copy_continuation_text', {text: 'hello world', sessionId: 1, deliverySeq: 4}],
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text').map(([, args]) => args)).toEqual([
      {text: 'hello', sessionId: 1, deliverySeq: 1}, {text: ' world', sessionId: 1, deliverySeq: 2},
    ]);
    store.cleanup();
  });

  it('keeps late negotiated A on guarded IPC while a cold B owns the visible transcript', async () => {
    appConfigMock.autoPasteText = true;
    let revision = 0;
    invokeMock.mockImplementation((cmd: string) => Promise.resolve(cmd === 'auto_paste_continuation_text' ? {status: 'confirmed', revision: ++revision} : undefined));
    const { handlers, store } = await initializeStoreWithHandlers();
    const status = (session_id: number) => handlers.get('recording:status')({payload: {session_id, status: 'Recording'}});
    const stable = (session_id: number, delivery_seq: number, text: string) => handlers.get('transcription:final')({payload: {session_id, delivery_seq, text, timestamp: 0, start: 0, duration: 0}});
    await status(1);
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 1, logicalRunId: 1, continuationPhase: 'active', desiredOn: true, pendingStart: false, status: 'Recording'}});
    await stable(1, 1, 'A');
    handlers.get('recording:intent-projection')({payload: {intentRevision: 2, runId: 2, logicalRunId: null, continuationPhase: null, desiredOn: true, pendingStart: false, status: 'Recording'}});
    await status(2);
    await stable(2, 1, 'B');
    await stable(1, 2, 'late');
    expect(store.finalText).toBe('B');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_continuation_text').map(([,args]) => args)).toEqual([
      {text: 'A', sessionId: 1, deliverySeq: 1}, {text: ' late', sessionId: 1, deliverySeq: 2},
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([,args]) => args)).toEqual([{text: 'B', sessionId: 2}]);
    store.cleanup();
  });

  it('delivers immutable terminal A after B starts, preserving A queue and target', async () => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    const firstPaste = deferred<void>();
    invokeMock.mockImplementation((command: string, args: any) => {
      if (command === 'auto_paste_text' && args.text === 'yes') return firstPaste.promise;
      return Promise.resolve();
    });
    const { handlers, store } = await initializeStoreWithHandlers();
    const status = (id: number) => handlers.get('recording:status')({ payload: { session_id: id, status: 'Recording' } });
    const stable = (id: number, seq: number, text: string) => handlers.get('transcription:final')({ payload: {
      session_id: id, delivery_seq: seq, timing_known: false, text, timestamp: 0, start: 0, duration: 0,
    } });
    await status(1);
    const inFlight = stable(1, 1, 'yes');
    await flushMicrotasks();
    await status(2);
    const tail = stable(1, 2, 'yes');
    const terminal = { session_id: 1, continuation_delivery: false, stable_snapshot: 'yes yes', delivery_complete: true, report: null, error: null };
    handlers.get('transcription:terminal')({ payload: terminal });
    terminal.stable_snapshot = 'mutated';
    handlers.get('transcription:terminal')({ payload: { ...terminal, stable_snapshot: 'duplicate' } });
    const next = stable(2, 1, 'B');
    firstPaste.resolve();
    await Promise.all([inFlight, tail, next]);
    await stable(1, 3, 'late');
    expect(store.finalText).toBe('B');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args)).toEqual([
      { text: 'yes', sessionId: 1 }, { text: ' yes', sessionId: 1 }, { text: 'B', sessionId: 2 },
    ]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native').map(([, args]) => args.text)).toEqual(['yes yes']);
    store.cleanup();
  });

  it('queues 50 received stable deliveries promptly when the paste queue is free', async () => {
    appConfigMock.autoPasteText = true;
    const elapsed: number[] = [];
    let receivedAt = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command === 'auto_paste_text') elapsed.push(performance.now() - receivedAt);
      return Promise.resolve();
    });
    const {handlers, store} = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({payload: {session_id: 1, status: 'Recording'}});
    for (let seq = 1; seq <= 50; seq++) {
      receivedAt = performance.now();
      await handlers.get('transcription:final')({payload: {
        session_id: 1, delivery_seq: seq, text: `token${seq}`, timing_known: false,
        timestamp: 0, start: 0, duration: 0,
      }});
    }
    expect(elapsed).toHaveLength(50);
    const sorted = [...elapsed].sort((a, b) => a - b);
    expect(sorted[47]).toBeLessThanOrEqual(100);
    consoleSpies[1].mockRestore();
    console.info('STABLE_ENQUEUE_EVIDENCE', JSON.stringify({
      scope: 'real store handler to mocked native invoke, free queue, Vitest; upper bound on enqueue, not OS paste latency',
      samples_ms: elapsed, p95_ms: sorted[47], max_ms: sorted[49],
    }));
    store.cleanup();
  });

  it('deduplicates stable by run and sequence while preserving repeated unknown-range text', async () => {
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    const payload = { session_id: 1, text: 'again', timestamp: 0, start: 0, duration: 0, timing_known: false, is_segment_final: true, delivery_seq: 1 };
    await handlers.get('transcription:partial')({ payload });
    await handlers.get('transcription:final')({ payload });
    await handlers.get('transcription:partial')({ payload: { ...payload, delivery_seq: 2 } });
    await handlers.get('transcription:partial')({ payload });
    expect(store.finalText).toBe('again again');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args.text)).toEqual(['again', ' again']);
    store.cleanup();
  });

  it('terminal incomplete outcome copies only stable text and never promotes negotiated interim', async () => {
    vi.useFakeTimers();
    try {
      appConfigMock.autoCopyToClipboard = true;
      appConfigMock.autoPasteText = true;
      invokeMock.mockResolvedValue(undefined);
      const { handlers, store } = await initializeStoreWithHandlers();
      await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
      await handlers.get('transcription:partial')({ payload: {
        session_id: 1, text: 'unconfirmed draft', timestamp: 0, start: 0, duration: 0,
        completion_v1: true, timing_known: false, is_segment_final: false,
      } });
      await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Idle' } });
      await vi.advanceTimersByTimeAsync(2000);
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'auto_paste_text')).toBe(false);
      handlers.get('transcription:terminal')({ payload: {
        session_id: 1, continuation_delivery: false, stable_snapshot: 'confirmed', delivery_complete: false,
        report: { provider: { tail_evidence: 'unconfirmed' } }, error: 'deadline',
      } });
      await vi.advanceTimersByTimeAsync(0);
      await handlers.get('transcription:partial')({ payload: { session_id: 1, text: 'late draft', is_segment_final: false } });
      expect(store.finalText).toBe('confirmed');
      expect(store.partialText).toBe('');
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args.text)).toEqual(['confirmed']);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native').map(([, args]) => args.text)).toEqual(['confirmed']);
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it.each([false, true])('recovers successful empty negotiated terminal once (refused=%s)', async (refused) => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    for (const text of ['old draft', 'latest draft']) {
      await handlers.get('transcription:partial')({ payload: {
        session_id: 1, text, completion_v1: true, continuation_delivery: refused,
        is_segment_final: false, timestamp: 0, timing_known: false,
      } });
    }
    const terminal = { session_id: 1, continuation_delivery: false, stable_snapshot: '',
      delivery_complete: true, report: null, error: null };
    handlers.get('transcription:terminal')({ payload: terminal });
    handlers.get('transcription:terminal')({ payload: terminal });
    await handlers.get('transcription:final')({ payload: {
      session_id: 1, text: 'late final', delivery_seq: 1, timestamp: 1,
    } });
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.finalText).toBe('latest draft');
    const effects = invokeMock.mock.calls.filter(([cmd]) =>
      ['auto_paste_text', 'copy_to_clipboard_native', 'auto_paste_continuation_text', 'copy_continuation_text'].includes(cmd));
    expect(effects.map(([, args]) => args.text)).toEqual(refused ? [] : ['latest draft', 'latest draft']);
    expect(invokeMock.mock.calls.filter(([cmd, args]) => cmd === 'log_client_event' &&
      args.event === 'transcription_terminal_interim_fallback').map(([, args]) => args.data))
      .toEqual([{ sessionId: 1, textLength: 12 }]);
    store.cleanup();
  });

  it('recovers negotiated interim after an empty sequenced stable and successful terminal', async () => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    await handlers.get('transcription:partial')({ payload: {
      session_id: 1, text: 'recognized draft', completion_v1: true,
      continuation_delivery: false, is_segment_final: false, timestamp: 0,
    } });
    await handlers.get('transcription:final')({ payload: {
      session_id: 1, text: '', delivery_seq: 1, continuation_delivery: false,
    } });

    expect(store.partialText).toBe('recognized draft');
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'auto_paste_text')).toBe(false);

    const terminal = {
      session_id: 1, continuation_delivery: false, stable_snapshot: '',
      delivery_complete: true, report: null, error: null,
    };
    handlers.get('transcription:terminal')({ payload: terminal });
    handlers.get('transcription:terminal')({ payload: terminal });
    for (let i = 0; i < 10; i++) await flushMicrotasks();

    expect(store.finalText).toBe('recognized draft');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args.text))
      .toEqual(['recognized draft']);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native').map(([, args]) => args.text))
      .toEqual(['recognized draft']);
    store.cleanup();
  });

  it.each(['', 'corrected final'])('recovers negotiated text after unsequenced speech-final %j without early paste', async (final) => {
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    for (const text of ['old draft', 'latest draft']) {
      await handlers.get('transcription:partial')({ payload: {
        session_id: 1, text, completion_v1: true, continuation_delivery: false,
        is_segment_final: false, timestamp: 0,
      } });
    }
    await handlers.get('transcription:final')({ payload: { session_id: 1, text: final, timestamp: 1 } });
    expect(store.partialText).toBe('');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toHaveLength(0);
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, continuation_delivery: false, stable_snapshot: '', delivery_complete: true,
      report: null, error: null,
    } });
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.finalText).toBe(final || 'latest draft');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args.text))
      .toEqual([final || 'latest draft']);
    store.cleanup();
  });

  it('keeps negotiated interim snapshots isolated across sessions', async () => {
    appConfigMock.autoCopyToClipboard = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    for (const session_id of [1, 2]) {
      await handlers.get('recording:status')({ payload: { session_id, status: 'Recording' } });
      await handlers.get('transcription:partial')({ payload: {
        session_id, text: `draft ${session_id}`, completion_v1: true, is_segment_final: false,
      } });
    }
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, continuation_delivery: false, stable_snapshot: '', delivery_complete: true,
      report: null, error: null,
    } });
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.partialText).toBe('draft 2');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native').map(([, args]) => args.text))
      .toEqual(['draft 1']);
    store.cleanup();
  });

  it.each([
    { error: 'failed' },
    { delivery_complete: false },
    { report: { error: 'failed' } },
    { report: { shared_failure: true } },
    { report: { provider: { error: 'failed' } } },
    ...['deadline', 'cancelled', 'processor_error'].map(reason => ({ report: { audio: { reason } } })),
    ...['deadline', 'cancelled', 'provider_error'].map(reason => ({ report: { provider: { reason } } })),
  ])('does not recover negotiated interim for adverse terminal %j', async (outcome) => {
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    await handlers.get('transcription:partial')({ payload: {
      session_id: 1, text: 'draft', completion_v1: true, is_segment_final: false,
    } });
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, continuation_delivery: false, stable_snapshot: '', delivery_complete: true,
      report: null, error: null, ...outcome,
    } });
    await flushMicrotasks();
    expect(store.finalText).toBe('');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toHaveLength(0);
    store.cleanup();
  });

  it.each(['stable', ''])('never appends negotiated interim when stable was received (terminal=%s)', async (snapshot) => {
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    await handlers.get('transcription:final')({ payload: {
      session_id: 1, text: 'stable', delivery_seq: 1, continuation_delivery: false,
    } });
    await handlers.get('transcription:partial')({ payload: {
      session_id: 1, text: 'stable overlapping draft', completion_v1: true, is_segment_final: false,
    } });
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, continuation_delivery: false, stable_snapshot: snapshot, delivery_complete: true,
      report: null, error: null,
    } });
    for (let i = 0; i < 10; i++) await flushMicrotasks();
    expect(store.finalText).toBe(snapshot);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args.text))
      .toEqual(['stable']);
    store.cleanup();
  });

  it('preserves the legacy segment-final prefix when terminal contains only the speech-final suffix', async () => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    await handlers.get('transcription:partial')({ payload: {
      session_id: 1, text: 'one', timestamp: 0, start: 0, duration: 1, is_segment_final: true,
    } });
    await handlers.get('transcription:final')({ payload: {
      session_id: 1, text: 'two', timestamp: 1, start: 1, duration: 1,
    } });
    expect(store.finalText).toBe('one two');
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, continuation_delivery: false, stable_snapshot: 'two', delivery_complete: true, report: null, error: null,
    } });
    await flushMicrotasks();
    expect(store.finalText).toBe('one two');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native').map(([, args]) => args.text)).toEqual(['one two']);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args)).toEqual([
      { text: 'one', sessionId: 1 }, { text: ' two', sessionId: 1 },
    ]);
    store.cleanup();
  });

  it('preserves legacy DG stable segment and interim stop fallback across terminal and B start', async () => {
    appConfigMock.autoCopyToClipboard = true;
    appConfigMock.autoPasteText = true;
    invokeMock.mockResolvedValue(undefined);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({ payload: { session_id: 1, status: 'Recording' } });
    await handlers.get('transcription:partial')({ payload: {
      session_id: 1, text: 'stable', timestamp: 0, start: 0, duration: 1, is_segment_final: true,
    } });
    await handlers.get('transcription:partial')({ payload: {
      session_id: 1, text: 'legacy tail', timestamp: 0, start: 1, duration: 1, is_segment_final: false, timing_known: false,
    } });
    await handlers.get('recording:status')({ payload: { session_id: 2, status: 'Recording' } });
    handlers.get('transcription:terminal')({ payload: {
      session_id: 1, continuation_delivery: false, stable_snapshot: '', delivery_complete: true, report: null, error: null,
    } });
    // A queued B stable delta acts as the existing ordered delivery barrier.
    await handlers.get('transcription:final')({ payload: {
      session_id: 2, text: 'B', timestamp: 0, delivery_seq: 1, timing_known: false,
    } });
    expect(store.finalText).toBe('B');
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native').map(([, args]) => args.text)).toEqual(['stable legacy tail']);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text').map(([, args]) => args)).toEqual([
      { text: 'stable', sessionId: 1 }, { text: ' legacy tail', sessionId: 1 }, { text: 'B', sessionId: 2 },
    ]);
    store.cleanup();
  });

  it('reconciles a corrected suffix without blanking its stable animated prefix', () => {
    expect(
      reconcilePartialAnimation('The quick brown fax', 'The quick brown fox')
    ).toEqual({
      renderedText: 'The quick brown fox',
      textToAnimate: '',
    });
  });

  it('не превращает подтвержденный pending start в timeout/retry даже через 60 секунд', async () => {
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
      const start = store.startRecording();
      await flushMicrotasks();

      await handlers.get('recording:intent-projection')({
        payload: {
          runId: 80,
          intentRevision: 2,
          status: 'Processing',
          desiredOn: true,
          pendingStart: true,
          processingJobs: 1,
          shutdownRequested: false,
        },
      });
      await vi.advanceTimersByTimeAsync(60_000);
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'start_recording')).toHaveLength(1);
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'stop_recording')).toHaveLength(0);
      expect(vi.getTimerCount()).toBe(0);

      await handlers.get('recording:intent-projection')({
        payload: {
          runId: 81,
          intentRevision: 2,
          status: 'Starting',
          desiredOn: true,
          pendingStart: false,
          processingJobs: 1,
          shutdownRequested: false,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 81, status: 'Recording', stopped_via_hotkey: false },
      });
      await start;

      expect(store.status).toBe('Recording');
      expect(store.sessionId).toBe(81);
    } finally {
      vi.useRealTimers();
    }
  });

  it.each([
    ['startFailed', 'terminal start'],
    ['runtimeFailed', 'terminal runtime'],
    ['stopUncertain', 'terminal mic-safety'],
    ['finalizeFailed', 'terminal finalize'],
  ] as const)('закрывает активную сессию при %s fault (%s)', async (fault, _label) => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();
    await handlers.get('recording:status')({
      payload: { session_id: 91, status: 'Recording', stopped_via_hotkey: false },
    });
    expect(store.sessionId).toBe(91);

    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 91,
        intentRevision: 1,
        status: 'Error',
        desiredOn: false,
        pendingStart: false,
        processingJobs: 0,
        shutdownRequested: false,
        fault,
      },
    });

    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
    expect(store.recordingStartPending).toBe(false);
    expect(store.error).toBeTruthy();

    await handlers.get('recording:status')({
      payload: { session_id: 91, status: 'Recording', stopped_via_hotkey: false },
    });
    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
  });

  it.each(['error-first', 'projection-first', 'connection-quota-projection'] as const)(
    'keeps the provider quota error when runtimeFailed arrives %s',
    async (order) => {
      invokeMock.mockResolvedValue(null);
      const { handlers, store } = await initializeStoreWithHandlers();
      const providerError = () => handlers.get('transcription:error')({
        payload: {
          session_id: 501,
          error: 'Provider quota exceeded',
          error_type: 'provider_quota_exceeded',
          error_details: {
            category: 'provider_quota_exceeded',
            serverCode: 'PROVIDER_QUOTA_EXCEEDED',
          },
        },
      });
      const connectionError = () => handlers.get('transcription:error')({
        payload: {
          session_id: 501,
          error: 'Failed to send audio after provider close',
          error_type: 'connection',
          error_details: { category: 'connection' },
        },
      });
      const runtimeFailure = () => handlers.get('recording:intent-projection')({
        payload: {
          runId: 501,
          faultRunId: 501,
          intentRevision: 1,
          status: 'Processing',
          desiredOn: false,
          pendingStart: false,
          processingJobs: 0,
          shutdownRequested: false,
          fault: 'runtimeFailed',
        },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 501, status: 'Recording', stopped_via_hotkey: false },
      });
      if (order === 'error-first') {
        await providerError();
        await runtimeFailure();
      } else if (order === 'projection-first') {
        await runtimeFailure();
        await providerError();
      } else {
        await connectionError();
        await providerError();
        await runtimeFailure();
      }

      expect(store.status).toBe('Error');
      expect(store.sessionId).toBeNull();
      expect(store.errorType).toBe('provider_quota_exceeded');
      expect(store.error).toBeTruthy();
    },
  );

  it.each(['error-first', 'projection-first'] as const)(
    'keeps the connection timeout diagnosis when startFailed arrives %s',
    async (order) => {
      invokeMock.mockResolvedValue(null);
      const { handlers, store } = await initializeStoreWithHandlers();
      await handlers.get('recording:status')({
        payload: { session_id: 502, status: 'Starting', stopped_via_hotkey: false },
      });
      const timeoutError = () => handlers.get('transcription:error')({
        payload: {
          session_id: 502,
          error: 'Connection error: WS connection timeout',
          error_type: 'timeout',
          error_details: { category: 'timeout' },
        },
      });
      const startFailure = () => handlers.get('recording:intent-projection')({
        payload: {
          runId: null,
          faultRunId: 502,
          intentRevision: 1,
          status: 'Error',
          desiredOn: true,
          pendingStart: true,
          processingJobs: 0,
          shutdownRequested: false,
          fault: 'startFailed',
        },
      });
      if (order === 'error-first') {
        await timeoutError();
        await startFailure();
      } else {
        await startFailure();
        await timeoutError();
      }

      expect(store.status).toBe('Error');
      expect(store.sessionId).toBeNull();
      expect(store.errorType).toBe('timeout');
      expect(store.errorFullText).toContain('WS connection timeout');
      expect(store.errorFullText).toContain('"category": "timeout"');
      expect(store.recordingStartPending).toBe(false);
    },
  );

  it.each(['runtimeFailed', 'startFailed'] as const)('does not carry a previous run provider error into a failed new run (%s)', async (fault) => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({
      payload: { session_id: 511, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:error')({
      payload: {
        session_id: 511,
        error: 'Provider quota exceeded',
        error_type: 'provider_quota_exceeded',
        error_details: { category: 'provider_quota_exceeded' },
      },
    });
    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 511,
        faultRunId: 511,
        intentRevision: 1,
        status: 'Processing',
        desiredOn: false,
        pendingStart: false,
        processingJobs: 0,
        shutdownRequested: false,
        fault,
      },
    });

    store.prepareForRustHotkeyStart(false);
    await handlers.get('recording:status')({
      payload: { session_id: 512, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:error')({
      payload: {
        session_id: 512,
        error: 'New run connection failure',
        error_type: 'connection',
        error_details: { category: 'connection' },
      },
    });
    await handlers.get('transcription:error')({
      payload: {
        session_id: 511,
        error: 'Late provider quota error from the previous run',
        error_type: 'provider_quota_exceeded',
        error_details: { category: 'provider_quota_exceeded' },
      },
    });
    expect(store.status).toBe('Error');
    expect(store.errorType).toBe('connection');
    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 512,
        faultRunId: 512,
        intentRevision: 2,
        status: 'Processing',
        desiredOn: false,
        pendingStart: false,
        processingJobs: 0,
        shutdownRequested: false,
        fault,
      },
    });

    expect(store.status).toBe('Error');
    expect(store.errorType).toBe('connection');
  });

  it.each(['startFailed', 'runtimeFailed', 'stopUncertain'] as const)(
    'preserves the old transcript tail when pending successor reports %s',
    async (fault) => {
      invokeMock.mockResolvedValue(null);
      const { handlers, store } = await initializeStoreWithHandlers();
      await handlers.get('recording:status')({
        payload: { session_id: 1, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 1, status: 'Processing', stopped_via_hotkey: true },
      });

      await handlers.get('recording:intent-projection')({
        payload: {
          runId: null,
          faultRunId: 2,
          intentRevision: 2,
          status: 'Error',
          desiredOn: false,
          pendingStart: false,
          processingJobs: 1,
          shutdownRequested: false,
          fault,
        },
      });
      expect(store.status).toBe('Error');
      expect(store.sessionId).toBe(1);
      expect(store.error).toBeTruthy();

      await handlers.get('recording:status')({
        payload: { session_id: 2, status: 'Error', stopped_via_hotkey: false },
      });
      expect(store.sessionId).toBe(1);

      await handlers.get('transcription:final')({
        payload: {
          session_id: 1,
          text: 'old run final tail',
          timestamp: 1,
          start: 0,
          duration: 1,
        },
      });
      expect(store.finalText).toContain('old run final tail');

      await handlers.get('recording:intent-projection')({
        payload: {
          runId: 2,
          intentRevision: 2,
          status: 'Processing',
          desiredOn: false,
          pendingStart: false,
          processingJobs: 1,
          shutdownRequested: false,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 1, status: 'Idle', stopped_via_hotkey: true },
      });
      expect(store.status).toBe('Error');
      expect(store.sessionId).toBe(1);
      expect(store.error).toBeTruthy();
    },
  );

  it('requires actual typed capture readiness, independently from offered capability and event generation', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    handlers.get('recording:intent-projection')({payload: {intentRevision: 1, runId: 2, logicalRunId: 1, continuationPhase: 'continue_pending', desiredOn: true, pendingStart: true, status: 'Processing'}});
    const readiness = {revision: 1, runId: 2, logicalRunId: 1, captureEpisodeId: 2, captureGeneration: 8, state: 'buffering', reason: 'connecting-provider', generation: 1, captureReady: false, transportReady: false, offeredContinuation: true};
    handlers.get('recording:capture-readiness')({payload: readiness});
    expect(store.isCaptureReady).toBe(false);
    handlers.get('recording:capture-readiness')({payload: {...readiness, generation: 2, captureReady: 'true'}});
    expect(store.isCaptureReady).toBe(false);
    expect(store.captureReadiness?.generation).toBe(1);
    handlers.get('recording:capture-readiness')({payload: {...readiness, generation: 3, captureReady: true}});
    expect(store.isCaptureReady).toBe(true);
    expect(store.captureRunId).toBe(2);
    expect(store.captureGeneration).toBe(8);
    expect(store.captureReadiness?.transportReady).toBe(false);
    store.cleanup();
  });

  it('accepts warm activation only as unavailable and rejects stale activation after stop', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    const intent = { intentRevision: 1, runId: 2, desiredOn: true, pendingStart: true, status: 'Starting' };
    handlers.get('recording:intent-projection')({ payload: intent });
    const warm = { revision: 1, runId: 2, state: 'unavailable',
      reason: 'activating-warm-capture', generation: 1, captureReady: false, transportReady: false };
    handlers.get('recording:capture-readiness')({ payload: warm });
    expect(store.captureReadiness?.reason).toBe('activating-warm-capture');
    expect(store.isCaptureReady).toBe(false);
    for (const invalid of [{ captureReady: true }, { transportReady: true }, { state: 'buffering' },
      { revision: null }, { runId: null }]) {
      handlers.get('recording:capture-readiness')({ payload: { ...warm, generation: 2, ...invalid } });
      expect(store.captureReadiness?.generation).toBe(1);
      expect(store.isCaptureReady).toBe(false);
    }
    handlers.get('recording:intent-projection')({ payload: {
      ...intent, intentRevision: 2, desiredOn: false, pendingStart: false, status: 'Idle',
    } });
    handlers.get('recording:capture-readiness')({ payload: { ...warm, generation: 3 } });
    expect(store.captureReadiness).toBeNull();
    expect(store.isCaptureReady).toBe(false);
    store.cleanup();
  });

  it('fences capture readiness by intent revision independently from transcript session', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();

    await handlers.get('recording:status')({
      payload: { session_id: 90, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 8,
        runId: 101,
        state: 'buffering',
        reason: 'finalizing-previous',
        generation: 1,
      },
    });
    expect(store.isCaptureReady).toBe(false);

    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 90,
        intentRevision: 8,
        status: 'Processing',
        desiredOn: true,
        pendingStart: true,
        processingJobs: 1,
        shutdownRequested: false,
      },
    });
    await handlers.get('recording:status')({
      payload: { session_id: 90, status: 'Processing', stopped_via_hotkey: true },
    });

    expect(store.sessionId).toBe(90);
    expect(store.captureRunId).toBe(101);
    expect(store.captureReadiness?.reason).toBe('finalizing-previous');
    expect(store.isCaptureReady).toBe(true);

    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 7,
        runId: 89,
        state: 'unavailable',
        reason: 'error',
        generation: 2,
      },
    });
    await handlers.get('recording:intent-projection')({
      payload: {
        runId: null,
        intentRevision: 7,
        status: 'Idle',
        desiredOn: false,
        pendingStart: false,
        processingJobs: 0,
        shutdownRequested: false,
      },
    });

    expect(store.recordingIntentRevision).toBe(8);
    expect(store.isCaptureReady).toBe(true);
  });

  it('cannot resurrect a cancelled capture with delayed readiness', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();

    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 120,
        intentRevision: 20,
        status: 'Starting',
        desiredOn: true,
        pendingStart: true,
        processingJobs: 0,
        shutdownRequested: false,
      },
    });
    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 20,
        runId: 121,
        state: 'buffering',
        reason: 'connecting-provider',
        generation: 5,
      },
    });
    expect(store.isCaptureReady).toBe(true);

    await handlers.get('recording:intent-projection')({
      payload: {
        runId: null,
        intentRevision: 21,
        status: 'Idle',
        desiredOn: false,
        pendingStart: false,
        processingJobs: 0,
        shutdownRequested: false,
      },
    });
    expect(store.captureReadiness).toBeNull();
    expect(store.isCaptureReady).toBe(false);

    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 20,
        runId: 121,
        state: 'streaming',
        reason: 'recording',
        generation: 6,
      },
    });
    expect(store.captureReadiness).toBeNull();
    expect(store.isCaptureReady).toBe(false);
  });

  it('hydrates missed intent state from the authoritative readiness getter', async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'get_recording_capture_readiness') {
        return {
          revision: 30,
          runId: 131,
          state: 'buffering',
          reason: 'connecting-provider',
          generation: 10,
        };
      }
      return null;
    });

    const { store } = await initializeStoreWithHandlers();
    await flushMicrotasks();

    expect(store.recordingDesiredOn).toBe(true);
    expect(store.recordingIntentRevision).toBe(30);
    expect(store.captureRunId).toBe(131);
    expect(store.isCaptureReady).toBe(true);
  });

  it('hydrates from an equal-generation getter after the event wins the race', async () => {
    const getter = deferred<any>();
    const snapshot = {
      revision: 31,
      runId: 132,
      state: 'buffering',
      reason: 'connecting-provider',
      generation: 11,
    };
    invokeMock.mockImplementation((command: string) =>
      command === 'get_recording_capture_readiness' ? getter.promise : Promise.resolve(null)
    );
    const { handlers, store } = await initializeStoreWithHandlers();

    await handlers.get('recording:capture-readiness')({ payload: snapshot });
    expect(store.isCaptureReady).toBe(false);
    getter.resolve(snapshot);
    await flushMicrotasks();

    expect(store.recordingIntentRevision).toBe(31);
    expect(store.captureRunId).toBe(132);
    expect(store.isCaptureReady).toBe(true);
  });

  it('does not erase a known Off revision with an idle null-revision snapshot', async () => {
    let readinessSnapshot: any = null;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'get_recording_status') return 'Idle';
      if (command === 'get_recording_capture_readiness') return readinessSnapshot;
      return null;
    });
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:intent-projection')({
      payload: {
        runId: null,
        intentRevision: 21,
        status: 'Idle',
        desiredOn: false,
        pendingStart: false,
        processingJobs: 0,
        shutdownRequested: false,
      },
    });
    readinessSnapshot = {
      revision: null,
      runId: null,
      state: 'unavailable',
      reason: 'idle',
      generation: 12,
    };
    await store.reconcileBackendStatus('idle_snapshot');
    expect(store.recordingIntentRevision).toBe(21);

    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 20,
        intentRevision: 20,
        status: 'Starting',
        desiredOn: true,
        pendingStart: true,
        processingJobs: 0,
        shutdownRequested: false,
      },
    });
    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 20,
        runId: 20,
        state: 'streaming',
        reason: 'recording',
        generation: 13,
      },
    });
    expect(store.recordingDesiredOn).toBe(false);
    expect(store.recordingIntentRevision).toBe(21);
    expect(store.isCaptureReady).toBe(false);
  });

  it('does not apply an older Starting snapshot after Recording arrives during readiness refresh', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    const readiness = deferred<any>();
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_recording_status') return Promise.resolve('Starting');
      if (command === 'get_recording_capture_readiness') return readiness.promise;
      return Promise.resolve(null);
    });

    const reconcile = store.reconcileBackendStatus('window_shown');
    await flushMicrotasks();
    await handlers.get('recording:status')({
      payload: { session_id: 4, status: 'Recording', stopped_via_hotkey: true },
    });
    readiness.resolve(null);

    expect(await reconcile).toBeNull();
    expect(store.status).toBe('Recording');
    expect(store.sessionId).toBe(4);
    expect(store.lastAcceptedRecordingStatus).toEqual({
      session_id: 4,
      status: 'Recording',
      stopped_via_hotkey: true,
    });
  });

  it('applies authoritative Idle after readiness clears a provisional start', async () => {
    invokeMock.mockResolvedValue(null);
    const { store } = await initializeStoreWithHandlers();
    store.prepareForRustHotkeyStart(false);
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'get_recording_status') return 'Idle';
      if (command === 'get_recording_capture_readiness') {
        return {
          revision: null,
          runId: null,
          state: 'unavailable',
          reason: 'idle',
          generation: 1,
        };
      }
      return null;
    });

    expect(await store.reconcileBackendStatus('window_shown')).toBe('Idle');
    expect(store.status).toBe('Idle');
    expect(store.recordingDesiredOn).toBe(false);
    expect(store.isCaptureReady).toBe(false);
  });

  it('settles a no-session provisional start when readiness turns Off during reconciliation', async () => {
    invokeMock.mockResolvedValue(null);
    const { store } = await initializeStoreWithHandlers();
    store.prepareForRustHotkeyStart(false);
    const readiness = deferred<any>();
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_recording_status') return Promise.resolve('Starting');
      if (command === 'get_recording_capture_readiness') return readiness.promise;
      return Promise.resolve(null);
    });

    const reconcile = store.reconcileBackendStatus('window_shown');
    await flushMicrotasks();
    readiness.resolve({
      revision: 9,
      runId: null,
      state: 'unavailable',
      reason: 'cancelled',
      generation: 9,
    });

    expect(await reconcile).toBe('Idle');
    expect(store.status).toBe('Idle');
    expect(store.sessionId).toBeNull();
    expect(store.recordingDesiredOn).toBe(false);
  });

  it('does not close an old transcript session when readiness turns Off during reconciliation', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({
      payload: { session_id: 170, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('recording:status')({
      payload: { session_id: 170, status: 'Processing', stopped_via_hotkey: true },
    });
    const readiness = deferred<any>();
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_recording_status') return Promise.resolve('Recording');
      if (command === 'get_recording_capture_readiness') return readiness.promise;
      return Promise.resolve(null);
    });

    const reconcile = store.reconcileBackendStatus('window_shown');
    await flushMicrotasks();
    readiness.resolve({
      revision: 10,
      runId: null,
      state: 'unavailable',
      reason: 'idle',
      generation: 10,
    });

    expect(await reconcile).toBeNull();
    expect(store.status).toBe('Processing');
    expect(store.sessionId).toBe(170);
    await handlers.get('transcription:final')({
      payload: {
        session_id: 170,
        text: 'preserved old tail',
        timestamp: 1,
        start: 0,
        duration: 1,
      },
    });
    expect(store.finalText).toContain('preserved old tail');
  });

  it('does not let a stale revision poison readiness generation fencing', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 140,
        intentRevision: 40,
        status: 'Starting',
        desiredOn: true,
        pendingStart: true,
        processingJobs: 0,
        shutdownRequested: false,
      },
    });
    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 39,
        runId: 139,
        state: 'unavailable',
        reason: 'error',
        generation: 11,
      },
    });
    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 40,
        runId: 141,
        state: 'buffering',
        reason: 'recording',
        generation: 10,
      },
    });

    expect(store.captureRunId).toBe(141);
    expect(store.isCaptureReady).toBe(true);
  });

  it('targets pending capture run on stop while preserving old transcript ownership', async () => {
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === 'stop_recording') {
        expect(args).toEqual({ expectedSessionId: 151 });
        return 'Recording stop requested';
      }
      return null;
    });
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({
      payload: { session_id: 150, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('recording:intent-projection')({
      payload: {
        runId: 150,
        intentRevision: 50,
        status: 'Processing',
        desiredOn: true,
        pendingStart: true,
        processingJobs: 1,
        shutdownRequested: false,
      },
    });
    await handlers.get('recording:capture-readiness')({
      payload: {
        revision: 50,
        runId: 151,
        state: 'buffering',
        reason: 'finalizing-previous',
        generation: 1,
      },
    });

    await store.stopRecording('cancel_pending_capture');

    expect(store.sessionId).toBe(150);
    expect(store.status).toBe('Processing');
  });

  it('settles a cancelled provisional native session without waiting for an Idle status', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:capture-readiness')({ payload: {
      revision: 64, runId: 32, state: 'unavailable', reason: 'starting-capture', generation: 1,
    } });
    await handlers.get('recording:intent-projection')({ payload: {
      runId: 32, intentRevision: 64, status: 'Starting', desiredOn: true,
      pendingStart: true, processingJobs: 0, shutdownRequested: false,
    } });
    await handlers.get('recording:status')({
      payload: { session_id: 32, status: 'Starting', stopped_via_hotkey: true },
    });
    expect(store.status).toBe('Starting');
    expect(store.sessionId).toBe(32);

    await handlers.get('recording:capture-readiness')({ payload: {
      revision: 65, runId: 32, state: 'unavailable', reason: 'cancelled', generation: 2,
    } });
    await handlers.get('recording:intent-projection')({ payload: {
      runId: 32, intentRevision: 65, status: 'Starting', desiredOn: false,
      pendingStart: false, processingJobs: 0, shutdownRequested: false,
    } });

    expect(store.status).toBe('Idle');
    expect(store.sessionId).toBeNull();
    await handlers.get('recording:status')({
      payload: { session_id: 32, status: 'Starting', stopped_via_hotkey: true },
    });
    expect(store.status).toBe('Idle');
    expect(store.sessionId).toBeNull();
  });

  it('preserves a real recording tail when UI missed Recording before its Off projection', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:intent-projection')({ payload: {
      runId: 33, intentRevision: 66, status: 'Starting', desiredOn: true,
      pendingStart: false, processingJobs: 0, shutdownRequested: false,
    } });
    await handlers.get('recording:status')({
      payload: { session_id: 33, status: 'Starting', stopped_via_hotkey: true },
    });
    expect(store.status).toBe('Starting');

    await handlers.get('recording:intent-projection')({ payload: {
      runId: 33, intentRevision: 67, status: 'Processing', desiredOn: false,
      pendingStart: false, processingJobs: 1, shutdownRequested: false,
    } });
    expect(store.sessionId).toBe(33);

    await handlers.get('transcription:final')({ payload: {
      session_id: 33, text: 'recording tail after delayed status', timestamp: 1, start: 0, duration: 1,
    } });
    expect(store.finalText).toContain('recording tail after delayed status');
  });

  it('keeps the previous transcript session open until its final tail arrives', async () => {
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();
    await handlers.get('recording:status')({
      payload: { session_id: 160, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('recording:status')({
      payload: { session_id: 160, status: 'Processing', stopped_via_hotkey: true },
    });

    store.prepareForRustHotkeyStart(false);
    expect(store.sessionId).toBe(160);

    await handlers.get('transcription:final')({
      payload: {
        session_id: 160,
        text: 'previous transcript tail',
        timestamp: 1,
        start: 0,
        duration: 1,
      },
    });
    expect(store.finalText).toContain('previous transcript tail');

    await handlers.get('recording:status')({
      payload: { session_id: 161, status: 'Starting', stopped_via_hotkey: false },
    });
    expect(store.sessionId).toBe(161);
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

  it('поздняя старая initialize не затирает unlisten нового listener', async () => {
    const staleListen = deferred<() => void>();
    const staleUnlisten = vi.fn();
    const currentPartialUnlisten = vi.fn();
    let listenCall = 0;
    listenMock.mockImplementation(() => {
      listenCall += 1;
      if (listenCall === 1) return staleListen.promise;
      if (listenCall === 2) return Promise.resolve(currentPartialUnlisten);
      return Promise.resolve(vi.fn());
    });
    invokeMock.mockImplementation((command: string) => {
      if (command === 'get_incoming_translation_state') {
        return Promise.resolve({ session_id: 0, status: 'Idle' });
      }
      return Promise.resolve(null);
    });
    const store = useTranscriptionStore();

    const staleInitialize = store.initialize();
    for (let i = 0; i < 20 && listenMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }
    expect(listenMock).toHaveBeenCalledTimes(1);

    const currentInitialize = store.initialize();
    await currentInitialize;
    expect(listenMock.mock.calls.length).toBeGreaterThan(2);

    staleListen.resolve(staleUnlisten);
    await staleInitialize;
    expect(staleUnlisten).toHaveBeenCalledTimes(1);

    store.cleanup();
    expect(currentPartialUnlisten).toHaveBeenCalledTimes(1);
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

  it('не считает warm-start доказательством готовности capture', () => {
    const store = useTranscriptionStore();
    store.finalText = 'старый текст';
    store.accumulatedText = 'старый хвост';
    store.partialText = 'старый partial';
    appConfigMock.recordingMode = 'live_translation';

    store.prepareForRustHotkeyStart(true);

    expect(store.status).toBe('Starting');
    expect(store.isRecording).toBe(false);
    expect(store.isStarting).toBe(true);
    expect(store.isCaptureReady).toBe(false);
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
    expect(store.closedSessionIdFloor).toBeLessThan(91);
    expect(store.translationText).toBe('before error');
  });

  it('terminal transcription:error закрывает session даже без recording:status=Error', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 701, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:error')({
      payload: {
        session_id: 701,
        error: 'Provider quota exceeded',
        error_type: 'provider_quota_exceeded',
        error_details: { category: 'provider_quota_exceeded' },
      },
    });
    await handlers.get('transcription:partial')({
      payload: {
        session_id: 701,
        text: 'late partial after terminal error',
        timestamp: 2,
        is_segment_final: false,
        start: 0,
        duration: 1,
      },
    });
    await handlers.get('recording:status')({
      payload: { session_id: 701, status: 'Recording', stopped_via_hotkey: false },
    });

    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeLessThan(701);
    expect(store.partialText).toBe('');
    expect(store.errorType).toBe('provider_quota_exceeded');
  });

  it('late usage lookup старой limit error не перезаписывает новую Recording session', async () => {
    const usage = deferred<{
      licenses: Array<{
        status: string;
        plan: string;
        seconds_used: number;
        seconds_limit: number;
      }>;
    }>();
    apiClientMock.get.mockReturnValue(usage.promise);
    invokeMock.mockResolvedValue(null);
    const { handlers, store } = await initializeStoreWithHandlers();

    await handlers.get('recording:status')({
      payload: { session_id: 711, status: 'Recording', stopped_via_hotkey: false },
    });
    const staleError = handlers.get('transcription:error')({
      payload: {
        session_id: 711,
        error: 'Monthly limit exceeded',
        error_type: 'limit_exceeded',
        error_details: { category: 'limit_exceeded' },
      },
    });
    await flushMicrotasks();
    expect(apiClientMock.get).toHaveBeenCalledWith('/api/v1/account/licenses');

    await handlers.get('recording:status')({
      payload: { session_id: 712, status: 'Recording', stopped_via_hotkey: false },
    });
    usage.resolve({
      licenses: [{
        status: 'active',
        plan: 'pro',
        seconds_used: 3_600,
        seconds_limit: 7_200,
      }],
    });
    await staleError;

    expect(store.sessionId).toBe(712);
    expect(store.status).toBe('Recording');
    expect(store.errorType).toBeNull();
  });

  it('показывает INTERNAL_ERROR как ошибку обработки, а не как перезапуск сервера', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 702, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:error')({
      payload: {
        session_id: 702,
        error: 'Connection error: Внутренняя ошибка сервера',
        error_type: 'connection',
        error_details: {
          category: 'server_error',
          serverCode: 'INTERNAL_ERROR',
        },
      },
    });

    expect(store.status).toBe('Error');
    expect(store.error).toBe('Сервер не смог обработать аудио. Запустите запись ещё раз.');
    expect(store.error).not.toContain('перезапускается');
  });

  it('terminal error новой failed session не закрывает восстановленную меньшую session', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    store.prepareForRustHotkeyStart(false);

    await handlers.get('transcription:error')({
      payload: {
        session_id: 11,
        error: 'Provider quota exceeded',
        error_type: 'provider_quota_exceeded',
        error_details: { category: 'provider_quota_exceeded' },
      },
    });

    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeLessThan(10);

    await handlers.get('recording:status')({
      payload: { session_id: 10, status: 'Recording', stopped_via_hotkey: false },
    });

    expect(store.status).toBe('Recording');
    expect(store.sessionId).toBe(10);
    expect(store.errorType).toBeNull();
    expect(store.closedSessionIdFloor).toBeLessThan(10);
  });

  it('connect error закрывает failed attempt session и игнорирует поздний partial', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') return Promise.resolve('Recording started');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    const startPromise = store.startRecording();
    await flushMicrotasks();

    await handlers.get('recording:status')({
      payload: { session_id: 31, status: 'Starting', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:error')({
      payload: {
        session_id: 31,
        error: 'Invalid STT configuration',
        error_type: 'configuration',
      },
    });
    await handlers.get('transcription:partial')({
      payload: {
        session_id: 31,
        text: 'late partial from failed connect attempt',
        timestamp: 2,
        is_segment_final: false,
        start: 0,
        duration: 1,
      },
    });

    await startPromise;

    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeLessThan(31);
    expect(store.partialText).toBe('');
    expect(store.errorType).toBe('configuration');
  });

  it('new start закрывает previous session до adoption поздних transcription events', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_recording') return Promise.resolve('Recording started');
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('recording:status')({
      payload: { session_id: 61, status: 'Recording', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:partial')({
      payload: {
        session_id: 61,
        text: 'old live text',
        timestamp: 1,
        is_segment_final: false,
        start: 0,
        duration: 1,
      },
    });

    const startPromise = store.startRecording();
    await flushMicrotasks();

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 61,
        text: 'late old text after new start',
        timestamp: 2,
        is_segment_final: false,
        start: 1,
        duration: 1,
      },
    });

    expect(store.sessionId).toBeNull();
    expect(store.status).toBe('Starting');
    expect(store.partialText).toBe('');

    await handlers.get('recording:status')({
      payload: { session_id: 62, status: 'Recording', stopped_via_hotkey: false },
    });
    await startPromise;

    expect(store.sessionId).toBe(62);
    expect(store.status).toBe('Recording');
  });

  it('не усыновляет invalid session_id=0 из transcription event во время start', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    store.prepareForRustHotkeyStart(false);

    await handlers.get('transcription:partial')({
      payload: {
        session_id: 0,
        text: 'invalid zero session text',
        timestamp: 1,
        is_segment_final: false,
        start: 0,
        duration: 1,
      },
    });

    expect(store.sessionId).toBeNull();
    expect(store.status).toBe('Starting');
    expect(store.partialText).toBe('');
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

    expect(
      invokeMock.mock.calls.filter((call) => call[0] === 'start_incoming_translation')
    ).toHaveLength(1);
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

  it('incoming translation stop error сохраняет active state и следующий toggle снова делает stop', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'stop_incoming_translation') return Promise.reject('stop failed');
      return Promise.resolve('ok');
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 612, status: 'Recording' },
    });

    await store.toggleIncomingTranslation();

    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationError).toContain('stop failed');

    await store.toggleIncomingTranslation();

    expect(invokeMock.mock.calls.filter((call) => call[0] === 'stop_incoming_translation')).toHaveLength(2);
    expect(invokeMock).not.toHaveBeenCalledWith('start_incoming_translation');
  });

  it('incoming translation stop response loss принимает backend Idle snapshot', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'stop_incoming_translation') return Promise.reject('response channel closed');
      if (cmd === 'get_incoming_translation_state') {
        return Promise.resolve({ session_id: 0, status: 'Idle' });
      }
      return Promise.resolve('ok');
    });

    const store = useTranscriptionStore();
    await store.initialize();
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 614, status: 'Recording' },
    });

    await store.toggleIncomingTranslation();

    expect(store.incomingTranslationStatus).toBe('Idle');
    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.incomingTranslationError).toBeNull();
  });

  it('incoming translation start response loss принимает active backend snapshot', async () => {
    let snapshotCalls = 0;
    listenMock.mockResolvedValue(() => {});
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_incoming_translation') return Promise.reject('response channel closed');
      if (cmd === 'get_incoming_translation_state') {
        snapshotCalls += 1;
        return Promise.resolve(
          snapshotCalls === 1
            ? { session_id: 0, status: 'Idle' }
            : { session_id: 615, status: 'Recording' }
        );
      }
      return Promise.resolve('ok');
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await store.toggleIncomingTranslation();

    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationSessionId).toBe(615);
    expect(store.incomingTranslationError).toBeNull();
  });

  it('incoming translation stop success закрывает session даже если Idle event потерян', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'stop_incoming_translation') return Promise.resolve('Incoming translation stopped');
      return Promise.resolve('ok');
    });

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 613, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 613, text: 'перевод до стопа', timestamp: 1 },
    });

    await store.toggleIncomingTranslation();

    expect(store.incomingTranslationStatus).toBe('Idle');
    expect(store.incomingTranslationSessionId).toBeNull();

    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 613, text: ' поздний хвост', timestamp: 2 },
    });

    expect(store.incomingTranslationText).toBe('перевод до стопа');
  });

  it('incoming translation не применяет старый stop response поверх новой session', async () => {
    const handlers = new Map<string, any>();
    const pendingStop = deferred<string>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'stop_incoming_translation') return pendingStop.promise;
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 613, status: 'Recording' },
    });

    const stop = store.toggleIncomingTranslation();
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 614, status: 'Recording' },
    });
    pendingStop.resolve('Incoming translation stopped');
    await stop;

    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationSessionId).toBe(614);
  });

  it('spoken playback snapshot восстанавливает mute и отбрасывает stale playback events', async () => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string, args?: { muted?: boolean }) => {
      if (cmd === 'get_incoming_translation_state') {
        return Promise.resolve({
          session_id: 701,
          status: 'Recording',
          delivery: 'text_and_audio',
          playback_state: 'playing',
          muted: false,
        });
      }
      if (cmd === 'set_incoming_translation_muted') {
        return Promise.resolve({ session_id: 701, state: 'playing', muted: args?.muted });
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();
    expect(store.incomingTranslationDelivery).toBe('text_and_audio');
    expect(store.incomingTranslationMuted).toBe(false);

    await handlers.get('incoming_translation:playback')({
      payload: { session_id: 700, state: 'playing', muted: true },
    });
    expect(store.incomingTranslationMuted).toBe(false);

    await store.toggleIncomingTranslationMute();
    expect(invokeMock).toHaveBeenCalledWith('set_incoming_translation_muted', { muted: true });
    expect(store.incomingTranslationMuted).toBe(true);
  });

  it('incoming translation восстанавливает active backend session после renderer reload', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'get_incoming_translation_state') {
        return Promise.resolve({ session_id: 614, status: 'Recording' });
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    expect(store.incomingTranslationSessionId).toBe(614);
    expect(store.incomingTranslationStatus).toBe('Recording');

    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 614, text: 'восстановленный перевод', timestamp: 1 },
    });

    expect(store.incomingTranslationText).toBe('восстановленный перевод');
  });

  it('incoming translation восстанавливает terminal Error после renderer reload', async () => {
    listenMock.mockResolvedValue(() => {});
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'get_incoming_translation_state') {
        return Promise.resolve({ session_id: 618, status: 'Error' });
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();

    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingTranslationError).toBeTruthy();
  });

  it('incoming translation start success восстанавливает уже active backend session без event', async () => {
    let backendActive = false;

    listenMock.mockResolvedValue(() => {});
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'start_incoming_translation') {
        backendActive = true;
        return Promise.resolve('Incoming translation already running');
      }
      if (cmd === 'get_incoming_translation_state') {
        return Promise.resolve(
          backendActive
            ? { session_id: 617, status: 'Recording' }
            : { session_id: 0, status: 'Idle' }
        );
      }
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    await store.initialize();
    expect(store.incomingTranslationStatus).toBe('Idle');

    await store.toggleIncomingTranslation();

    expect(store.incomingTranslationSessionId).toBe(617);
    expect(store.incomingTranslationStatus).toBe('Recording');
  });

  it('incoming translation сбрасывает stale active state по authoritative Idle snapshot', async () => {
    const handlers = new Map<string, any>();
    let snapshotCall = 0;

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd !== 'get_incoming_translation_state') return Promise.resolve(null);
      snapshotCall += 1;
      return Promise.resolve(
        snapshotCall === 1
          ? { session_id: 616, status: 'Recording' }
          : { session_id: 0, status: 'Idle' }
      );
    });

    const store = useTranscriptionStore();
    await store.initialize();
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 616, text: 'последний перевод', timestamp: 1 },
    });

    await store.initialize();

    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.incomingTranslationStatus).toBe('Idle');
    expect(store.incomingTranslationError).toBeNull();
    expect(store.incomingTranslationText).toBe('последний перевод');
  });

  it('incoming translation не применяет stale snapshot поверх нового status event', async () => {
    const handlers = new Map<string, any>();
    const snapshot = deferred<{ session_id: number; status: string }>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === 'get_incoming_translation_state') return snapshot.promise;
      return Promise.resolve(null);
    });

    const store = useTranscriptionStore();
    const initialize = store.initialize();
    await vi.waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('get_incoming_translation_state');
    });

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 615, status: 'Processing' },
    });
    snapshot.resolve({ session_id: 615, status: 'Recording' });
    await initialize;

    expect(store.incomingTranslationStatus).toBe('Processing');
  });

  it('incoming translation игнорирует invalid session_id=0 events', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 0, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 0, text: 'invalid source', timestamp: 1 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 0, text: 'invalid translation', timestamp: 2 },
    });
    await handlers.get('incoming_translation:error')({
      payload: { session_id: 0, error: 'invalid auth error', error_type: 'authentication' },
    });

    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.incomingTranslationStatus).toBe('Idle');
    expect(store.incomingSourceText).toBe('');
    expect(store.incomingTranslationText).toBe('');
    expect(store.incomingTranslationError).toBeNull();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 614, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 614, text: 'valid source', timestamp: 3 },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 614, text: 'валидный перевод', timestamp: 4 },
    });

    expect(store.incomingTranslationSessionId).toBe(614);
    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingSourceText).toBe('valid source');
    expect(store.incomingTranslationText).toBe('валидный перевод');
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

  it('склеивает realtime incoming translation без пробелов внутри слов и перед пунктуацией', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 202, status: 'Recording' },
    });
    for (const text of ['Она об', 'ер', 'нулась', ', мол', ', ей нравится', '.']) {
      await handlers.get('incoming_translation:delta')({
        payload: {
          session_id: 202,
          text,
          timestamp: 1,
          delivery: 'text_and_audio',
        },
      });
    }

    for (const text of ['The call', 'er stopped', ' speaking.']) {
      await handlers.get('incoming_translation:source-final')({
        payload: {
          session_id: 202,
          text,
          timestamp: 1,
          delivery: 'text_and_audio',
        },
      });
    }

    expect(store.incomingTranslationText).toBe('Она обернулась, мол, ей нравится.');
    expect(store.incomingSourceText).toBe('The caller stopped speaking.');
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

    expect(invokeMock).toHaveBeenCalledWith('start_recording', { clientStartId: expect.any(String) });
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

  it('incoming translation помнит больше 128 последовательных закрытых sessions без блокировки нового меньшего id', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    for (let sessionId = 1_000; sessionId < 1_130; sessionId += 1) {
      await handlers.get('incoming_translation:status')({
        payload: { session_id: sessionId, status: 'Recording' },
      });
      await handlers.get('incoming_translation:status')({
        payload: { session_id: sessionId, status: 'Idle' },
      });
    }

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 1_000, status: 'Recording' },
    });
    expect(store.incomingTranslationSessionId).toBeNull();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 42, status: 'Recording' },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 42, text: 'fresh lower session', timestamp: 1 },
    });

    expect(store.incomingTranslationSessionId).toBe(42);
    expect(store.incomingTranslationText).toBe('fresh lower session');
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
    expect(store.incomingTranslationSessionId).toBeNull();
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

  it('incoming translation error event сам завершает session даже без status event', async () => {
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

    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingTranslationError).toContain('temporary network blip');
    expect(store.incomingTranslationText).toBe('первый перевод');
    expect(store.isIncomingTranslationActive).toBe(false);

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

  it('incoming translation уточняет terminal error при status-before-error и игнорирует старую ошибку после нового start', async () => {
    const handlers = new Map<string, any>();

    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 603, status: 'Recording' },
    });
    await handlers.get('incoming_translation:status')({
      payload: { session_id: 603, status: 'Error' },
    });

    expect(store.incomingTranslationSessionId).toBeNull();
    expect(store.incomingTranslationStatus).toBe('Error');
    expect(store.incomingTranslationError).toBeTruthy();

    await handlers.get('incoming_translation:error')({
      payload: { session_id: 603, error: 'capture stream failed', error_type: 'connection' },
    });
    expect(store.incomingTranslationError).toBe('capture stream failed');

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 604, status: 'Recording' },
    });
    await handlers.get('incoming_translation:error')({
      payload: { session_id: 603, error: 'late old error', error_type: 'connection' },
    });

    expect(store.incomingTranslationSessionId).toBe(604);
    expect(store.incomingTranslationStatus).toBe('Recording');
    expect(store.incomingTranslationError).toBeNull();
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

  it('background Error после stop закрывает session и игнорирует поздний partial', async () => {
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
      payload: { session_id: 23, status: 'Recording', stopped_via_hotkey: false },
    });

    const stopPromise = store.stopRecording();
    expect(store.status).toBe('Processing');

    await handlers.get('recording:status')({
      payload: { session_id: 23, status: 'Error', stopped_via_hotkey: false },
    });
    await handlers.get('transcription:partial')({
      payload: {
        session_id: 23,
        text: 'late partial after background error',
        timestamp: 2,
        is_segment_final: false,
        start: 0,
        duration: 1,
      },
    });

    await stopPromise;

    expect(store.status).toBe('Idle');
    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeGreaterThanOrEqual(23);
    expect(store.partialText).toBe('');
    expect(store.error).toBeNull();
  });

  it('background transcription error после stop закрывает session и игнорирует поздний текст', async () => {
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
      payload: { session_id: 24, status: 'Recording', stopped_via_hotkey: false },
    });

    const stopPromise = store.stopRecording();
    expect(store.status).toBe('Processing');

    await handlers.get('transcription:error')({
      payload: {
        session_id: 24,
        error: 'provider closed after stop',
        error_type: 'connection',
      },
    });
    await handlers.get('transcription:partial')({
      payload: {
        session_id: 24,
        text: 'late partial after background transcription error',
        timestamp: 2,
        is_segment_final: false,
        start: 0,
        duration: 1,
      },
    });
    await handlers.get('transcription:final')({
      payload: {
        session_id: 24,
        text: 'late final after background transcription error',
        timestamp: 3,
      },
    });

    await stopPromise;

    expect(store.status).toBe('Idle');
    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeGreaterThanOrEqual(24);
    expect(store.partialText).toBe('');
    expect(store.finalText).toBe('');
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
      ['auto_paste_text', { text: 'Ты уверен, что так будет надёжно,', sessionId: 18 }],
      ['auto_paste_text', { text: ' фокусироваться и не сломается?', sessionId: 18 }],
    ]);
    expect(store.finalText).toBe('Ты уверен, что так будет надёжно, фокусироваться и не сломается?');
  });

  it('provider-neutral partial correction commits immutable history and pastes each segment once', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });
      invokeMock.mockImplementation((command: string) => {
        if (command === 'get_recording_status') return Promise.resolve('Recording');
        return Promise.resolve(null);
      });

      const store = useTranscriptionStore();
      await store.initialize();

      await handlers.get('recording:status')({
        payload: { session_id: 19, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 19,
          text: 'The quick brown fax',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 0.8,
        },
      });
      await vi.advanceTimersByTimeAsync(200);

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 19,
          text: 'The quick brown fox',
          timestamp: 2,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });

      expect(store.partialText).toBe('The quick brown fox');
      expect(store.visiblePartialText).toBe('The quick brown fox');
      expect(store.accumulatedText).toBe('');

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 19,
          text: 'The quick brown fox',
          timestamp: 3,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });

      expect(store.accumulatedText).toBe('The quick brown fox');
      expect(store.partialText).toBe('');

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 19,
          text: 'The quick brown fox jumps over',
          timestamp: 4,
          is_segment_final: false,
          start: 1,
          duration: 0.6,
        },
      });

      expect(store.accumulatedText).toBe('The quick brown fox');
      expect(store.partialText).toBe('The quick brown fox jumps over');
      expect(store.displayText).toBe('The quick brown fox jumps over');

      await handlers.get('transcription:partial')({
        payload: {
          session_id: 19,
          text: 'jumps over',
          timestamp: 5,
          is_segment_final: true,
          start: 1,
          duration: 0.6,
        },
      });
      await handlers.get('transcription:final')({
        payload: { session_id: 19, text: '', timestamp: 6 },
      });

      await store.stopRecording('manual_test');
      await handlers.get('recording:status')({
        payload: { session_id: 19, status: 'Idle', stopped_via_hotkey: false },
      });
      await vi.advanceTimersByTimeAsync(500);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'The quick brown fox', sessionId: 19 }],
        ['auto_paste_text', { text: ' jumps over', sessionId: 19 }],
      ]);
    } finally {
      vi.useRealTimers();
    }
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
      ['auto_paste_text', { text: 'Первый кусок', sessionId: 32 }],
      ['auto_paste_text', { text: ' второй кусок', sessionId: 32 }],
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
      ['auto_paste_text', { text: 'Первый кусок', sessionId: 33 }],
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
      ['auto_paste_text', { text: 'Первый кусок', sessionId: 33 }],
      ['auto_paste_text', { text: ' второй кусок', sessionId: 33 }],
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
      ['auto_paste_text', { text: 'Старый текст', sessionId: 34 }],
      ['auto_paste_text', { text: 'Новый текст', sessionId: 35 }],
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
        ['auto_paste_text', { text: 'Первый кусок', sessionId: 36 }],
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
        ['auto_paste_text', { text: 'Первый кусок', sessionId: 36 }],
        ['auto_paste_text', { text: ' чистовой хвост', sessionId: 36 }],
      ]);
      expect(pasteCalls).not.toContainEqual(['auto_paste_text', { text: ' сырой хвост', sessionId: 36 }]);
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
        ['auto_paste_text', { text: 'последний распознанный текст', sessionId: 37 }],
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('быстрый restart вставляет pending tail с target старой сессии', async () => {
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
        payload: { session_id: 41, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 41,
          text: 'хвост старой сессии',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 41, status: 'Idle', stopped_via_hotkey: true },
      });

      await handlers.get('recording:status')({
        payload: { session_id: 42, status: 'Recording', stopped_via_hotkey: false },
      });
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'хвост старой сессии', sessionId: 41 }],
      ]);
      expect(store.sessionId).toBe(42);
      expect(store.status).toBe('Recording');
    } finally {
      vi.useRealTimers();
    }
  });

  it('delayed cleanup старой вставки не сбрасывает новую сессию', async () => {
    vi.useFakeTimers();
    const oldPaste = deferred<null>();
    try {
      const handlers = new Map<string, any>();
      appConfigMock.autoPasteText = true;

      listenMock.mockImplementation(async (eventName: string, handler: any) => {
        handlers.set(eventName, handler);
        return () => {};
      });
      let pasteCount = 0;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd !== 'auto_paste_text') return Promise.resolve(null);
        pasteCount += 1;
        return pasteCount === 1 ? oldPaste.promise : Promise.resolve(null);
      });

      const store = useTranscriptionStore();
      await store.initialize();
      await handlers.get('recording:status')({
        payload: { session_id: 51, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 51,
          text: 'старый хвост',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 51, status: 'Idle', stopped_via_hotkey: true },
      });
      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      await handlers.get('recording:status')({
        payload: { session_id: 52, status: 'Recording', stopped_via_hotkey: false },
      });
      const newFinal = handlers.get('transcription:partial')({
        payload: {
          session_id: 52,
          text: 'новый текст',
          timestamp: 2,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });
      await flushMicrotasks();

      expect(store.sessionId).toBe(52);
      expect(store.status).toBe('Recording');
      expect(store.accumulatedText).toBe('новый текст');

      oldPaste.resolve(null);
      await newFinal;
      await flushMicrotasks();

      expect(store.sessionId).toBe(52);
      expect(store.status).toBe('Recording');
      expect(store.accumulatedText).toBe('новый текст');
      expect(invokeMock.mock.calls.filter((call) => call[0] === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'старый хвост', sessionId: 51 }],
        ['auto_paste_text', { text: 'новый текст', sessionId: 52 }],
      ]);
    } finally {
      oldPaste.resolve(null);
      vi.useRealTimers();
    }
  });

  it('UI start синхронно сохраняет pending Idle tail и не ждёт старую вставку', async () => {
    vi.useFakeTimers();
    const oldPaste = deferred<null>();
    try {
      appConfigMock.autoPasteText = true;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === 'auto_paste_text') return oldPaste.promise;
        return Promise.resolve(null);
      });
      const { handlers, store } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 53, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 53,
          text: 'tail before button restart',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 53, status: 'Idle', stopped_via_hotkey: false },
      });

      const start = store.startRecording();
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'tail before button restart', sessionId: 53 }],
      ]);
      expect(store.status).toBe('Starting');

      await handlers.get('recording:status')({
        payload: { session_id: 54, status: 'Recording', stopped_via_hotkey: false },
      });
      oldPaste.resolve(null);
      await start;

      expect(store.sessionId).toBe(54);
      expect(store.status).toBe('Recording');
    } finally {
      oldPaste.resolve(null);
      vi.useRealTimers();
    }
  });

  it('stale high-session finalizer не закрывает восстановленную lower session', async () => {
    vi.useFakeTimers();
    const oldPaste = deferred<null>();
    try {
      appConfigMock.autoPasteText = true;
      invokeMock.mockImplementation((cmd: string) =>
        cmd === 'auto_paste_text' ? oldPaste.promise : Promise.resolve(null)
      );
      const { handlers, store } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 500, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 500,
          text: 'high session tail',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 500, status: 'Idle', stopped_via_hotkey: true },
      });
      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      // Native failed-start recovery can restore an older displaced owner without
      // replaying a monotonic recording:status event through this listener.
      store.sessionId = 400;
      oldPaste.resolve(null);
      await flushMicrotasks();
      await flushMicrotasks();

      expect(store.sessionId).toBe(400);
      expect(store.closedSessionIdFloor).toBeLessThan(400);
    } finally {
      oldPaste.resolve(null);
      vi.useRealTimers();
    }
  });

  it('stop snapshot сохраняет auto-paste intent пока auto-copy ждёт IPC', async () => {
    vi.useFakeTimers();
    const oldCopy = deferred<null>();
    try {
      appConfigMock.autoCopyToClipboard = true;
      appConfigMock.autoPasteText = true;
      invokeMock.mockImplementation((cmd: string) =>
        cmd === 'copy_to_clipboard_native' ? oldCopy.promise : Promise.resolve(null)
      );
      const { handlers } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 61, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 61,
          text: 'copy delayed tail',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 61, status: 'Idle', stopped_via_hotkey: true },
      });
      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      appConfigMock.autoPasteText = false;
      oldCopy.resolve(null);
      await flushMicrotasks();
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'copy delayed tail', sessionId: 61 }],
      ]);
    } finally {
      oldCopy.resolve(null);
      vi.useRealTimers();
    }
  });

  it('speech-final fallback copy блокирует paste новой session в общей delivery queue', async () => {
    vi.useFakeTimers();
    const fallbackCopy = deferred<null>();
    try {
      appConfigMock.autoCopyToClipboard = true;
      appConfigMock.autoPasteText = true;
      let pasteCount = 0;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === 'auto_paste_text') {
          pasteCount += 1;
          return pasteCount === 1
            ? Promise.reject(new Error('first native paste failed'))
            : Promise.resolve(null);
        }
        if (cmd === 'copy_to_clipboard_native') return fallbackCopy.promise;
        return Promise.resolve(null);
      });
      const { handlers } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 62, status: 'Recording', stopped_via_hotkey: false },
      });
      const oldFinal = handlers.get('transcription:final')({
        payload: {
          session_id: 62,
          text: 'old fallback',
          timestamp: 1,
          start: 0,
          duration: 1,
        },
      });
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) =>
        cmd === 'auto_paste_text' || cmd === 'copy_to_clipboard_native'
      )).toEqual([
        ['auto_paste_text', { text: 'old fallback', sessionId: 62 }],
        ['copy_to_clipboard_native', { text: 'old fallback' }],
      ]);

      await handlers.get('recording:status')({
        payload: { session_id: 63, status: 'Recording', stopped_via_hotkey: false },
      });
      const newSegment = handlers.get('transcription:partial')({
        payload: {
          session_id: 63,
          text: 'new delivery',
          timestamp: 2,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });
      await flushMicrotasks();
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toHaveLength(1);

      fallbackCopy.resolve(null);
      await Promise.all([oldFinal, newSegment]);

      expect(invokeMock.mock.calls.filter(([cmd]) =>
        cmd === 'auto_paste_text' || cmd === 'copy_to_clipboard_native'
      )).toEqual([
        ['auto_paste_text', { text: 'old fallback', sessionId: 62 }],
        ['copy_to_clipboard_native', { text: 'old fallback' }],
        ['auto_paste_text', { text: 'new delivery', sessionId: 63 }],
      ]);
    } finally {
      fallbackCopy.resolve(null);
      vi.useRealTimers();
    }
  });

  it('успешная in-flight вставка обновляет old ledger до расчёта restart tail', async () => {
    vi.useFakeTimers();
    const firstPaste = deferred<null>();
    try {
      appConfigMock.autoCopyToClipboard = true;
      appConfigMock.autoPasteText = true;
      let pasteCount = 0;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd !== 'auto_paste_text') return Promise.resolve(null);
        pasteCount += 1;
        return pasteCount === 1 ? firstPaste.promise : Promise.resolve(null);
      });
      const { handlers } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 71, status: 'Recording', stopped_via_hotkey: false },
      });
      const firstSegment = handlers.get('transcription:partial')({
        payload: {
          session_id: 71,
          text: 'alpha',
          timestamp: 1,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });
      await flushMicrotasks();
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 71,
          text: 'beta',
          timestamp: 2,
          is_segment_final: false,
          start: 1,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 71, status: 'Idle', stopped_via_hotkey: true },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 72, status: 'Recording', stopped_via_hotkey: false },
      });

      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'copy_to_clipboard_native')).toHaveLength(0);

      firstPaste.resolve(null);
      await firstSegment;
      await flushMicrotasks();
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) =>
        cmd === 'auto_paste_text' || cmd === 'copy_to_clipboard_native'
      )).toEqual([
        ['auto_paste_text', { text: 'alpha', sessionId: 71 }],
        ['copy_to_clipboard_native', { text: 'alpha beta' }],
        ['auto_paste_text', { text: ' beta', sessionId: 71 }],
      ]);
    } finally {
      firstPaste.resolve(null);
      vi.useRealTimers();
    }
  });

  it('failed in-flight вставка оставляет old ledger пустым для полного retry tail', async () => {
    vi.useFakeTimers();
    const firstPaste = deferred<null>();
    try {
      appConfigMock.autoPasteText = true;
      let pasteCount = 0;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd !== 'auto_paste_text') return Promise.resolve(null);
        pasteCount += 1;
        return pasteCount === 1 ? firstPaste.promise : Promise.resolve(null);
      });
      const { handlers } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 73, status: 'Recording', stopped_via_hotkey: false },
      });
      const firstSegment = handlers.get('transcription:partial')({
        payload: {
          session_id: 73,
          text: 'alpha',
          timestamp: 1,
          is_segment_final: true,
          start: 0,
          duration: 1,
        },
      });
      await flushMicrotasks();
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 73,
          text: 'beta',
          timestamp: 2,
          is_segment_final: false,
          start: 1,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 73, status: 'Idle', stopped_via_hotkey: true },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 74, status: 'Recording', stopped_via_hotkey: false },
      });

      firstPaste.reject(new Error('native paste failed'));
      await firstSegment;
      await flushMicrotasks();
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'alpha', sessionId: 73 }],
        ['auto_paste_text', { text: 'alpha beta', sessionId: 73 }],
      ]);
    } finally {
      firstPaste.resolve(null);
      vi.useRealTimers();
    }
  });

  it('duplicate Idle и repeated cleanup финализируют pending tail ровно один раз', async () => {
    vi.useFakeTimers();
    try {
      appConfigMock.autoPasteText = true;
      invokeMock.mockResolvedValue(null);
      const { handlers, store } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 75, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 75,
          text: 'single delivery',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 75, status: 'Idle', stopped_via_hotkey: true },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 75, status: 'Idle', stopped_via_hotkey: true },
      });

      store.cleanup();
      store.cleanup();
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'single delivery', sessionId: 75 }],
      ]);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('сработавший stop timer синхронно claims session до завершения delivery IPC', async () => {
    vi.useFakeTimers();
    const pendingPaste = deferred<null>();
    try {
      appConfigMock.autoPasteText = true;
      invokeMock.mockImplementation((cmd: string) =>
        cmd === 'auto_paste_text' ? pendingPaste.promise : Promise.resolve(null)
      );
      const { handlers, store } = await initializeStoreWithHandlers();

      await handlers.get('recording:status')({
        payload: { session_id: 76, status: 'Recording', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 76,
          text: 'claimed once',
          timestamp: 1,
          is_segment_final: false,
          start: 0,
          duration: 1,
        },
      });
      await handlers.get('recording:status')({
        payload: { session_id: 76, status: 'Idle', stopped_via_hotkey: true },
      });
      await vi.advanceTimersByTimeAsync(1_500);
      await flushMicrotasks();

      expect(store.sessionId).toBeNull();
      await handlers.get('recording:status')({
        payload: { session_id: 76, status: 'Idle', stopped_via_hotkey: true },
      });
      store.cleanup();
      pendingPaste.resolve(null);
      await flushMicrotasks();

      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'auto_paste_text')).toEqual([
        ['auto_paste_text', { text: 'claimed once', sessionId: 76 }],
      ]);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      pendingPaste.resolve(null);
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
      expect(store.closedSessionIdFloor).toBeLessThan(137);
      await handlers.get('transcription:partial')({
        payload: {
          session_id: 137,
          text: 'late closed text',
          timestamp: 2,
          is_segment_final: false,
          start: 1,
          duration: 1,
        },
      });
      expect(store.partialText).toBe('');
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
        ['auto_paste_text', { text: 'текст перед vad stop', sessionId: 39 }],
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
        ['auto_paste_text', { text: 'чистовой vad текст', sessionId: 39 }],
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
        ['auto_paste_text', { text: 'готовый сегмент', sessionId: 40 }],
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

  it('window_shown reconcile closes a stale active session when backend is authoritatively Idle', async () => {
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

    expect(store.sessionId).toBeNull();
    expect(store.closedSessionIdFloor).toBeGreaterThanOrEqual(51);
    expect(store.status).toBe('Idle');
  });

  it('ограничивает потоковые тексты перевода в длинной сессии', async () => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (eventName: string, handler: any) => {
      handlers.set(eventName, handler);
      return () => {};
    });
    invokeMock.mockResolvedValue(null);

    const store = useTranscriptionStore();
    await store.initialize();
    const longText = `old-prefix ${'x'.repeat(40_000)} latest-tail`;

    await handlers.get('incoming_translation:status')({
      payload: { session_id: 701, status: 'Recording' },
    });
    await handlers.get('incoming_translation:source-final')({
      payload: { session_id: 701, text: longText },
    });
    await handlers.get('incoming_translation:delta')({
      payload: { session_id: 701, text: longText },
    });

    expect(store.incomingSourceText.length).toBeLessThanOrEqual(32_000);
    expect(store.incomingSourceText).not.toContain('old-prefix');
    expect(store.incomingSourceText).toContain('latest-tail');
    expect(store.incomingTranslationText.length).toBeLessThanOrEqual(32_000);
    expect(store.incomingTranslationText).toContain('latest-tail');

    await handlers.get('recording:status')({
      payload: {
        session_id: 702,
        status: 'Recording',
        stopped_via_hotkey: false,
        mode: 'live_translation',
      },
    });
    await handlers.get('translation:delta')({
      payload: { session_id: 702, text: longText },
    });

    expect(store.translationText.length).toBeLessThanOrEqual(32_000);
    expect(store.translationText).not.toContain('old-prefix');
    expect(store.translationText).toContain('latest-tail');
  });
  it.each(['Starting', 'Recording'])('rejects older %s while a newer session is active', async (status) => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
    invokeMock.mockResolvedValue(null);
    const store = useTranscriptionStore();
    await store.initialize();
    await handlers.get('recording:status')({ payload: { session_id: 90, status: 'Recording' } });
    store.finalText = 'Current speech';
    await handlers.get('recording:status')({ payload: { session_id: 89, status } });
    expect(store.sessionId).toBe(90);
    expect(store.status).toBe('Recording');
    expect(store.finalText).toBe('Current speech');
    store.cleanup();
  });

  it.each(['Idle', 'Starting', 'Processing', 'Error'])('discards late reconcile %s after a newer recording event', async (snapshot) => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
    invokeMock.mockResolvedValue(null);
    const store = useTranscriptionStore();
    await store.initialize();
    const pending = deferred<string>();
    invokeMock.mockImplementation((cmd) => cmd === 'get_recording_status' ? pending.promise : Promise.resolve(null));
    const reconciliation = store.reconcileBackendStatus('stop_recording_success');
    await handlers.get('recording:status')({ payload: { session_id: 90, status: 'Recording' } });
    pending.resolve(snapshot);
    expect(await reconciliation).toBeNull();
    expect(store.status).toBe('Recording');
    expect(store.sessionId).toBe(90);
    expect(store.closedSessionIdFloor).toBeLessThan(90);
    store.cleanup();
  });

  it('discards reconcile after cleanup even when the visible state did not change', async () => {
    invokeMock.mockResolvedValue(null);
    const store = useTranscriptionStore();
    await store.initialize();
    const pending = deferred<string>();
    invokeMock.mockImplementation((cmd) => cmd === 'get_recording_status' ? pending.promise : Promise.resolve(null));
    const reconciliation = store.reconcileBackendStatus('window_shown');
    store.cleanup();
    pending.resolve('Recording');
    expect(await reconciliation).toBeNull();
    expect(store.status).toBe('Idle');
  });

  it('discards an older reconcile request after a newer snapshot completes', async () => {
    const store = useTranscriptionStore();
    const oldQuery = deferred<string>();
    const newQuery = deferred<string>();
    invokeMock.mockReturnValueOnce(oldQuery.promise).mockReturnValueOnce(newQuery.promise);
    const oldReconcile = store.reconcileBackendStatus('window_shown');
    const newReconcile = store.reconcileBackendStatus('window_shown');
    newQuery.resolve('Idle');
    expect(await newReconcile).toBe('Idle');
    oldQuery.resolve('Recording');
    expect(await oldReconcile).toBeNull();
    expect(store.status).toBe('Idle');
  });

  it.each(['resolve', 'reject'])('ignores a stale stop %s after another session starts', async (outcome) => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
    invokeMock.mockResolvedValue(null);
    const store = useTranscriptionStore();
    await store.initialize();
    await handlers.get('recording:status')({ payload: { session_id: 90, status: 'Recording' } });
    const pending = deferred<string>();
    invokeMock.mockImplementation((cmd) => {
      if (cmd === 'stop_recording') return pending.promise.then((value) => {
        if (outcome === 'reject') throw new Error('Old stop failed');
        return value;
      });
      if (cmd === 'get_recording_status') return Promise.resolve('Idle');
      return Promise.resolve(null);
    });
    const stop = store.stopRecording();
    expect(invokeMock).toHaveBeenCalledWith('stop_recording', { expectedSessionId: 90 });
    await handlers.get('recording:status')({ payload: { session_id: 91, status: 'Recording' } });
    store.finalText = 'New speech';
    store.error = 'New session diagnostic';
    pending.resolve('stopped');
    await stop;
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'get_recording_status')).toBe(false);
    expect(store.status).toBe('Recording');
    expect(store.sessionId).toBe(91);
    expect(store.finalText).toBe('New speech');
    expect(store.error).toBe('New session diagnostic');
    store.cleanup();
  });

  it.each(['hotkey', 'stop', 'cleanup'])('cancels a UI retry backoff on %s without resurrecting recording', async (cancellation) => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
      invokeMock.mockResolvedValue(null);
      const store = useTranscriptionStore();
      await store.initialize();
      invokeMock.mockImplementation((cmd) => {
        if (cmd === 'start_recording') return Promise.reject('Connection error: network unavailable');
        if (cmd === 'get_recording_status') return Promise.resolve('Idle');
        return Promise.resolve(null);
      });
      const firstStart = store.startRecording();
      await flushMicrotasks();
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
      if (cancellation === 'hotkey') {
        store.prepareForRustHotkeyStart(false);
        await handlers.get('recording:status')({ payload: { session_id: 202, status: 'Recording' } });
        store.finalText = 'Fresh speech';
      } else if (cancellation === 'stop') {
        await store.stopRecording();
      } else {
        store.cleanup();
      }
      const stops = invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording').length;
      await vi.advanceTimersByTimeAsync(40_000);
      await firstStart;
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(stops);
      expect(store.isConnecting).toBe(false);
      if (cancellation === 'hotkey') {
        expect(store.status).toBe('Recording');
        expect(store.sessionId).toBe(202);
        expect(store.finalText).toBe('Fresh speech');
      } else if (cancellation === 'stop') {
        expect(store.status).toBe('Idle');
      }
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it.each(['resolve', 'reject'])('ignores a late old start %s after a Rust-owned session starts', async (outcome) => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
      invokeMock.mockResolvedValue(null);
      const store = useTranscriptionStore();
      await store.initialize();
      const pending = deferred<void>();
      invokeMock.mockImplementation((cmd) => cmd === 'start_recording' ? pending.promise.then(() => {
        if (outcome === 'reject') throw new Error('Invalid STT configuration');
        return 'started';
      }) : Promise.resolve(null));
      const oldStart = store.startRecording();
      await flushMicrotasks();
      store.prepareForRustHotkeyStart(false);
      await handlers.get('recording:status')({ payload: { session_id: 202, status: 'Recording' } });
      store.finalText = 'Fresh speech';
      store.error = 'New session diagnostic';
      pending.resolve();
      await vi.advanceTimersByTimeAsync(40_000);
      await oldStart;
      expect(store.status).toBe('Recording');
      expect(store.sessionId).toBe(202);
      expect(store.error).toBe('New session diagnostic');
      expect(store.finalText).toBe('Fresh speech');
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(0);
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it('keeps its own native start echo and retries with exactly one session-scoped cleanup', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
      invokeMock.mockResolvedValue(null);
      const store = useTranscriptionStore();
      await store.initialize();
      let starts = 0;
      invokeMock.mockImplementation(async (cmd, args) => {
        if (cmd === 'start_recording') {
          starts++;
          store.prepareForRustHotkeyStart(false, args.clientStartId);
          await handlers.get('recording:status')({ payload: { session_id: 300 + starts, status: starts === 1 ? 'Starting' : 'Recording' } });
          if (starts === 1) throw new Error('Connection error: network unavailable');
          return 'started';
        }
        if (cmd === 'stop_recording') {
          expect(args).toEqual({ expectedSessionId: 301 });
          return 'stopped';
        }
        return null;
      });
      const start = store.startRecording();
      await vi.advanceTimersByTimeAsync(2_000);
      await start;
      expect(starts).toBe(2);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(1);
      expect(store.status).toBe('Recording');
      expect(store.sessionId).toBe(302);
      expect(store.isConnecting).toBe(false);
      const ownId = invokeMock.mock.calls.find(([cmd]) => cmd === 'start_recording')![1].clientStartId;
      store.prepareForRustHotkeyStart(false, ownId);
      expect(store.sessionId).toBe(302);
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it.each(['startFailed', 'runtimeFailed'] as const)(
    'keeps a UI retry alive across the %s cleanup projection', async (fault) => {
    vi.useFakeTimers();
    try {
      invokeMock.mockResolvedValue(null);
      const { handlers, store } = await initializeStoreWithHandlers();
      let starts = 0;
      invokeMock.mockImplementation(async (cmd) => {
        if (cmd !== 'start_recording') return null;
        starts++;
        if (starts === 2) {
          await handlers.get('recording:intent-projection')({ payload: {
            runId: 2, intentRevision: 2, status: 'Starting', desiredOn: true,
            pendingStart: false, processingJobs: 0, shutdownRequested: false,
          } });
          await handlers.get('recording:status')({
            payload: { session_id: 2, status: 'Starting', stopped_via_hotkey: false },
          });
          await handlers.get('recording:status')({
            payload: { session_id: 2, status: 'Recording', stopped_via_hotkey: false },
          });
        }
        return 'Recording start requested';
      });

      const start = store.startRecording();
      await flushMicrotasks();
      await handlers.get('recording:intent-projection')({ payload: {
        runId: 1, intentRevision: 1, status: 'Starting', desiredOn: true,
        pendingStart: false, processingJobs: 0, shutdownRequested: false,
      } });
      await handlers.get('recording:status')({
        payload: { session_id: 1, status: 'Starting', stopped_via_hotkey: false },
      });
      await handlers.get('recording:intent-projection')({ payload: {
        runId: null, faultRunId: 1, intentRevision: 1, status: 'Error', desiredOn: false,
        pendingStart: false, processingJobs: 0, shutdownRequested: false, fault,
      } });
      expect(store.isConnecting).toBe(true);
      expect(store.error).toBeNull();
      await handlers.get('recording:status')({
        payload: { session_id: 1, status: 'Error', stopped_via_hotkey: false },
      });
      expect(store.status).toBe('Starting');
      await handlers.get('transcription:error')({ payload: {
        session_id: 1, error: 'Connection error: network unavailable', error_type: 'connection',
      } });
      await handlers.get('recording:intent-projection')({ payload: {
        runId: null, intentRevision: 1, status: 'Idle', desiredOn: false,
        pendingStart: false, processingJobs: 0, shutdownRequested: false,
      } });
      invokeMock.mockImplementation(async (cmd) => {
        if (cmd === 'get_recording_status') return 'Starting';
        if (cmd === 'get_recording_capture_readiness') return {
          revision: 1, runId: null, state: 'unavailable', reason: 'idle', generation: 1,
        };
        if (cmd !== 'start_recording') return null;
        starts++;
        if (starts === 2) {
          await handlers.get('recording:intent-projection')({ payload: {
            runId: 2, intentRevision: 2, status: 'Starting', desiredOn: true,
            pendingStart: false, processingJobs: 0, shutdownRequested: false,
          } });
          await handlers.get('recording:status')({
            payload: { session_id: 2, status: 'Starting', stopped_via_hotkey: false },
          });
          await handlers.get('recording:status')({
            payload: { session_id: 2, status: 'Recording', stopped_via_hotkey: false },
          });
        }
        return 'Recording start requested';
      });
      await store.reconcileBackendStatus('failed_start_cleanup');

      expect(store.isConnecting).toBe(true);
      expect(store.status).toBe('Starting');
      expect(starts).toBe(1);

      await vi.advanceTimersByTimeAsync(1_000);
      await start;
      expect(starts).toBe(2);
      expect(store.status).toBe('Recording');
      expect(store.sessionId).toBe(2);
      expect(store.isConnecting).toBe(false);
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it('does not suppress a runtime fault after Recording while start IPC is unresolved', async () => {
    const pending = deferred<string>();
    invokeMock.mockImplementation(async (command) => {
      if (command === 'start_recording') return pending.promise;
      return null;
    });
    const { handlers, store } = await initializeStoreWithHandlers();
    const start = store.startRecording();
    await flushMicrotasks();
    await handlers.get('recording:intent-projection')({ payload: {
      runId: 71, intentRevision: 1, status: 'Starting', desiredOn: true,
      pendingStart: false, processingJobs: 0, shutdownRequested: false,
    } });
    await handlers.get('recording:status')({ payload: {
      session_id: 71, status: 'Recording', stopped_via_hotkey: false,
    } });
    await handlers.get('recording:intent-projection')({ payload: {
      runId: null, faultRunId: 71, intentRevision: 2, status: 'Error', desiredOn: false,
      pendingStart: false, processingJobs: 0, shutdownRequested: false,
      fault: 'runtimeFailed',
    } });

    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
    expect(store.error).toBeTruthy();
    pending.resolve('Recording start requested');
    await start;
    expect(store.status).toBe('Error');
    expect(store.sessionId).toBeNull();
  });

  it.each(['status', 'partial-adoption', 'reconcile'] as const)(
    'does not retry a run that reached Recording through %s when a provider error arrives before runtimeFailed',
    async (recordingEvidence) => {
    vi.useFakeTimers();
    try {
      const pending = deferred<string>();
      invokeMock.mockImplementation(async (command) => {
        if (command === 'start_recording') return pending.promise;
        if (command === 'get_recording_status') return 'Recording';
        if (command === 'get_recording_capture_readiness') return {
          revision: 1, runId: 71, captureEpisodeId: 71, captureGeneration: 1,
          state: 'streaming', reason: 'recording', captureReady: true, generation: 1,
        };
        return null;
      });
      const { handlers, store } = await initializeStoreWithHandlers();
      const start = store.startRecording();
      await flushMicrotasks();
      await handlers.get('recording:intent-projection')({ payload: {
        runId: 71, intentRevision: 1, status: 'Starting', desiredOn: true,
        pendingStart: false, processingJobs: 0, shutdownRequested: false,
      } });
      if (recordingEvidence === 'status') {
        await handlers.get('recording:status')({ payload: {
          session_id: 71, status: 'Recording', stopped_via_hotkey: false,
        } });
      } else if (recordingEvidence === 'partial-adoption') {
        await handlers.get('transcription:partial')({ payload: {
          session_id: 71, text: 'provider is active', is_segment_final: false,
        } });
      } else {
        expect(await store.reconcileBackendStatus('recording_recovery')).toBe('Recording');
      }
      expect(store.status).toBe('Recording');
      await handlers.get('transcription:error')({ payload: {
        session_id: 71, error: 'Provider quota exceeded', error_type: 'provider_quota_exceeded',
        error_details: {
          category: 'provider_quota_exceeded', serverCode: 'PROVIDER_QUOTA_EXCEEDED',
        },
      } });
      await handlers.get('recording:intent-projection')({ payload: {
        runId: null, faultRunId: 71, intentRevision: 2, status: 'Error', desiredOn: false,
        pendingStart: false, processingJobs: 0, shutdownRequested: false,
        fault: 'runtimeFailed',
      } });

      pending.resolve('Recording start requested');
      await vi.advanceTimersByTimeAsync(40_000);
      await start;
      expect(invokeMock.mock.calls.filter(([command]) => command === 'start_recording')).toHaveLength(1);
      expect(store.status).toBe('Error');
      expect(store.sessionId).toBeNull();
      expect(store.errorType).toBe('provider_quota_exceeded');
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it('cancels a UI retry when a newer native Off supersedes failed-intent cleanup', async () => {
    vi.useFakeTimers();
    try {
      invokeMock.mockResolvedValue(null);
      const { handlers, store } = await initializeStoreWithHandlers();
      invokeMock.mockImplementation((cmd) =>
        cmd === 'start_recording' ? Promise.resolve('Recording start requested') : Promise.resolve(null)
      );
      const start = store.startRecording();
      await flushMicrotasks();
      await handlers.get('recording:status')({
        payload: { session_id: 1, status: 'Starting', stopped_via_hotkey: false },
      });
      await handlers.get('transcription:error')({ payload: {
        session_id: 1, error: 'Connection error: network unavailable', error_type: 'connection',
      } });
      await handlers.get('recording:intent-projection')({ payload: {
        runId: null, faultRunId: 1, intentRevision: 1, status: 'Error', desiredOn: false,
        pendingStart: false, processingJobs: 0, shutdownRequested: false, fault: 'startFailed',
      } });
      await handlers.get('recording:intent-projection')({ payload: {
        runId: null, intentRevision: 2, status: 'Idle', desiredOn: false,
        pendingStart: false, processingJobs: 0, shutdownRequested: false,
      } });

      await vi.advanceTimersByTimeAsync(40_000);
      await start;
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'start_recording')).toHaveLength(1);
      expect(store.isConnecting).toBe(false);
      expect(store.status).toBe('Idle');
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it.each([0, 402])('handles failed-start cleanup with native active owner %s', async (replacementOwner) => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
      invokeMock.mockResolvedValue(null);
      const store = useTranscriptionStore();
      await store.initialize();
      let starts = 0;
      let activeOwner = 0;
      invokeMock.mockImplementation(async (cmd, args) => {
        if (cmd === 'start_recording') {
          starts++;
          activeOwner = 400 + starts;
          store.prepareForRustHotkeyStart(false, args.clientStartId);
          await handlers.get('recording:status')({ payload: { session_id: activeOwner, status: 'Starting' } });
          if (starts === 1) {
            // Native restores the displaced owner before publishing the start failure.
            activeOwner = replacementOwner;
            await handlers.get('transcription:error')({ payload: {
              session_id: 401, error: 'Connection error: network unavailable', error_type: 'connection',
            } });
            await handlers.get('recording:status')({ payload: { session_id: 401, status: 'Error' } });
            throw new Error('Connection error: network unavailable');
          }
          await handlers.get('recording:status')({ payload: { session_id: activeOwner, status: 'Recording' } });
          return 'started';
        }
        if (cmd === 'stop_recording') {
          expect(args).toEqual({ expectedSessionId: 401 });
          return activeOwner === 0 ? 'Recording already stopped' : 'Stale recording stop ignored';
        }
        return null;
      });
      const start = store.startRecording();
      await vi.advanceTimersByTimeAsync(2_000);
      await start;
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'stop_recording')).toHaveLength(1);
      expect(starts).toBe(replacementOwner === 0 ? 2 : 1);
      expect(store.isConnecting).toBe(false);
      if (replacementOwner === 0) {
        expect(store.status).toBe('Recording');
        expect(store.sessionId).toBe(402);
      } else {
        expect(activeOwner).toBe(replacementOwner);
      }
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it('cancels an outcome wait immediately when cleaned up', async () => {
    vi.useFakeTimers();
    try {
      invokeMock.mockResolvedValue('started');
      const store = useTranscriptionStore();
      const start = store.startRecording();
      await flushMicrotasks();
      await flushMicrotasks();
      expect(vi.getTimerCount()).toBeGreaterThan(0);
      store.cleanup();
      await start;
      expect(vi.getTimerCount()).toBe(0);
      expect(store.isConnecting).toBe(false);
    } finally { vi.useRealTimers(); }
  });

  it('does not logout or retry when auth refresh completes after a new hotkey start', async () => {
    const handlers = new Map<string, any>();
    listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
    invokeMock.mockResolvedValue(null);
    const store = useTranscriptionStore();
    await store.initialize();
    const refresh = deferred<null>();
    authContainerMock.refreshTokensUseCase.execute.mockReturnValue(refresh.promise);
    invokeMock.mockImplementation((cmd) => cmd === 'start_recording' ? Promise.reject('Authentication: HTTP 401') : Promise.resolve(null));
    const oldStart = store.startRecording();
    for (let i = 0; i < 20 && !authContainerMock.refreshTokensUseCase.execute.mock.calls.length; i++) await flushMicrotasks();
    expect(authContainerMock.refreshTokensUseCase.execute).toHaveBeenCalledTimes(1);
    store.prepareForRustHotkeyStart(false);
    await handlers.get('recording:status')({ payload: { session_id: 302, status: 'Recording' } });
    refresh.resolve(null);
    await oldStart;
    expect(store.status).toBe('Recording');
    expect(store.sessionId).toBe(302);
    expect(authStoreMock.reset).not.toHaveBeenCalled();
    expect(tokenRepoMock.clear).not.toHaveBeenCalled();
    store.cleanup();
  });

  it('preserves an actual pending warm start when shown sees backend Idle', async () => {
    invokeMock.mockResolvedValue('Idle');
    const store = useTranscriptionStore();
    store.prepareForRustHotkeyStart(true);
    await store.reconcileBackendStatus('window_shown');
    expect(store.status).toBe('Starting');
    expect(store.sessionId).toBeNull();
  });

  it.each([false, true])('does not replace the first visible transcript with a shorter animation prefix (segmentFinal=%s)', async (isSegmentFinal) => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
      invokeMock.mockResolvedValue(null);
      const store = useTranscriptionStore();
      await store.initialize();
      await handlers.get('recording:status')({ payload: { session_id: 2, status: 'Recording' } });
      await handlers.get('transcription:partial')({ payload: {
        session_id: 2, text: 'Native fixture session 2', timestamp: 1,
        start: 0, duration: 0.5, is_segment_final: isSegmentFinal,
      } });
      expect(store.displayText).toBe('Native fixture session 2');
      await vi.advanceTimersByTimeAsync(15);
      expect(store.displayText).toBe('Native fixture session 2');
      await vi.advanceTimersByTimeAsync(200);
      expect(store.displayText).toBe('Native fixture session 2');
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

  it('still animates later partial suffixes after the initial segment is seeded', async () => {
    vi.useFakeTimers();
    try {
      const handlers = new Map<string, any>();
      listenMock.mockImplementation(async (name, handler) => { handlers.set(name, handler); return () => {}; });
      invokeMock.mockResolvedValue(null);
      const store = useTranscriptionStore();
      await store.initialize();
      await handlers.get('recording:status')({ payload: { session_id: 2, status: 'Recording' } });
      const partial = (text: string) => handlers.get('transcription:partial')({ payload: {
        session_id: 2, text, timestamp: 1, start: 0, duration: 0.5, is_segment_final: false,
      } });
      await partial('Native');
      await partial('Native fixture session 2');
      expect(store.displayText).toBe('Native');
      await vi.advanceTimersByTimeAsync(200);
      expect(store.displayText).toBe('Native fixture session 2');
      store.cleanup();
    } finally { vi.useRealTimers(); }
  });

});
