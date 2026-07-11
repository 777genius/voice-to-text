import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useAppConfigStore } from './appConfig';
import { CMD_GET_APP_CONFIG_SNAPSHOT, STATE_SYNC_INVALIDATION_EVENT } from '@/windowing/stateSync';

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
      play_completion_sound: false,
      hide_recording_window_on_hotkey: false,
      show_mini_recording_window: false,
      keep_recording_until_manual_stop: false,
      hold_to_record: false,
      double_space_hotkey_enabled: false,
      microphone_sensitivity: 100,
      selected_audio_device: null,
      openai_api_key: null,
      ...overrides,
    };
  }

  beforeEach(() => {
    setActivePinia(createPinia());
    (window as any).__TAURI__ = {};
    invokeMock.mockReset();
    listenMock.mockReset();
  });

  it('создает новый store с продуктовыми дефолтами', () => {
    const store = useAppConfigStore();

    expect(store.autoPasteText).toBe(true);
    expect(store.showMiniRecordingWindow).toBe(true);
    expect(store.incomingTranslationDelivery).toBe('captions_only');
    expect(store.incomingTranslationVolume).toBe(100);
  });

  it('startSync: подписывается и загружает snapshot', async () => {
    const unlistenFn = vi.fn();
    listenMock.mockResolvedValue(unlistenFn);

    invokeMock.mockResolvedValue({
      revision: '7',
      data: makeSnapshotData({
        recording_hotkey: 'CmdOrCtrl+Shift+P',
        auto_paste_text: true,
        play_completion_sound: true,
        hide_recording_window_on_hotkey: true,
        show_mini_recording_window: true,
        keep_recording_until_manual_stop: true,
        hold_to_record: true,
        double_space_hotkey_enabled: true,
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
    expect(store.playCompletionSound).toBe(true);
    expect(store.hideRecordingWindowOnHotkey).toBe(true);
    expect(store.showMiniRecordingWindow).toBe(true);
    expect(store.keepRecordingUntilManualStop).toBe(true);
    expect(store.holdToRecord).toBe(true);
    expect(store.doubleSpaceHotkeyEnabled).toBe(true);
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
        play_completion_sound: true,
        hide_recording_window_on_hotkey: true,
        show_mini_recording_window: true,
        keep_recording_until_manual_stop: true,
        hold_to_record: true,
        double_space_hotkey_enabled: true,
        microphone_sensitivity: 50,
        selected_audio_device: 'Mic B',
        recording_mode: 'dictation',
        openai_api_key: 'sk-test',
        incoming_translation_delivery: 'text_and_audio',
        incoming_translation_volume: 64,
      },
      '42',
    );

    expect(store.revision).toBe('42');
    expect(store.recordingHotkey).toBe('Alt+Z');
    expect(store.autoCopyToClipboard).toBe(true);
    expect(store.autoPasteText).toBe(false);
    expect(store.playCompletionSound).toBe(true);
    expect(store.hideRecordingWindowOnHotkey).toBe(true);
    expect(store.showMiniRecordingWindow).toBe(true);
    expect(store.keepRecordingUntilManualStop).toBe(true);
    expect(store.holdToRecord).toBe(true);
    expect(store.doubleSpaceHotkeyEnabled).toBe(true);
    expect(store.microphoneSensitivity).toBe(50);
    expect(store.selectedAudioDevice).toBe('Mic B');
    expect(store.openaiApiKey).toBe('sk-test');
    expect(store.incomingTranslationDelivery).toBe('text_and_audio');
    expect(store.incomingTranslationVolume).toBe(64);
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

  it('concurrent startSync переиспользует in-flight start и не создает второй handle', async () => {
    const pendingListen = deferred<() => void>();
    const unlistenFn = vi.fn();
    listenMock.mockReturnValue(pendingListen.promise);
    invokeMock.mockResolvedValue({
      revision: '1',
      data: makeSnapshotData(),
    });

    const store = useAppConfigStore();
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

    const store = useAppConfigStore();
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
