import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { isTauriAvailable } from '@/utils/tauri';
import {
  CMD_GET_APP_CONFIG_SNAPSHOT,
  TOPIC_APP_CONFIG,
  createStoreTauriTopicSync,
} from '@/windowing/stateSync';
import type { RevisionSyncHandle } from '@/windowing/stateSync';
import type {
  AppConfigSnapshotData,
  RecordingMode,
  TauriSnapshotEnvelope,
} from '@/windowing/stateSync';

export const useAppConfigStore = defineStore('appConfig', () => {
  const revision = ref('0');
  const isLoaded = ref(false);
  const isSyncing = ref(false);

  const recordingHotkey = ref('CmdOrCtrl+Shift+X');
  const autoCopyToClipboard = ref(false);
  const autoPasteText = ref(true);
  const playCompletionSound = ref(false);
  const hideRecordingWindowOnHotkey = ref(false);
  const showMiniRecordingWindow = ref(true);
  const keepRecordingUntilManualStop = ref(false);
  const microphoneSensitivity = ref(100);
  const selectedAudioDevice = ref('');
  const recordingMode = ref<RecordingMode>('dictation');
  const openaiApiKey = ref('');

  let syncHandle: RevisionSyncHandle | null = null;

  function applySnapshot(data: AppConfigSnapshotData, rev: string): void {
    revision.value = rev;
    recordingHotkey.value = data.recording_hotkey ?? recordingHotkey.value;
    autoCopyToClipboard.value = data.auto_copy_to_clipboard ?? autoCopyToClipboard.value;
    autoPasteText.value = data.auto_paste_text ?? autoPasteText.value;
    playCompletionSound.value = data.play_completion_sound ?? playCompletionSound.value;
    hideRecordingWindowOnHotkey.value =
      data.hide_recording_window_on_hotkey ?? hideRecordingWindowOnHotkey.value;
    showMiniRecordingWindow.value =
      data.show_mini_recording_window ?? showMiniRecordingWindow.value;
    keepRecordingUntilManualStop.value =
      data.keep_recording_until_manual_stop ?? keepRecordingUntilManualStop.value;
    microphoneSensitivity.value = data.microphone_sensitivity ?? microphoneSensitivity.value;
    selectedAudioDevice.value = data.selected_audio_device ?? '';
    recordingMode.value = data.recording_mode ?? recordingMode.value;
    openaiApiKey.value = data.openai_api_key ?? '';
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
    playCompletionSound,
    hideRecordingWindowOnHotkey,
    showMiniRecordingWindow,
    keepRecordingUntilManualStop,
    microphoneSensitivity,
    selectedAudioDevice,
    recordingMode,
    openaiApiKey,

    hasSelectedAudioDevice: computed(() => Boolean(selectedAudioDevice.value)),

    refresh,
    startSync,
    stopSync,
    applySnapshot,
  };
});
