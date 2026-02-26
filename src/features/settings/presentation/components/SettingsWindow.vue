<script setup lang="ts">
import { ref, nextTick, onMounted, onUnmounted, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isTauriAvailable } from '@/utils/tauri';
import { useAppConfigStore } from '@/stores/appConfig';
import { useSttConfigStore } from '@/stores/sttConfig';
import UpdateDialog from '@/presentation/components/UpdateDialog.vue';
import { useSettings } from '../composables/useSettings';
import { useSettingsTheme } from '../composables/useSettingsTheme';
import { useSettingsStore } from '../../store/settingsStore';
import type { SettingsState } from '../../domain/types';
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

const { t, locale } = useI18n();
const { loadConfig, saveConfig, isSaving, isLoading, errorMessage, clearError } =
  useSettings();
const { initializeTheme } = useSettingsTheme();

const appConfigStore = useAppConfigStore();
const sttConfigStore = useSttConfigStore();
const settingsStore = useSettingsStore();

const showUpdateDialog = ref(false);
const settingsBodyRef = ref<HTMLElement | null>(null);

let unlistenOpened: UnlistenFn | null = null;

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

function disarmLiveApplyAudioDevice(): void {
  liveApplyAudioDeviceArmed.value = false;
}

async function applySelectedAudioDevice(deviceName: string): Promise<void> {
  if (!isTauriAvailable()) return;

  const seq = ++liveApplySeq;
  try {
    await invokeUpdateAppConfig({ selectedAudioDevice: deviceName });
    // Если за время invoke пользователь уже выбрал другое устройство — игнорируем этот результат.
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

onMounted(async () => {
  initializeTheme();
  if (!isTauriAvailable()) {
    await loadConfig();
    captureBaseline();
    return;
  }

  // Запускаем sync (идемпотентно — если уже запущен, сразу выходит)
  await appConfigStore.startSync();
  await sttConfigStore.startSync();

  unlistenOpened = await listen<{ scrollToSection?: string | null } | boolean>(
    'settings-window-opened',
    async (event) => {
      if (isLoading.value) return;
      disarmLiveApplyAudioDevice();
      await Promise.all([appConfigStore.refresh(), sttConfigStore.refresh()]);
      await loadConfig();
      captureBaseline();
      armLiveApplyAudioDevice();

      const payload = event.payload;
      const scrollToSection =
        payload && typeof payload === 'object' && 'scrollToSection' in payload
          ? (payload as { scrollToSection?: string | null }).scrollToSection
          : null;
      if (scrollToSection && settingsBodyRef.value) {
        const el = settingsBodyRef.value.querySelector<HTMLElement>(
          `[data-settings-section="${scrollToSection}"]`
        );
        if (el) {
          await nextTick();
          el.scrollIntoView({ behavior: 'smooth', block: 'center' });
          el.classList.add('settings-section-flash');
          setTimeout(() => el.classList.remove('settings-section-flash'), 2200);
        }
      }
    }
  );

  await loadConfig();
  captureBaseline();
  armLiveApplyAudioDevice();
});

onUnmounted(() => {
  if (unlistenOpened) {
    unlistenOpened();
  }
});

async function handleClose(opts?: { discard?: boolean }): Promise<void> {
  showUpdateDialog.value = false;
  const shouldDiscard = opts?.discard ?? true;
  if (shouldDiscard) {
    discardDraftChanges();
  } else {
    captureBaseline();
  }
  if (!isTauriAvailable()) return;

  try {
    await invoke('show_recording_window');
  } catch {}

  // Дополнительная страховка: даже если invoke по какой-то причине не сработал,
  // скрываем текущее окно настроек напрямую.
  try {
    await getCurrentWindow().hide();
  } catch {}
}

async function handleSave(): Promise<void> {
  const success = await saveConfig();
  if (!success) return;

  await handleClose({ discard: false });
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

    <div ref="settingsBodyRef" class="settings-body">
      <div v-if="isLoading" class="loading">
        <v-progress-circular indeterminate color="primary" />
      </div>

      <template v-else>
        <div class="settings-two-cols">
          <LanguageSection />
          <ThemeSection />
        </div>
        <KeytermsSection />
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
  overflow-y: scroll;
  padding: var(--spacing-md);
}

.settings-body::-webkit-scrollbar {
  width: 6px;
}

.settings-body::-webkit-scrollbar-track {
  background: transparent;
}

.settings-body::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.2);
  border-radius: 3px;
}

.settings-body::-webkit-scrollbar-thumb:hover {
  background: rgba(255, 255, 255, 0.3);
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
