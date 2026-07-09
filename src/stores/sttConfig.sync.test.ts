import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useSttConfigStore } from './sttConfig';
import { CMD_GET_STT_CONFIG_SNAPSHOT, STATE_SYNC_INVALIDATION_EVENT } from '@/windowing/stateSync';
import { BackendStreamingProviderType, SttProviderType } from '@/types';

const invokeMock = vi.fn();
const listenMock = vi.fn();

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

function makeSnapshotData(overrides: Partial<any> = {}) {
  return {
    provider: 'backend',
    backend_streaming_provider: 'deepgram',
    language: 'en',
    auto_detect_language: false,
    enable_punctuation: true,
    filter_profanity: false,
    deepgram_api_key: null,
    assemblyai_api_key: null,
    model: null,
    keep_connection_alive: false,
    streaming_keyterms: null,
    ...overrides,
  };
}

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

describe('useSttConfigStore sync', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    (window as any).__TAURI__ = {};
    invokeMock.mockReset();
    listenMock.mockReset();
  });

  it('startSync: подписывается и загружает snapshot', async () => {
    const unlistenFn = vi.fn();
    listenMock.mockResolvedValue(unlistenFn);

    invokeMock.mockResolvedValue({
      revision: '3',
      data: {
        provider: 'backend',
        backend_streaming_provider: 'deepgram',
        language: 'en',
        auto_detect_language: true,
        enable_punctuation: false,
        filter_profanity: true,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: 'large',
        keep_connection_alive: true,
        streaming_keyterms: null,
      },
    });

    const store = useSttConfigStore();
    await store.startSync();

    expect(listenMock).toHaveBeenCalledWith(STATE_SYNC_INVALIDATION_EVENT, expect.any(Function));
    expect(invokeMock).toHaveBeenCalledWith(CMD_GET_STT_CONFIG_SNAPSHOT, undefined);
    expect(store.revision).toBe('3');
    expect(store.provider).toBe('backend');
    expect(store.backendStreamingProvider).toBe('deepgram');
    expect(store.language).toBe('en');
    expect(store.autoDetectLanguage).toBe(true);
    expect(store.enablePunctuation).toBe(false);
    expect(store.filterProfanity).toBe(true);
    expect(store.model).toBe('large');
    expect(store.keepConnectionAlive).toBe(true);
    expect(store.isLoaded).toBe(true);
  });

  it('applySnapshot обновляет значения', () => {
    const store = useSttConfigStore();

    store.applySnapshot(
      {
        provider: SttProviderType.Deepgram,
        backend_streaming_provider: BackendStreamingProviderType.ElevenLabs,
        language: 'de',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: 'key-123',
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        streaming_keyterms: null,
      },
      '15',
    );

    expect(store.revision).toBe('15');
    expect(store.provider).toBe('deepgram');
    expect(store.backendStreamingProvider).toBe('elevenlabs');
    expect(store.language).toBe('de');
    expect(store.deepgramApiKey).toBe('key-123');
    expect(store.isLoaded).toBe(true);
  });

  it('applySnapshot: если streaming_keyterms не пришёл — не затирает текущее значение', () => {
    const store = useSttConfigStore();

    store.applySnapshot(
      {
        provider: SttProviderType.Backend,
        backend_streaming_provider: BackendStreamingProviderType.Deepgram,
        language: 'ru',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        streaming_keyterms: 'Kubernetes, VoicetextAI',
      },
      '1',
    );

    expect(store.streamingKeyterms).toBe('Kubernetes, VoicetextAI');

    // Мокаем "partial snapshot" без streaming_keyterms (как в scenario тестах)
    store.applySnapshot(
      {
        provider: SttProviderType.Backend,
        backend_streaming_provider: BackendStreamingProviderType.Deepgram,
        language: 'en',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        // streaming_keyterms отсутствует намеренно
      } as any,
      '2',
    );

    expect(store.language).toBe('en');
    expect(store.streamingKeyterms).toBe('Kubernetes, VoicetextAI');
  });

  it('applySnapshot: читает legacy deepgram_keyterms если нового поля нет', () => {
    const store = useSttConfigStore();

    store.applySnapshot(
      {
        provider: SttProviderType.Backend,
        backend_streaming_provider: BackendStreamingProviderType.Deepgram,
        language: 'ru',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        deepgram_keyterms: 'Legacy, Terms',
      } as any,
      'legacy',
    );

    expect(store.streamingKeyterms).toBe('Legacy, Terms');
  });

  it('startSync: при ошибке start() — handle обнуляется и retry работает', async () => {
    listenMock.mockResolvedValue(vi.fn());
    invokeMock.mockRejectedValueOnce(new Error('network error'));

    const store = useSttConfigStore();
    const failed = await store.startSync();
    expect(failed).toBe(false);
    expect(store.isSyncing).toBe(false);

    // retry
    invokeMock.mockResolvedValue({
      revision: '1',
      data: {
        provider: 'backend',
        backend_streaming_provider: 'deepgram',
        language: 'en',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        streaming_keyterms: null,
      },
    });

    const succeeded = await store.startSync();
    expect(succeeded).toBe(true);
    expect(store.isSyncing).toBe(true);
    expect(store.language).toBe('en');
  });

  it('refresh делегирует в handle.refresh()', async () => {
    const unlistenFn = vi.fn();
    listenMock.mockResolvedValue(unlistenFn);

    invokeMock.mockResolvedValue({
      revision: '1',
      data: {
        provider: 'backend',
        backend_streaming_provider: 'deepgram',
        language: 'ru',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        streaming_keyterms: null,
      },
    });

    const store = useSttConfigStore();
    await store.startSync();
    expect(store.language).toBe('ru');

    invokeMock.mockResolvedValue({
      revision: '2',
      data: {
        provider: 'backend',
        backend_streaming_provider: 'deepgram',
        language: 'ja',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
        streaming_keyterms: null,
      },
    });

    await store.refresh();
    expect(store.revision).toBe('2');
    expect(store.language).toBe('ja');
  });

  it('concurrent startSync переиспользует in-flight start и не создает второй handle', async () => {
    const pendingListen = deferred<() => void>();
    const unlistenFn = vi.fn();
    listenMock.mockReturnValue(pendingListen.promise);
    invokeMock.mockResolvedValue({
      revision: '1',
      data: makeSnapshotData(),
    });

    const store = useSttConfigStore();
    const first = store.startSync();
    const second = store.startSync();

    for (let i = 0; i < 20 && listenMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }
    expect(listenMock).toHaveBeenCalledTimes(1);

    pendingListen.resolve(unlistenFn);
    await expect(Promise.all([first, second])).resolves.toEqual([true, true]);
    expect(store.isSyncing).toBe(true);
  });

  it('stopSync останавливает late handle, если startSync завершился после stop', async () => {
    const pendingListen = deferred<() => void>();
    const unlistenFn = vi.fn();
    listenMock.mockReturnValueOnce(pendingListen.promise);
    invokeMock.mockResolvedValue({
      revision: '1',
      data: makeSnapshotData(),
    });

    const store = useSttConfigStore();
    const start = store.startSync();
    for (let i = 0; i < 20 && listenMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }

    store.stopSync();
    pendingListen.resolve(unlistenFn);

    await expect(start).resolves.toBe(false);
    expect(unlistenFn).toHaveBeenCalledTimes(1);
    expect(store.isSyncing).toBe(false);
  });
});
