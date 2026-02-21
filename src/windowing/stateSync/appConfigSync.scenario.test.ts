import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useAppConfigStore } from '@/stores/appConfig';
import {
  CMD_GET_APP_CONFIG_SNAPSHOT,
  CMD_UPDATE_APP_CONFIG,
  TOPIC_APP_CONFIG,
} from '@/windowing/stateSync';

// Это не "настоящий e2e" (без запуска Tauri), но сценарий максимально близок:
// settings → update_app_config → invalidation → main window refreshes snapshot.

const invokeMock = vi.fn();
const listenMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

describe('scenario: app-config sync across windows (mocked tauri)', () => {
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

  it('after update_app_config, main store observes new value via state-sync invalidation', async () => {
    // Source of truth on "Rust side"
    let currentRevision = '0';
    let currentData = {
      recording_hotkey: 'CmdOrCtrl+Shift+X',
      auto_copy_to_clipboard: false,
      auto_paste_text: false,
      microphone_sensitivity: 100,
      selected_audio_device: null,
    };

    invokeMock.mockImplementation(async (commandName: string, args?: any) => {
      if (commandName === CMD_GET_APP_CONFIG_SNAPSHOT) {
        return { revision: currentRevision, data: currentData };
      }

      if (commandName === CMD_UPDATE_APP_CONFIG) {
        // "Rust" applies change and emits invalidation
        if (typeof args?.autoCopyToClipboard === 'boolean') {
          currentData = { ...currentData, auto_copy_to_clipboard: args.autoCopyToClipboard };
        }
        currentRevision = String(BigInt(currentRevision) + BigInt(1));

        invalidationHandler?.({
          payload: {
            topic: TOPIC_APP_CONFIG,
            revision: currentRevision,
            sourceId: 'settings',
            timestampMs: Date.now(),
          },
        });

        return null;
      }

      throw new Error(`Unexpected invoke: ${commandName}`);
    });

    // "Main window" pinia instance
    const piniaMain = createPinia();
    setActivePinia(piniaMain);
    const appConfigMain = useAppConfigStore(piniaMain);

    await appConfigMain.startSync();
    expect(appConfigMain.autoCopyToClipboard).toBe(false);

    // "Settings window" triggers update on Rust side
    await invokeMock(CMD_UPDATE_APP_CONFIG, { autoCopyToClipboard: true });

    await vi.waitFor(() => {
      expect(appConfigMain.autoCopyToClipboard).toBe(true);
    });
  });
});

