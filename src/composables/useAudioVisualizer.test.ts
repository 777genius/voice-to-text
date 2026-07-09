import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TauriAudioSpectrumSource } from './useAudioVisualizer';
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
