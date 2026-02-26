import { beforeEach, describe, expect, it, vi, afterEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useSettingsStore } from './settingsStore';
import { CMD_UPDATE_STT_CONFIG } from '@/windowing/stateSync';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

describe('settingsStore deepgramKeyterms persistence (debounced)', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    (window as any).__TAURI__ = {};
    localStorage.clear();
    invokeMock.mockReset();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('setDeepgramKeyterms вызывает update_stt_config с debounce', async () => {
    const store = useSettingsStore();

    store.setDeepgramKeyterms('Kubernetes, VoicetextAI');
    expect(invokeMock).not.toHaveBeenCalled();

    vi.advanceTimersByTime(449);
    expect(invokeMock).not.toHaveBeenCalled();

    vi.advanceTimersByTime(1);
    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_STT_CONFIG, {
      provider: 'backend',
      language: 'ru',
      deepgramKeyterms: 'Kubernetes, VoicetextAI',
    });
  });

  it('пустые/пробельные keyterms сохраняются как null', () => {
    const store = useSettingsStore();

    store.setDeepgramKeyterms('Deepgram');
    vi.advanceTimersByTime(450);
    expect(invokeMock).toHaveBeenCalledTimes(1);

    store.setDeepgramKeyterms('   \n\t  ');
    vi.advanceTimersByTime(450);

    expect(invokeMock).toHaveBeenCalledWith(CMD_UPDATE_STT_CONFIG, {
      provider: 'backend',
      language: 'ru',
      deepgramKeyterms: null,
    });
  });

  it('повтор того же значения не вызывает лишний invoke', () => {
    const store = useSettingsStore();

    store.setDeepgramKeyterms('Deepgram');
    vi.advanceTimersByTime(450);
    expect(invokeMock).toHaveBeenCalledTimes(1);

    // то же самое, но с лишними пробелами — normalizeKeytermsForPersist даст тот же результат
    store.setDeepgramKeyterms('  Deepgram  ');
    vi.advanceTimersByTime(450);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});

