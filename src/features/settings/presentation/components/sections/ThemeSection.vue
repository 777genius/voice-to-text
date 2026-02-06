<script setup lang="ts">
/**
 * Секция выбора темы — тройной переключатель (Light / Dark / Auto)
 */

import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettingsTheme } from '../../composables/useSettingsTheme';

const { t } = useI18n();
const { currentTheme, setTheme, useSystemTheme } = useSettingsTheme({ persist: false });

type ThemeSegment = 'light' | 'dark' | 'auto';

const segmentValue = computed<ThemeSegment>(() => {
  if (useSystemTheme.value) return 'auto';
  return currentTheme.value === 'light' ? 'light' : 'dark';
});

function onSegmentChange(value: ThemeSegment): void {
  if (value === 'auto') {
    useSystemTheme.value = true;
  } else {
    useSystemTheme.value = false;
    setTheme(value);
  }
}
</script>

<template>
  <SettingGroup :title="t('settings.theme.label')">
    <v-btn-toggle
      class="theme-toggle"
      :model-value="segmentValue"
      mandatory
      density="compact"
      color="primary"
      @update:model-value="onSegmentChange"
    >
      <v-btn value="light" size="small">{{ t('settings.theme.light') }}</v-btn>
      <v-btn value="dark" size="small">{{ t('settings.theme.dark') }}</v-btn>
      <v-btn value="auto" size="small">{{ t('settings.theme.auto') }}</v-btn>
    </v-btn-toggle>
  </SettingGroup>
</template>

<style scoped>
.theme-toggle {
  border-radius: 9999px !important;
  overflow: hidden;
  border: 1px solid rgba(var(--v-border-color), var(--v-border-opacity));
}

.theme-toggle :deep(.v-btn) {
  border-radius: 0 !important;
  border: none !important;
}
</style>
