<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { isTauriAvailable } from '@/utils/tauri';
import { useAppConfigStore } from '@/stores/appConfig';
import { useSttConfigStore } from '@/stores/sttConfig';
import UpdateDialog from '@/presentation/components/UpdateDialog.vue';
import { useSettings } from '../composables/useSettings';
import { useSettingsTheme } from '../composables/useSettingsTheme';

// Секции
import LanguageSection from './sections/LanguageSection.vue';
import ThemeSection from './sections/ThemeSection.vue';
import HotkeySection from './sections/HotkeySection.vue';
import AutoActionsSection from './sections/AutoActionsSection.vue';
import AudioDeviceSection from './sections/AudioDeviceSection.vue';
import MicTestSection from './sections/MicTestSection.vue';
import UpdatesSection from './sections/UpdatesSection.vue';

const { t } = useI18n();
const { loadConfig, saveConfig, isSaving, isLoading, errorMessage, clearError } =
  useSettings();
const { initializeTheme } = useSettingsTheme();

const appConfigStore = useAppConfigStore();
const sttConfigStore = useSttConfigStore();

const showUpdateDialog = ref(false);

let unlistenOpened: UnlistenFn | null = null;

onMounted(async () => {
  initializeTheme();
  if (!isTauriAvailable()) {
    await loadConfig();
    return;
  }

  // Запускаем sync (идемпотентно — если уже запущен, сразу выходит)
  await appConfigStore.startSync();
  await sttConfigStore.startSync();

  unlistenOpened = await listen<boolean>('settings-window-opened', async () => {
    if (isLoading.value) return;
    // Подтягиваем свежий конфиг через per-topic handles, дожидаемся завершения
    await Promise.all([appConfigStore.refresh(), sttConfigStore.refresh()]);
    await loadConfig();
  });

  await loadConfig();
});

onUnmounted(() => {
  if (unlistenOpened) {
    unlistenOpened();
  }
});

async function handleClose(): Promise<void> {
  showUpdateDialog.value = false;
  try {
    await invoke('show_recording_window');
  } catch {}
}

async function handleSave(): Promise<void> {
  const success = await saveConfig();
  if (!success) return;

  await handleClose();
}
</script>

<template>
  <div class="settings-window">
    <div class="settings-header" data-tauri-drag-region>
      <div class="settings-title">
        {{ t('settings.title') }}
      </div>
      <v-btn
        class="no-drag"
        icon="mdi-close"
        variant="text"
        size="small"
        @click="handleClose"
      />
    </div>

    <div class="settings-body">
      <div v-if="isLoading" class="loading">
        <v-progress-circular indeterminate color="primary" />
      </div>

      <template v-else>
        <div class="settings-two-cols">
          <LanguageSection />
          <ThemeSection />
        </div>
        <HotkeySection />
        <AutoActionsSection />
        <AudioDeviceSection />
        <MicTestSection />
        <UpdatesSection @show-update-dialog="showUpdateDialog = true" />

        <v-alert
          v-if="errorMessage"
          type="error"
          variant="tonal"
          closable
          class="mt-4"
          @click:close="clearError"
        >
          {{ errorMessage }}
        </v-alert>
      </template>
    </div>

    <div class="settings-footer">
      <v-spacer />
      <v-btn variant="text" @click="handleClose">
        {{ t('settings.cancel') }}
      </v-btn>
      <v-btn
        color="primary"
        :loading="isSaving"
        :disabled="isLoading"
        @click="handleSave"
      >
        {{ isSaving ? t('settings.saving') : t('settings.save') }}
      </v-btn>
    </div>

    <UpdateDialog v-model="showUpdateDialog" />
  </div>
</template>

<style scoped>
.settings-window {
  width: 100%;
  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--glass-bg);
  border: 1px solid var(--glass-border);
  border-radius: var(--radius-xl);
  overflow: hidden;
}

:global(.theme-light) .settings-window {
  background: rgba(255, 255, 255, 0.98);
}

.settings-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--spacing-md);
  border-bottom: 1px solid var(--glass-border);
}

.settings-title {
  font-size: 16px;
  font-weight: 600;
}

.settings-body {
  flex: 1;
  overflow-y: auto;
  padding: var(--spacing-md);
}

.settings-footer {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
  padding: var(--spacing-md);
  border-top: 1px solid var(--glass-border);
}

.loading {
  display: flex;
  justify-content: center;
  padding: var(--spacing-xl);
}

.settings-two-cols {
  display: grid;
  grid-template-columns: 1fr;
  gap: var(--spacing-xl);
}

@media (min-width: 600px) {
  .settings-two-cols {
    grid-template-columns: 1fr 1fr;
  }
}
</style>
