<script setup lang="ts">
/**
 * Секция выбора режима записи: классический STT (dictation) или
 * realtime-перевод в platform virtual microphone (live_translation).
 */

import { computed, onMounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { invoke } from '@tauri-apps/api/core';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';
import type { RecordingMode } from '../../../domain/types';
import { isTauriAvailable } from '@/utils/tauri';

const { t } = useI18n();
const { recordingMode, openaiApiKey } = useSettings();
const showOpenaiKey = ref(false);
const platformStatus = ref<LiveTranslationPlatformStatus | null>(null);
const platformStatusLoading = ref(false);

const options: Array<{ value: RecordingMode; labelKey: string }> = [
  { value: 'dictation', labelKey: 'settings.recordingMode.dictation' },
  { value: 'live_translation', labelKey: 'settings.recordingMode.liveTranslation' },
];

type PlatformSetupState =
  | 'ready'
  | 'missing_dependency'
  | 'missing_virtual_device'
  | 'unsupported'
  | 'error';

interface LiveTranslationPlatformStatus {
  platform: string;
  status: PlatformSetupState;
  outgoing_supported: boolean;
  incoming_supported: boolean;
  virtual_microphone_name: string;
  message: string;
}

const platformStatusType = computed(() => {
  if (!platformStatus.value) return 'info';
  return platformStatus.value.status === 'ready' ? 'success' : 'warning';
});

const platformStatusMessage = computed(() => {
  if (platformStatusLoading.value) {
    return t('settings.recordingMode.platformStatusChecking');
  }
  return (
    platformStatus.value?.message ||
    t('settings.recordingMode.platformStatusUnavailable')
  );
});

async function loadPlatformStatus(): Promise<void> {
  if (recordingMode.value !== 'live_translation' || !isTauriAvailable()) {
    return;
  }
  platformStatusLoading.value = true;
  try {
    platformStatus.value = await invoke<LiveTranslationPlatformStatus>(
      'get_live_translation_platform_status',
    );
  } catch (error) {
    platformStatus.value = {
      platform: navigator.platform,
      status: 'error',
      outgoing_supported: false,
      incoming_supported: false,
      virtual_microphone_name: '',
      message: error instanceof Error ? error.message : String(error),
    };
  } finally {
    platformStatusLoading.value = false;
  }
}

onMounted(loadPlatformStatus);
watch(recordingMode, (mode) => {
  if (mode === 'live_translation') {
    void loadPlatformStatus();
  }
});
</script>

<template>
  <SettingGroup :title="t('settings.recordingMode.label')">
    <v-btn-toggle
      v-model="recordingMode"
      mandatory
      density="comfortable"
      color="primary"
      variant="outlined"
      class="recording-mode-toggle"
    >
      <v-btn
        v-for="opt in options"
        :key="opt.value"
        :value="opt.value"
        size="small"
      >
        {{ t(opt.labelKey) }}
      </v-btn>
    </v-btn-toggle>

    <div class="mt-2">
      <span class="text-caption text-medium-emphasis">
        {{ t('settings.recordingMode.hintBody') }}
      </span>
    </div>

    <v-expand-transition>
      <div v-if="recordingMode === 'live_translation'" class="openai-key-block">
        <div class="text-caption text-medium-emphasis mb-1">
          {{ t('settings.openaiApiKey.label') }}
        </div>
        <v-text-field
          v-model="openaiApiKey"
          :type="showOpenaiKey ? 'text' : 'password'"
          :placeholder="t('settings.openaiApiKey.placeholder')"
          density="comfortable"
          hide-details
          autocomplete="new-password"
          spellcheck="false"
          autocapitalize="off"
          :append-inner-icon="showOpenaiKey ? 'mdi-eye-off' : 'mdi-eye'"
          @click:append-inner="showOpenaiKey = !showOpenaiKey"
        />
        <div class="text-caption text-medium-emphasis mt-2">
          {{ t('settings.openaiApiKey.hint') }}
        </div>
        <v-alert
          class="platform-status mt-3"
          :type="platformStatusType"
          variant="tonal"
          density="compact"
        >
          <div class="platform-status__message">
            {{ platformStatusMessage }}
          </div>
          <div
            v-if="platformStatus?.virtual_microphone_name"
            class="platform-status__device"
          >
            {{
              t('settings.recordingMode.virtualMicrophone', {
                name: platformStatus.virtual_microphone_name,
              })
            }}
          </div>
        </v-alert>
      </div>
    </v-expand-transition>
  </SettingGroup>
</template>

<style scoped>
.recording-mode-toggle {
  flex-wrap: wrap;
}

.openai-key-block {
  margin-top: 14px;
}

.platform-status {
  font-size: 12px;
}

.platform-status__message,
.platform-status__device {
  line-height: 1.35;
}

.platform-status__device {
  margin-top: 4px;
  opacity: 0.78;
}
</style>
