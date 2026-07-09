import { createApp, ref } from 'vue';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  createSettingsSectionFlashController,
  useSettingsScrollToSectionListener,
} from './useSettingsScrollToSection';

const listenMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('@/utils/tauri', () => ({
  isTauriAvailable: () => true,
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

function mountScrollListener() {
  const root = document.createElement('div');
  const app = createApp({
    setup() {
      useSettingsScrollToSectionListener(ref(null));
      return () => null;
    },
  });
  document.body.appendChild(root);
  app.mount(root);

  return {
    unmount() {
      app.unmount();
      root.remove();
    },
  };
}

describe('useSettingsScrollToSectionListener', () => {
  beforeEach(() => {
    listenMock.mockReset();
    vi.useRealTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = '';
  });

  it('отписывает listener, если listen resolve пришел после unmount', async () => {
    const pendingListen = deferred<() => void>();
    const unlisten = vi.fn();
    listenMock.mockReturnValue(pendingListen.promise);
    const wrapper = mountScrollListener();

    for (let i = 0; i < 20 && listenMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }
    expect(listenMock).toHaveBeenCalledTimes(1);

    wrapper.unmount();
    pendingListen.resolve(unlisten);
    await flushMicrotasks();

    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('продлевает highlight при повторном flash той же секции', () => {
    vi.useFakeTimers();
    const el = document.createElement('section');
    const flash = createSettingsSectionFlashController(2200);

    flash.flash(el);
    vi.advanceTimersByTime(1500);
    flash.flash(el);
    vi.advanceTimersByTime(1000);

    expect(el.classList.contains('settings-section-flash')).toBe(true);

    vi.advanceTimersByTime(1200);

    expect(el.classList.contains('settings-section-flash')).toBe(false);
  });

  it('cleanup снимает pending highlight и отменяет таймер', () => {
    vi.useFakeTimers();
    const el = document.createElement('section');
    const flash = createSettingsSectionFlashController(2200);

    flash.flash(el);
    flash.cleanup();
    vi.advanceTimersByTime(2200);

    expect(el.classList.contains('settings-section-flash')).toBe(false);
  });
});
