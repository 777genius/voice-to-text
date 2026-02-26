<script setup lang="ts">
/**
 * Главный компонент панели настроек
 * Объединяет все секции и управляет загрузкой/сохранением
 */

import { ref, nextTick, onMounted, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import UpdateDialog from '@/presentation/components/UpdateDialog.vue';
import { useSettings } from '../composables/useSettings';
import { useSettingsTheme } from '../composables/useSettingsTheme';
import { useSettingsStore } from '../../store/settingsStore';
import type { SettingsState } from '../../domain/types';
import { isTauriAvailable } from '@/utils/tauri';
import { invokeUpdateAppConfig } from '@/windowing/stateSync';

// Секции
import LanguageSection from './sections/LanguageSection.vue';
import KeytermsSection from './sections/KeytermsSection.vue';
import ThemeSection from './sections/ThemeSection.vue';
import HotkeySection from './sections/HotkeySection.vue';
import AutoActionsSection from './sections/AutoActionsSection.vue';
import AudioDeviceSection from './sections/AudioDeviceSection.vue';
import MicTestSection from './sections/MicTestSection.vue';
import UpdatesSection from './sections/UpdatesSection.vue';

const emit = defineEmits<{
  close: [];
}>();

const { t, locale } = useI18n();
const {
  loadConfig,
  saveConfig,
  isSaving,
  isLoading,
  errorMessage,
  clearError,
} = useSettings();

// Инициализация темы
const { initializeTheme } = useSettingsTheme();

// Диалог обновления
const showUpdateDialog = ref(false);
const settingsStore = useSettingsStore();
const settingsContentRef = ref<HTMLElement | null>(null);

// Загрузка конфигурации при монтировании
onMounted(async () => {
  initializeTheme();
  await loadConfig();
  captureBaseline();
  armLiveApplyAudioDevice();
});

async function scrollToPendingSection(): Promise<void> {
  const sectionId = settingsStore.pendingScrollToSection;
  if (!sectionId || !settingsContentRef.value) return;
  settingsStore.pendingScrollToSection = null;
  await nextTick();
  const el = settingsContentRef.value.querySelector<HTMLElement>(
    `[data-settings-section="${sectionId}"]`
  );
  if (el) {
    el.scrollIntoView({ behavior: 'smooth', block: 'center' });
    el.classList.add('settings-section-flash');
    setTimeout(() => el.classList.remove('settings-section-flash'), 2200);
  }
}

watch(
  () => isLoading.value,
  async (loading) => {
    if (!loading) await scrollToPendingSection();
  },
  { immediate: true }
);

const baselineState = ref<SettingsState | null>(null);
const baselineUiLocale = ref<string>('');

const liveApplyAudioDeviceArmed = ref(false);
let lastAppliedAudioDevice = '';
let liveApplySeq = 0;

function snapshotSettingsState(): SettingsState {
  return {
    provider: settingsStore.provider,
    language: settingsStore.language,
    deepgramApiKey: settingsStore.deepgramApiKey,
    assemblyaiApiKey: settingsStore.assemblyaiApiKey,
    whisperModel: settingsStore.whisperModel,
    theme: settingsStore.theme,
    useSystemTheme: settingsStore.useSystemTheme,
    recordingHotkey: settingsStore.recordingHotkey,
    microphoneSensitivity: settingsStore.microphoneSensitivity,
    selectedAudioDevice: settingsStore.selectedAudioDevice,
    autoCopyToClipboard: settingsStore.autoCopyToClipboard,
    autoPasteText: settingsStore.autoPasteText,
    deepgramKeyterms: settingsStore.deepgramKeyterms,
  };
}

function captureBaseline(): void {
  baselineState.value = snapshotSettingsState();
  baselineUiLocale.value = String(locale.value ?? '');
}

function armLiveApplyAudioDevice(): void {
  if (!isTauriAvailable()) {
    liveApplyAudioDeviceArmed.value = false;
    return;
  }
  lastAppliedAudioDevice = settingsStore.selectedAudioDevice;
  liveApplyAudioDeviceArmed.value = true;
}

async function applySelectedAudioDevice(deviceName: string): Promise<void> {
  if (!isTauriAvailable()) return;
  const seq = ++liveApplySeq;
  try {
    await invokeUpdateAppConfig({ selectedAudioDevice: deviceName });
    if (seq !== liveApplySeq) return;
    lastAppliedAudioDevice = deviceName;
    if (baselineState.value) {
      baselineState.value.selectedAudioDevice = deviceName;
    }
  } catch (err) {
    console.error('Не удалось применить устройство записи:', err);
  }
}

function discardDraftChanges(): void {
  if (baselineState.value) {
    settingsStore.applyState(baselineState.value);
  }
  if (baselineUiLocale.value) {
    locale.value = baselineUiLocale.value;
    document.documentElement.dataset.uiLocale = baselineUiLocale.value;
  }
}

function handleClose(arg?: unknown): void {
  // Этот хендлер используется как:
  // - @click (Vue передаёт MouseEvent)
  // - handleClose({ discard: false })
  // Поэтому принимаем unknown и различаем по форме.
  const shouldDiscard =
    typeof arg === 'object' && arg !== null && 'discard' in arg
      ? Boolean((arg as { discard?: boolean }).discard ?? true)
      : true;
  if (shouldDiscard) {
    discardDraftChanges();
  } else {
    captureBaseline();
  }
  emit('close');
}

// Сохранение и закрытие
async function handleSave() {
  const success = await saveConfig();
  if (success) {
    handleClose({ discard: false });
  }
}

watch(
  () => settingsStore.selectedAudioDevice,
  (next, prev) => {
    if (!isTauriAvailable()) return;
    if (!liveApplyAudioDeviceArmed.value) return;
    if (next === prev) return;
    if (next === lastAppliedAudioDevice) return;
    void applySelectedAudioDevice(next);
  }
);
</script>

<template>
  <div class="settings-overlay" @click.self="handleClose">
    <v-card class="settings-panel" elevation="0">
      <!-- Заголовок -->
      <v-card-title class="d-flex justify-space-between align-center pa-4">
        <span class="text-h6">{{ t('settings.title') }}</span>
        <v-btn
          icon="mdi-close"
          variant="text"
          size="small"
          @click="handleClose"
        />
      </v-card-title>

      <v-divider />

      <!-- Контент -->
      <v-card-text ref="settingsContentRef" class="settings-content pa-4">
        <!-- Индикатор загрузки -->
        <div v-if="isLoading" class="d-flex justify-center py-8">
          <v-progress-circular indeterminate color="primary" />
        </div>

        <template v-else>
          <!-- Язык + тема в две колонки -->
          <div class="settings-two-cols">
            <LanguageSection />
            <ThemeSection />
          </div>

          <!-- Ключевые термины -->
          <KeytermsSection />

          <!-- Горячая клавиша -->
          <HotkeySection />

          <!-- Автоматические действия -->
          <AutoActionsSection />

          <!-- Выбор аудио устройства -->
          <AudioDeviceSection />

          <!-- Тест микрофона -->
          <MicTestSection />

          <!-- Обновления -->
          <UpdatesSection @show-update-dialog="showUpdateDialog = true" />

          <!-- Ошибка сохранения -->
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
      </v-card-text>

      <v-divider />

      <!-- Футер с кнопками -->
      <v-card-actions class="pa-4">
        <v-spacer />
        <v-btn
          variant="text"
          @click="handleClose"
        >
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
      </v-card-actions>
    </v-card>

    <!-- Диалог обновления -->
    <UpdateDialog v-model="showUpdateDialog" />
  </div>
</template>

<style scoped>
.settings-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  display: flex;
  z-index: 1000;
  -webkit-app-region: no-drag;
  background: rgba(0, 0, 0, 0.5);
}

.settings-panel {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  border-radius: 0 !important;
}

.settings-content {
  flex: 1;
  overflow-y: auto;
  overflow-x: hidden;
}

/* Скроллбар для контента */
.settings-content::-webkit-scrollbar {
  width: 8px;
}

.settings-content::-webkit-scrollbar-track {
  background: transparent;
}

.settings-content::-webkit-scrollbar-thumb {
  background: rgba(var(--v-theme-on-surface), 0.2);
  border-radius: 4px;
}

.settings-content::-webkit-scrollbar-thumb:hover {
  background: rgba(var(--v-theme-on-surface), 0.3);
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

:deep(.settings-section-flash) {
  border-radius: var(--radius-md, 8px);
  animation: settings-section-flash 2.2s ease-out;
}

@keyframes settings-section-flash {
  0% {
    box-shadow: 0 0 0 0 rgba(var(--v-theme-primary), 0.45);
  }
  25% {
    box-shadow: 0 0 0 6px rgba(var(--v-theme-primary), 0.3);
  }
  100% {
    box-shadow: 0 0 0 6px transparent;
  }
}
</style>
