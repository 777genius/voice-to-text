import { createApp, nextTick } from 'vue';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import ModelManager from './ModelManager.vue';

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('vue-i18n', () => ({
  useI18n: () => ({
    t: (key: string) => key,
  }),
}));

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
  await nextTick();
}

function mountModelManager() {
  const root = document.createElement('div');
  const app = createApp(ModelManager);
  document.body.appendChild(root);
  app.mount(root);

  return {
    root,
    unmount() {
      app.unmount();
      root.remove();
    },
  };
}

describe('ModelManager listener lifecycle', () => {
  let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'get_available_whisper_models') return [];
      return undefined;
    });
  });

  afterEach(() => {
    consoleErrorSpy.mockRestore();
    document.body.innerHTML = '';
  });

  it('очищает уже зарегистрированный listener, если следующий download listener не поднялся', async () => {
    const unlistenStarted = vi.fn();
    listenMock
      .mockResolvedValueOnce(unlistenStarted)
      .mockRejectedValueOnce(new Error('progress listener failed'));

    const wrapper = mountModelManager();
    for (let i = 0; i < 20 && listenMock.mock.calls.length < 2; i++) {
      await flushMicrotasks();
    }
    await flushMicrotasks();

    expect(unlistenStarted).toHaveBeenCalledTimes(1);
    expect(consoleErrorSpy).toHaveBeenCalledWith(
      'Failed to listen whisper model download events:',
      expect.any(Error)
    );
    expect(wrapper.root.textContent).toContain('Error: progress listener failed');

    wrapper.unmount();
  });
});
