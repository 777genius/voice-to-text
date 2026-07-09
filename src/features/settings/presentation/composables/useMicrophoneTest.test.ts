import { createApp } from 'vue';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useMicrophoneTest } from './useMicrophoneTest';

const serviceMock = vi.hoisted(() => ({
  listenMicrophoneLevel: vi.fn(),
  startMicrophoneTest: vi.fn(),
  stopMicrophoneTest: vi.fn(),
}));

vi.mock('../../infrastructure/adapters/TauriSettingsService', () => ({
  tauriSettingsService: serviceMock,
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

function mountUseMicrophoneTest() {
  let api!: ReturnType<typeof useMicrophoneTest>;
  const root = document.createElement('div');
  const app = createApp({
    setup() {
      api = useMicrophoneTest();
      return () => null;
    },
  });

  document.body.appendChild(root);
  app.mount(root);

  return {
    api,
    unmount() {
      app.unmount();
      root.remove();
    },
  };
}

describe('useMicrophoneTest', () => {
  beforeEach(() => {
    serviceMock.listenMicrophoneLevel.mockReset();
    serviceMock.startMicrophoneTest.mockReset();
    serviceMock.stopMicrophoneTest.mockReset();
    serviceMock.stopMicrophoneTest.mockResolvedValue([]);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('не создает второй level listener при concurrent start', async () => {
    const pendingListen = deferred<() => void>();
    const pendingStart = deferred<void>();
    const unlisten = vi.fn();
    serviceMock.listenMicrophoneLevel.mockReturnValue(pendingListen.promise);
    serviceMock.startMicrophoneTest.mockReturnValue(pendingStart.promise);

    const wrapper = mountUseMicrophoneTest();
    try {
      const firstStart = wrapper.api.start(125, 'Mic A');
      const secondStart = wrapper.api.start(50, 'Mic B');

      expect(serviceMock.listenMicrophoneLevel).toHaveBeenCalledTimes(1);

      pendingListen.resolve(unlisten);
      await Promise.resolve();
      expect(serviceMock.startMicrophoneTest).toHaveBeenCalledTimes(1);
      expect(serviceMock.startMicrophoneTest).toHaveBeenCalledWith(125, 'Mic A');

      pendingStart.resolve();
      await Promise.all([firstStart, secondStart]);

      expect(wrapper.api.isTesting.value).toBe(true);
      wrapper.api.cleanup();
      expect(unlisten).toHaveBeenCalledTimes(1);
    } finally {
      wrapper.unmount();
    }
  });

  it('останавливает backend mic test при unmount активного теста', async () => {
    const unlisten = vi.fn();
    serviceMock.listenMicrophoneLevel.mockResolvedValue(unlisten);
    serviceMock.startMicrophoneTest.mockResolvedValue(undefined);

    const wrapper = mountUseMicrophoneTest();
    await wrapper.api.start(100, null);
    expect(wrapper.api.isTesting.value).toBe(true);

    wrapper.unmount();
    await Promise.resolve();

    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(serviceMock.stopMicrophoneTest).toHaveBeenCalledTimes(1);
    expect(wrapper.api.isTesting.value).toBe(false);
  });

  it('не вызывает backend stop, если unmount произошел до подписки на level events', async () => {
    const pendingListen = deferred<() => void>();
    const unlisten = vi.fn();
    serviceMock.listenMicrophoneLevel.mockReturnValue(pendingListen.promise);

    const wrapper = mountUseMicrophoneTest();
    const start = wrapper.api.start(100, null);
    expect(serviceMock.listenMicrophoneLevel).toHaveBeenCalledTimes(1);

    wrapper.unmount();
    pendingListen.resolve(unlisten);
    await start;

    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(serviceMock.startMicrophoneTest).not.toHaveBeenCalled();
    expect(serviceMock.stopMicrophoneTest).not.toHaveBeenCalled();
  });

  it('останавливает backend после pending start, если unmount произошел во время start', async () => {
    const unlisten = vi.fn();
    const pendingStart = deferred<void>();
    serviceMock.listenMicrophoneLevel.mockResolvedValue(unlisten);
    serviceMock.startMicrophoneTest.mockReturnValue(pendingStart.promise);

    const wrapper = mountUseMicrophoneTest();
    const start = wrapper.api.start(100, null);
    await flushMicrotasks();
    expect(serviceMock.startMicrophoneTest).toHaveBeenCalledTimes(1);

    wrapper.unmount();
    await flushMicrotasks();
    expect(serviceMock.stopMicrophoneTest).not.toHaveBeenCalled();

    pendingStart.resolve();
    await start;

    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(serviceMock.stopMicrophoneTest).toHaveBeenCalledTimes(1);
    expect(wrapper.api.isTesting.value).toBe(false);
  });
});
