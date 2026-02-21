import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useAppConfigStore } from './appConfig';
import { CMD_GET_APP_CONFIG_SNAPSHOT, STATE_SYNC_INVALIDATION_EVENT } from '@/windowing/stateSync';

const invokeMock = vi.fn();
const listenMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

describe('useAppConfigStore sync', () => {
  function makeSnapshotData(overrides: Partial<any> = {}) {
    return {
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      microphone_sensitivity: 100,
      selected_audio_device: null,
      ...overrides,
    };
  }

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
      revision: '7',
      data: makeSnapshotData({
        recording_hotkey: 'CmdOrCtrl+Shift+P',
        auto_paste_text: true,
        microphone_sensitivity: 120,
        selected_audio_device: 'Mic A',
      }),
    });

    const store = useAppConfigStore();
    await store.startSync();

    // Библиотека вызывает listen для подписки на invalidation
    expect(listenMock).toHaveBeenCalledWith(STATE_SYNC_INVALIDATION_EVENT, expect.any(Function));
    // И invoke для получения snapshot
    expect(invokeMock).toHaveBeenCalledWith(CMD_GET_APP_CONFIG_SNAPSHOT, undefined);
    expect(store.revision).toBe('7');
    expect(store.recordingHotkey).toBe('CmdOrCtrl+Shift+P');
    expect(store.autoCopyToClipboard).toBe(false);
    expect(store.autoPasteText).toBe(true);
    expect(store.microphoneSensitivity).toBe(120);
    expect(store.selectedAudioDevice).toBe('Mic A');
  });

  it('applySnapshot обновляет значения из SnapshotEnvelope', () => {
    const store = useAppConfigStore();

    store.applySnapshot(
      {
        recording_hotkey: 'Alt+Z',
        auto_copy_to_clipboard: true,
        auto_paste_text: false,
        microphone_sensitivity: 50,
        selected_audio_device: 'Mic B',
      },
      '42',
    );

    expect(store.revision).toBe('42');
    expect(store.recordingHotkey).toBe('Alt+Z');
    expect(store.autoCopyToClipboard).toBe(true);
    expect(store.autoPasteText).toBe(false);
    expect(store.microphoneSensitivity).toBe(50);
    expect(store.selectedAudioDevice).toBe('Mic B');
    expect(store.isLoaded).toBe(true);
  });

  it('refresh делегирует в handle.refresh()', async () => {
    const unlistenFn = vi.fn();
    listenMock.mockResolvedValue(unlistenFn);

    invokeMock.mockResolvedValue({
      revision: '5',
      data: makeSnapshotData({ recording_hotkey: 'R' }),
    });

    const store = useAppConfigStore();
    await store.startSync();

    expect(store.recordingHotkey).toBe('R');

    // Обновляем "бэкенд" и делаем ручной refresh
    invokeMock.mockResolvedValue({
      revision: '6',
      data: makeSnapshotData({ recording_hotkey: 'S' }),
    });

    await store.refresh();
    expect(store.revision).toBe('6');
    expect(store.recordingHotkey).toBe('S');
  });

  it('stopSync вызывает unlisten и сбрасывает handle', async () => {
    const unlistenFn = vi.fn();
    listenMock.mockResolvedValue(unlistenFn);

    invokeMock.mockResolvedValue({
      revision: '1',
      data: makeSnapshotData(),
    });

    const store = useAppConfigStore();
    await store.startSync();
    expect(store.isSyncing).toBe(true);

    store.stopSync();
    expect(store.isSyncing).toBe(false);
    expect(unlistenFn).toHaveBeenCalled();
  });

  it('startSync: при ошибке start() — handle обнуляется и retry работает', async () => {
    listenMock.mockResolvedValue(vi.fn());
    // Первый вызов start() внутри state-sync вызывает invoke → ошибка
    invokeMock.mockRejectedValueOnce(new Error('network error'));

    const store = useAppConfigStore();
    const failed = await store.startSync();
    expect(failed).toBe(false);
    expect(store.isSyncing).toBe(false);

    // retry — теперь invoke отдаёт валидный snapshot
    invokeMock.mockResolvedValue({
      revision: '1',
      data: makeSnapshotData({ recording_hotkey: 'Alt+R' }),
    });

    const succeeded = await store.startSync();
    expect(succeeded).toBe(true);
    expect(store.isSyncing).toBe(true);
    expect(store.recordingHotkey).toBe('Alt+R');
  });

  it('startSync идемпотентен — повторный вызов не создаёт второй handle', async () => {
    const unlistenFn = vi.fn();
    listenMock.mockResolvedValue(unlistenFn);

    invokeMock.mockResolvedValue({
      revision: '1',
      data: makeSnapshotData(),
    });

    const store = useAppConfigStore();
    await store.startSync();
    await store.startSync();

    // listen должен быть вызван только один раз
    expect(listenMock).toHaveBeenCalledTimes(1);
  });
});
