<script setup lang="ts">
/**
 * Секция выбора языка распознавания
 */

import { computed, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';
import type { LanguageOption } from '../../../domain/types';

const { t } = useI18n();
const { language, syncLocale } = useSettings();

type UiLanguageOption = LanguageOption & {
  flag: string;
};

const FLAGS: Record<string, string> = {
  en: '🇺🇸',
  ru: '🇷🇺',
  uk: '🇺🇦',
  es: '🇪🇸',
  fr: '🇫🇷',
  de: '🇩🇪',
};

const languageOptions = computed<UiLanguageOption[]>(() => [
  { value: 'en', label: t('languages.en'), flag: FLAGS.en },
  { value: 'ru', label: t('languages.ru'), flag: FLAGS.ru },
  { value: 'uk', label: t('languages.uk'), flag: FLAGS.uk },
  { value: 'es', label: t('languages.es'), flag: FLAGS.es },
  { value: 'fr', label: t('languages.fr'), flag: FLAGS.fr },
  { value: 'de', label: t('languages.de'), flag: FLAGS.de },
]);

// Синхронизируем UI локаль при изменении языка
watch(language, () => {
  syncLocale();
});
</script>

<template>
  <SettingGroup :title="t('settings.language.label')">
    <v-autocomplete
      data-testid="settings-language-autocomplete"
      v-model="language"
      :items="languageOptions"
      item-title="label"
      item-value="value"
      density="comfortable"
      hide-details
      :placeholder="t('settings.language.searchPlaceholder')"
      auto-select-first="exact"
      :clearable="false"
    >
      <template #selection="{ item }">
        <span class="mr-2">{{ (item?.raw as UiLanguageOption)?.flag }}</span>
        <span>{{ (item?.raw as UiLanguageOption)?.label }}</span>
      </template>

      <template #item="{ props, item }">
        <v-list-item v-bind="props">
          <template #prepend>
            <span class="mr-2">{{ (item?.raw as UiLanguageOption)?.flag }}</span>
          </template>
          <v-list-item-title>{{ (item?.raw as UiLanguageOption)?.label }}</v-list-item-title>
        </v-list-item>
      </template>
    </v-autocomplete>
  </SettingGroup>
</template>
