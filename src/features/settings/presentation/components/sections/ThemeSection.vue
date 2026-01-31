<script setup lang="ts">
/**
 * Секция выбора темы (dark/light)
 */

import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettingsTheme } from '../../composables/useSettingsTheme';

const { t } = useI18n();
const { currentTheme, setTheme } = useSettingsTheme();
</script>

<template>
  <SettingGroup :title="t('settings.theme.label')">
    <v-switch
      :model-value="currentTheme === 'light'"
      color="primary"
      hide-details
      inset
      :label="t('settings.theme.light')"
      @update:model-value="setTheme($event ? 'light' : 'dark')"
    />
  </SettingGroup>
</template>
