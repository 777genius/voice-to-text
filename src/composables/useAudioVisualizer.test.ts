import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, defineComponent, ref } from 'vue';
import { TauriAudioSpectrumSource, useAudioVisualizer } from './useAudioVisualizer';
import { MicTestAudioSource } from '../features/settings/presentation/composables/useMicTestAudioSource';
import type { UnlistenFn } from '@tauri-apps/api/event';

const listenMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('../utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe('audio visualizer sources', () => {
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    listenMock.mockReset();
    consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
  });

  afterEach(() => {
    vi.useRealTimers();
    consoleErrorSpy.mockRestore();
  });

  it('uses exact physical capture generation independently from logical transcript identity', async () => {
    listenMock.mockResolvedValue(() => {});
    const source = new TauriAudioSpectrumSource(() => ({ runId: 8, kind: 'capture', captureGeneration: 42 }));
    const bars = vi.fn();
    await source.start(bars);
    const handler = listenMock.mock.calls[0][1];
    handler({payload: {run_id: 1, capture_generation: 42, bars: [0.1]}});
    handler({payload: {run_id: 8, capture_generation: 41, bars: [0.2]}});
    handler({payload: {run_id: 8, capture_generation: 43, bars: [0.3]}});
    handler({payload: {run_id: 8, capture_generation: 42, bars: [0.4]}});
    expect(bars).toHaveBeenCalledTimes(1);
    expect(bars).toHaveBeenCalledWith([0.4]);
    source.stop();
  });

  it('an explicit episode-generation owner supersedes the legacy meter high-water mark', async () => {
    listenMock.mockResolvedValue(() => {});
    let now = 100;
    const clock = vi.spyOn(performance, 'now').mockImplementation(() => now);
    let owner: {runId: number; kind: 'capture'; captureGeneration?: number} = {runId: 8, kind: 'capture'};
    const source = new TauriAudioSpectrumSource(() => owner);
    try {
      const bars = vi.fn();
      await source.start(bars);
      const handler = listenMock.mock.calls[0][1];
      handler({payload: {run_id: 8, capture_generation: 42, bars: [0.4]}});
      owner = {runId: 9, kind: 'capture', captureGeneration: 1};
      now = 200;
      handler({payload: {run_id: 9, capture_generation: 42, bars: [0.9]}});
      handler({payload: {run_id: 9, capture_generation: 1, bars: [0.1]}});
      expect(bars.mock.calls).toEqual([[[0.4]], [[0.1]]]);
    } finally {
      source.stop();
      clock.mockRestore();
    }
  });

  it('TauriAudioSpectrumSource не создает дубль listener при concurrent start', async () => {
    const pendingListen = deferred<UnlistenFn>();
    const unlisten = vi.fn();
    listenMock.mockReturnValue(pendingListen.promise);
    const source = new TauriAudioSpectrumSource();
    const firstBars = vi.fn();
    const latestBars = vi.fn();

    const firstStart = source.start(firstBars);
    const secondStart = source.start(latestBars);

    expect(listenMock).toHaveBeenCalledTimes(1);
    pendingListen.resolve(unlisten);
    await Promise.all([firstStart, secondStart]);

    const handler = listenMock.mock.calls[0][1] as (event: { payload: { bars: number[] } }) => void;
    handler({ payload: { bars: [1, 0.5] } });

    expect(firstBars).not.toHaveBeenCalled();
    expect(latestBars).toHaveBeenCalledWith(expect.any(Array));
    source.stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('TauriAudioSpectrumSource не пробрасывает rejected listen и разрешает retry', async () => {
    const unlisten = vi.fn();
    listenMock
      .mockRejectedValueOnce(new Error('event bus unavailable'))
      .mockResolvedValueOnce(unlisten);
    const source = new TauriAudioSpectrumSource();

    await expect(source.start(vi.fn())).resolves.toBeUndefined();
    expect(consoleErrorSpy).toHaveBeenCalledWith(
      'Failed to listen audio spectrum events:',
      expect.any(Error)
    );

    await source.start(vi.fn());
    expect(listenMock).toHaveBeenCalledTimes(2);
    source.stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('TauriAudioSpectrumSource отписывает pending listener если stop пришел до resolve', async () => {
    const pendingListen = deferred<UnlistenFn>();
    const unlisten = vi.fn();
    listenMock.mockReturnValue(pendingListen.promise);
    const source = new TauriAudioSpectrumSource();
    const onBars = vi.fn();

    const start = source.start(onBars);
    source.stop();
    pendingListen.resolve(unlisten);
    await start;

    expect(unlisten).toHaveBeenCalledTimes(1);
    const handler = listenMock.mock.calls[0][1] as (event: { payload: { bars: number[] } }) => void;
    handler({ payload: { bars: [1] } });
    expect(onBars).not.toHaveBeenCalled();
  });

  it('TauriAudioSpectrumSource отправляет throttled кадр в актуальный callback', async () => {
    vi.useFakeTimers();
    const unlisten = vi.fn();
    listenMock.mockResolvedValue(unlisten);
    const source = new TauriAudioSpectrumSource();
    const firstBars = vi.fn();
    const latestBars = vi.fn();

    await source.start(firstBars);
    const handler = listenMock.mock.calls[0][1] as (event: { payload: { bars: number[] } }) => void;
    (source as unknown as { lastAppliedAt: number }).lastAppliedAt = performance.now();

    handler({ payload: { bars: [0.25, 0.5] } });
    await source.start(latestBars);
    await vi.advanceTimersByTimeAsync(50);

    expect(firstBars).not.toHaveBeenCalled();
    expect(latestBars).toHaveBeenCalledWith([0.25, 0.5]);
    source.stop();
  });

  it('MicTestAudioSource не создает дубль listener при concurrent start', async () => {
    const pendingListen = deferred<UnlistenFn>();
    const unlisten = vi.fn();
    listenMock.mockReturnValue(pendingListen.promise);
    const source = new MicTestAudioSource();
    const firstBars = vi.fn();
    const latestBars = vi.fn();

    const firstStart = source.start(firstBars);
    const secondStart = source.start(latestBars);

    expect(listenMock).toHaveBeenCalledTimes(1);
    pendingListen.resolve(unlisten);
    await Promise.all([firstStart, secondStart]);

    const handler = listenMock.mock.calls[0][1] as (event: { payload: { level: number } }) => void;
    handler({ payload: { level: 0.5 } });

    expect(firstBars).not.toHaveBeenCalled();
    expect(latestBars).toHaveBeenCalledWith(expect.arrayContaining([expect.any(Number)]));
    source.stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('rejects stale capture owners and rechecks a throttled frame after Stop or A to B', async () => {
    vi.useFakeTimers();
    listenMock.mockResolvedValue(vi.fn());
    let owner: { runId: number; kind: 'capture' } | null = { runId: 10, kind: 'capture' };
    const source = new TauriAudioSpectrumSource(() => owner);
    const onBars = vi.fn();
    await source.start(onBars);
    const emit = (run_id: number, capture_generation: number) =>
      listenMock.mock.calls[0][1]({ payload: { run_id, capture_generation, bars: [0.8] } });
    (source as unknown as { lastAppliedAt: number }).lastAppliedAt = performance.now();
    emit(10, 5);
    owner = { runId: 11, kind: 'capture' };
    await vi.advanceTimersByTimeAsync(50);
    expect(onBars).not.toHaveBeenCalled();
    emit(10, 5);
    emit(11, 4);
    expect(onBars).not.toHaveBeenCalled();
    emit(11, 6);
    expect(onBars).toHaveBeenCalledTimes(1);
    emit(11, 6);
    owner = null;
    await vi.advanceTimersByTimeAsync(50);
    expect(onBars).toHaveBeenCalledTimes(1);
    source.stop();
  });

  it('accepts incoming translation only for its session and separate unleased schema', async () => {
    vi.useFakeTimers();
    listenMock.mockResolvedValue(vi.fn());
    const source = new TauriAudioSpectrumSource(() => ({ runId: 42, kind: 'translation' }));
    const onBars = vi.fn();
    await source.start(onBars);
    const emit = (run_id: number, capture_generation: number | null) =>
      listenMock.mock.calls[0][1]({ payload: { run_id, capture_generation, bars: [0.5] } });
    emit(41, null);
    emit(42, 1);
    await vi.advanceTimersByTimeAsync(50);
    expect(onBars).not.toHaveBeenCalled();
    emit(42, null);
    await vi.advanceTimersByTimeAsync(50);
    expect(onBars).toHaveBeenCalledWith([0.5]);
    source.stop();
  });

  it('MicTestAudioSource не пробрасывает rejected listen и разрешает retry', async () => {
    const unlisten = vi.fn();
    listenMock
      .mockRejectedValueOnce(new Error('mic event bus unavailable'))
      .mockResolvedValueOnce(unlisten);
    const source = new MicTestAudioSource();

    await expect(source.start(vi.fn())).resolves.toBeUndefined();
    expect(consoleErrorSpy).toHaveBeenCalledWith(
      'Failed to listen microphone test level events:',
      expect.any(Error)
    );

    await source.start(vi.fn());
    expect(listenMock).toHaveBeenCalledTimes(2);
    source.stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});


it('clears rendered A bars on owner replacement and Stop before any new sample', () => {
  const active = ref(true);
  const owner = ref({ runId: 1, kind: 'capture' as const });
  let emit!: (bars: number[]) => void;
  let visualizer!: ReturnType<typeof useAudioVisualizer>;
  const wrapper = createApp(defineComponent({
    setup() {
      visualizer = useAudioVisualizer(active, {
        barCount: 1, getOwner: () => owner.value,
        source: { start: async (onBars) => { emit = onBars; }, stop: vi.fn() },
      });
      return () => null;
    },
  }));
  wrapper.mount(document.createElement('div'));
  emit([1]);
  expect(visualizer.bars.value[0]).toBeGreaterThan(0);
  owner.value = {runId: 2, kind: 'capture'};
  expect(visualizer.bars.value).toEqual([0]);
  emit([1]);
  expect(visualizer.bars.value[0]).toBeGreaterThan(0);
  visualizer.stop();
  expect(visualizer.bars.value).toEqual([0]);
  wrapper.unmount();
});
