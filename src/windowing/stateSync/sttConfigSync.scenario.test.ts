import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import { useSttConfigStore } from '@/stores/sttConfig';
import {
  CMD_GET_STT_CONFIG_SNAPSHOT,
  CMD_UPDATE_STT_CONFIG,
  STATE_SYNC_INVALIDATION_EVENT,
  TOPIC_STT_CONFIG,
} from '@/windowing/stateSync';
import { SttProviderType } from '@/types';

// Это не настоящий e2e (без Tauri runtime), но сценарий приближен:
// settings → update_stt_config → invalidation → main window refreshes snapshot.

const invokeMock = vi.fn();
const listenMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

describe('scenario: stt-config sync across windows (mocked tauri)', () => {
  let invalidationHandler: ((event: any) => void) | null = null;

  beforeEach(() => {
    (window as any).__TAURI__ = {};
    invalidationHandler = null;
    invokeMock.mockReset();
    listenMock.mockReset();

    listenMock.mockImplementation(async (_eventName: string, handler: any) => {
      invalidationHandler = handler;
      return () => {};
    });
  });

  it('after update_stt_config, main store observes new value via state-sync invalidation', async () => {
    let currentRevision = '0';
    let currentData = {
      provider: SttProviderType.Backend,
      language: 'ru',
      auto_detect_language: false,
      enable_punctuation: true,
      filter_profanity: false,
      deepgram_api_key: null,
      assemblyai_api_key: null,
      model: null,
      keep_connection_alive: false,
    };

    invokeMock.mockImplementation(async (commandName: string, args?: any) => {
      if (commandName === CMD_GET_STT_CONFIG_SNAPSHOT) {
        return { revision: currentRevision, data: currentData };
      }

      if (commandName === CMD_UPDATE_STT_CONFIG) {
        // «Rust» применяет изменения
        if (typeof args?.language === 'string') {
          currentData = { ...currentData, language: args.language };
        }

        currentRevision = String(BigInt(currentRevision) + BigInt(1));

        // и эмитит invalidation
        invalidationHandler?.({
          payload: {
            topic: TOPIC_STT_CONFIG,
            revision: currentRevision,
            sourceId: 'settings',
            timestampMs: Date.now(),
          },
        });

        return null;
      }

      throw new Error(`Unexpected invoke: ${commandName}`);
    });

    const piniaMain = createPinia();
    setActivePinia(piniaMain);
    const sttConfigMain = useSttConfigStore(piniaMain);

    await sttConfigMain.startSync();
    expect(listenMock).toHaveBeenCalledWith(STATE_SYNC_INVALIDATION_EVENT, expect.any(Function));
    expect(sttConfigMain.language).toBe('ru');

    await invokeMock(CMD_UPDATE_STT_CONFIG, { language: 'en' });

    await vi.waitFor(() => {
      expect(sttConfigMain.language).toBe('en');
    });
  });
});
