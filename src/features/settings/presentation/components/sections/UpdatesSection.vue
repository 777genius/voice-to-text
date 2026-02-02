<script setup lang="ts">
/**
 * Секция проверки и установки обновлений
 */

import { onMounted, onUnmounted, ref, nextTick } from 'vue';
import { useI18n } from 'vue-i18n';
import { useUpdater } from '@/composables/useUpdater';
import SettingGroup from '../shared/SettingGroup.vue';
import { isTauriAvailable } from '@/utils/tauri';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { EVENT_SETTINGS_FOCUS_UPDATES } from '@/types';

const emit = defineEmits<{
  'show-update-dialog': [];
}>();

const { t } = useI18n();
const { store: updateStore, checkForUpdates, loadCurrentVersion } = useUpdater();

const rootEl = ref<HTMLElement | null>(null);
const isHighlighted = ref(false);

let highlightTimer: number | null = null;
let unlistenFocus: UnlistenFn | null = null;

const SETTINGS_PENDING_FOCUS_KEY = 'settings:pending-focus';

function runHighlight(): void {
  isHighlighted.value = true;
  if (highlightTimer !== null) {
    window.clearTimeout(highlightTimer);
  }
  highlightTimer = window.setTimeout(() => {
    isHighlighted.value = false;
    highlightTimer = null;
  }, 1600);
}

async function focusThisSection(): Promise<void> {
  // Даём Vue дорендерить, чтобы scrollIntoView был стабильным
  await nextTick();
  rootEl.value?.scrollIntoView({ behavior: 'smooth', block: 'center' });
  runHighlight();
}

function consumePendingFocusFromStorage(): boolean {
  try {
    const raw = localStorage.getItem(SETTINGS_PENDING_FOCUS_KEY);
    if (!raw) return false;
    const parsed = JSON.parse(raw) as { target?: string } | null;
    if (parsed?.target !== 'updates') return false;
    localStorage.removeItem(SETTINGS_PENDING_FOCUS_KEY);
    return true;
  } catch {
    return false;
  }
}

onMounted(async () => {
  if (isTauriAvailable()) {
    await loadCurrentVersion();

    // Если апдейт уже известен (например, пришли сюда с бейджа) — не дёргаем check повторно.
    if (!updateStore.availableVersion) {
      await checkForUpdates();
    }

    // Фокус из других окон (бейдж/трей)
    try {
      unlistenFocus = await listen(EVENT_SETTINGS_FOCUS_UPDATES, async () => {
        await focusThisSection();
      });
    } catch {}
  }

  // Fallback: если фокус запросили до того, как окно было готово/слушатель повесился
  // или если мы в web-режиме (без tauri).
  if (consumePendingFocusFromStorage()) {
    await focusThisSection();
  }
});

onUnmounted(() => {
  if (unlistenFocus) {
    unlistenFocus();
    unlistenFocus = null;
  }
  if (highlightTimer !== null) {
    window.clearTimeout(highlightTimer);
    highlightTimer = null;
  }
});
</script>

<template>
  <div
    ref="rootEl"
    class="updates-section"
    :class="{ 'updates-section--highlight': isHighlighted }"
  >
    <SettingGroup :title="t('settings.updates.label')">
      <div class="d-flex flex-column ga-3">
        <div v-if="updateStore.currentVersion" class="text-caption text-medium-emphasis">
          {{ t('settings.updates.currentVersion', { version: updateStore.currentVersion }) }}
        </div>

        <v-progress-linear
          v-if="updateStore.isChecking && !updateStore.availableVersion"
          indeterminate
          height="6"
          rounded
          color="primary"
        />

        <!-- Доступное обновление -->
        <v-alert
          v-if="updateStore.availableVersion"
          type="success"
          variant="tonal"
        >
          <div class="d-flex flex-column">
            <div class="d-flex align-center ga-2 mb-1">
              <v-icon>mdi-party-popper</v-icon>
              <span class="font-weight-medium">
                {{ t('settings.updates.availableTitle', { version: updateStore.availableVersion }) }}
              </span>
            </div>
            <div class="text-body-2 mb-2">
              {{ t('settings.updates.availableSubtitle') }}
            </div>
            <v-btn
              color="success"
              variant="flat"
              size="small"
              class="align-self-start"
              @click="emit('show-update-dialog')"
            >
              {{ t('settings.updates.update') }}
            </v-btn>
          </div>
        </v-alert>

        <!-- Последняя версия -->
        <v-alert
          v-if="updateStore.isLatest && !updateStore.availableVersion && !updateStore.isChecking"
          type="info"
          variant="tonal"
          density="compact"
        >
          {{ t('settings.updates.latest') }}
        </v-alert>

        <!-- Сообщение об ошибке -->
        <v-alert
          v-if="updateStore.error && !updateStore.availableVersion"
          type="error"
          variant="tonal"
          density="compact"
        >
          {{ updateStore.error }}
        </v-alert>

        <!-- Кнопка проверки обновлений показываем только если была ошибка -->
        <v-btn
          v-if="updateStore.error && !updateStore.isChecking && !updateStore.availableVersion"
          color="primary"
          variant="flat"
          class="align-self-start"
          @click="checkForUpdates"
        >
          <v-icon start>mdi-update</v-icon>
          {{ t('settings.updates.check') }}
        </v-btn>
      </div>
    </SettingGroup>
    </div>
</template>

<style scoped>
.updates-section {
  border-radius: 12px;
}

.updates-section--highlight {
  outline: 2px solid rgba(var(--v-theme-success), 0.55);
  box-shadow: 0 0 0 6px rgba(var(--v-theme-success), 0.12);
  animation: updates-highlight-pulse 1.6s ease-in-out;
}

@keyframes updates-highlight-pulse {
  0% {
    box-shadow: 0 0 0 0 rgba(var(--v-theme-success), 0.0);
  }
  35% {
    box-shadow: 0 0 0 8px rgba(var(--v-theme-success), 0.18);
  }
  100% {
    box-shadow: 0 0 0 6px rgba(var(--v-theme-success), 0.12);
  }
}
</style>
