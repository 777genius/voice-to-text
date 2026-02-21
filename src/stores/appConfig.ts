import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { isTauriAvailable } from '@/utils/tauri';
import {
  CMD_GET_APP_CONFIG_SNAPSHOT,
  TOPIC_APP_CONFIG,
  createStoreTauriTopicSync,
} from '@/windowing/stateSync';
import type { RevisionSyncHandle } from '@/windowing/stateSync';
import type { AppConfigSnapshotData, TauriSnapshotEnvelope } from '@/windowing/stateSync';

export const useAppConfigStore = defineStore('appConfig', () => {
  const revision = ref('0');
  const isLoaded = ref(false);
  const isSyncing = ref(false);

  const recordingHotkey = ref('CmdOrCtrl+Shift+X');
  const autoCopyToClipboard = ref(true);
  const autoPasteText = ref(false);
  const microphoneSensitivity = ref(100);
  const selectedAudioDevice = ref('');

  let syncHandle: RevisionSyncHandle | null = null;

  function applySnapshot(data: AppConfigSnapshotData, rev: string): void {
    revision.value = rev;
    recordingHotkey.value = data.recording_hotkey ?? recordingHotkey.value;
    autoCopyToClipboard.value = data.auto_copy_to_clipboard ?? autoCopyToClipboard.value;
    autoPasteText.value = data.auto_paste_text ?? autoPasteText.value;
    microphoneSensitivity.value = data.microphone_sensitivity ?? microphoneSensitivity.value;
    selectedAudioDevice.value = data.selected_audio_device ?? '';
    isLoaded.value = true;
  }

  async function refresh(): Promise<void> {
    if (!isTauriAvailable() || !syncHandle) return;
    await syncHandle.refresh();
  }

  async function startSync(): Promise<boolean> {
    if (!isTauriAvailable()) return false;
    // Идемпотентность: если уже запущено — считаем, что успешно.
    if (syncHandle) return true;

    const handle = createStoreTauriTopicSync<AppConfigSnapshotData>({
      topic: TOPIC_APP_CONFIG,
      commandName: CMD_GET_APP_CONFIG_SNAPSHOT,
      label: 'appConfig',
      applier: {
        apply(snapshot: TauriSnapshotEnvelope<AppConfigSnapshotData>) {
          applySnapshot(snapshot.data, snapshot.revision);
        },
      },
    });

    try {
      await handle.start();
      syncHandle = handle;
      isSyncing.value = true;
      return true;
    } catch (err) {
      handle.stop();
      console.error('[appConfig] sync start failed:', err);
      return false;
    }
  }

  function stopSync(): void {
    if (syncHandle) {
      syncHandle.stop();
      syncHandle = null;
    }
    isSyncing.value = false;
  }

  return {
    revision,
    isLoaded,
    isSyncing,
    recordingHotkey,
    autoCopyToClipboard,
    autoPasteText,
    microphoneSensitivity,
    selectedAudioDevice,

    hasSelectedAudioDevice: computed(() => Boolean(selectedAudioDevice.value)),

    refresh,
    startSync,
    stopSync,
    applySnapshot,
  };
});
