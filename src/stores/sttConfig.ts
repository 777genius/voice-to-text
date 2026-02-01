import { defineStore } from 'pinia';
import { ref } from 'vue';
import { isTauriAvailable } from '@/utils/tauri';
import {
  CMD_GET_STT_CONFIG_SNAPSHOT,
  TOPIC_STT_CONFIG,
  createStoreTauriTopicSync,
} from '@/windowing/stateSync';
import type { RevisionSyncHandle } from '@/windowing/stateSync';
import type { SttConfigSnapshotData, TauriSnapshotEnvelope } from '@/windowing/stateSync';
import { SttProviderType } from '@/types';

export const useSttConfigStore = defineStore('sttConfig', () => {
  const revision = ref('0');
  const isLoaded = ref(false);
  const isSyncing = ref(false);

  const provider = ref<SttProviderType>(SttProviderType.Backend);
  const language = ref('ru');
  const autoDetectLanguage = ref(false);
  const enablePunctuation = ref(true);
  const filterProfanity = ref(false);
  const deepgramApiKey = ref<string | null>(null);
  const assemblyaiApiKey = ref<string | null>(null);
  const model = ref<string | null>(null);
  const keepConnectionAlive = ref(false);

  let syncHandle: RevisionSyncHandle | null = null;

  function applySnapshot(data: SttConfigSnapshotData, rev: string): void {
    revision.value = rev;
    provider.value = data.provider ?? provider.value;
    language.value = data.language ?? language.value;
    autoDetectLanguage.value = data.auto_detect_language ?? autoDetectLanguage.value;
    enablePunctuation.value = data.enable_punctuation ?? enablePunctuation.value;
    filterProfanity.value = data.filter_profanity ?? filterProfanity.value;
    deepgramApiKey.value = data.deepgram_api_key ?? null;
    assemblyaiApiKey.value = data.assemblyai_api_key ?? null;
    model.value = data.model ?? null;
    keepConnectionAlive.value = data.keep_connection_alive ?? keepConnectionAlive.value;
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

    const handle = createStoreTauriTopicSync<SttConfigSnapshotData>({
      topic: TOPIC_STT_CONFIG,
      commandName: CMD_GET_STT_CONFIG_SNAPSHOT,
      label: 'sttConfig',
      applier: {
        apply(snapshot: TauriSnapshotEnvelope<SttConfigSnapshotData>) {
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
      console.error('[sttConfig] sync start failed:', err);
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
    provider,
    language,
    autoDetectLanguage,
    enablePunctuation,
    filterProfanity,
    deepgramApiKey,
    assemblyaiApiKey,
    model,
    keepConnectionAlive,

    refresh,
    startSync,
    stopSync,
    applySnapshot,
  };
});
