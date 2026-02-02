<script setup lang="ts">
/**
 * Секция выбора темы (dark/light)
 */

import { onMounted, onUnmounted, ref, computed } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettingsTheme } from '../../composables/useSettingsTheme';
import type { AppTheme } from '../../../domain/types';

const { t } = useI18n();
const { currentTheme, setTheme, useSystemTheme } = useSettingsTheme();

const systemThemeNow = ref<AppTheme>('dark');

function detectSystemTheme(): AppTheme {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return 'dark';
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

let mql: MediaQueryList | null = null;
let mqlListener: ((e: MediaQueryListEvent) => void) | null = null;

function startWatchSystemTheme(): void {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
  mql = window.matchMedia('(prefers-color-scheme: light)');
  systemThemeNow.value = detectSystemTheme();
  mqlListener = () => {
    systemThemeNow.value = detectSystemTheme();
  };
  if (typeof mql.addEventListener === 'function') {
    mql.addEventListener('change', mqlListener);
  } else {
    // eslint-disable-next-line deprecation/deprecation
    mql.addListener(mqlListener as any);
  }
}

function stopWatchSystemTheme(): void {
  if (!mql || !mqlListener) return;
  if (typeof mql.removeEventListener === 'function') {
    mql.removeEventListener('change', mqlListener);
  } else {
    // eslint-disable-next-line deprecation/deprecation
    mql.removeListener(mqlListener as any);
  }
  mql = null;
  mqlListener = null;
}

const systemThemeLabel = computed(() => {
  return systemThemeNow.value === 'light' ? t('settings.theme.light') : t('settings.theme.dark');
});

onMounted(() => {
  startWatchSystemTheme();
});

onUnmounted(() => {
  stopWatchSystemTheme();
});
</script>

<template>
  <SettingGroup :title="t('settings.theme.label')">
    <div class="theme-controls">
      <div class="theme-left">
        <v-switch
          data-testid="settings-theme-switch"
          :model-value="currentTheme === 'light'"
          color="primary"
          hide-details
          inset
          :disabled="useSystemTheme"
          :label="t('settings.theme.light')"
          @update:model-value="setTheme($event ? 'light' : 'dark')"
        />

        <div v-if="useSystemTheme" class="system-theme-hint text-caption text-medium-emphasis">
          {{ t('settings.theme.systemNow', { theme: systemThemeLabel }) }}
        </div>
      </div>

      <v-checkbox
        v-model="useSystemTheme"
        data-testid="settings-theme-use-system"
        color="primary"
        hide-details
        density="compact"
        :label="t('settings.theme.useSystem')"
      />
    </div>
  </SettingGroup>
</template>

<style scoped>
.theme-controls {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-md);
}

.theme-left {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.system-theme-hint {
  margin-left: 14px; /* визуально привязано к v-switch */
}
</style>
