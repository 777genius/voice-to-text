import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useSttConfigStore } from './sttConfig';
import { CMD_GET_STT_CONFIG_SNAPSHOT, STATE_SYNC_INVALIDATION_EVENT } from '@/windowing/stateSync';
import { SttProviderType } from '@/types';

const invokeMock = vi.fn();
const listenMock = vi.fn();

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
        language: 'en',
        auto_detect_language: true,
        enable_punctuation: false,
        filter_profanity: true,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: 'large',
        keep_connection_alive: true,
      },
    });

    const store = useSttConfigStore();
    await store.startSync();

    expect(listenMock).toHaveBeenCalledWith(STATE_SYNC_INVALIDATION_EVENT, expect.any(Function));
    expect(invokeMock).toHaveBeenCalledWith(CMD_GET_STT_CONFIG_SNAPSHOT, undefined);
    expect(store.revision).toBe('3');
    expect(store.provider).toBe('backend');
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
        language: 'de',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: 'key-123',
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
      },
      '15',
    );

    expect(store.revision).toBe('15');
    expect(store.provider).toBe('deepgram');
    expect(store.language).toBe('de');
    expect(store.deepgramApiKey).toBe('key-123');
    expect(store.isLoaded).toBe(true);
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
        language: 'en',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
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
        language: 'ru',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
      },
    });

    const store = useSttConfigStore();
    await store.startSync();
    expect(store.language).toBe('ru');

    invokeMock.mockResolvedValue({
      revision: '2',
      data: {
        provider: 'backend',
        language: 'ja',
        auto_detect_language: false,
        enable_punctuation: true,
        filter_profanity: false,
        deepgram_api_key: null,
        assemblyai_api_key: null,
        model: null,
        keep_connection_alive: false,
      },
    });

    await store.refresh();
    expect(store.revision).toBe('2');
    expect(store.language).toBe('ja');
  });
});
