<script setup lang="ts">
/**
 * Секция выбора языка распознавания.
 * Полный список языков Deepgram Nova-3 (~45 языков).
 */

import { computed, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import FlagIcon from '@/presentation/components/FlagIcon.vue';
import { useSettings } from '../../composables/useSettings';
import { STT_LANGUAGES } from '@/i18n.locales';

const { t } = useI18n();
const { language, syncLocale } = useSettings();

interface SttLanguageOption {
  value: string;
  label: string;
}

const languageOptions = computed<SttLanguageOption[]>(() =>
  STT_LANGUAGES.map(code => ({
    value: code,
    label: t(`languages.${code}`),
  }))
);

const isMulti = computed(() => language.value === 'multi');

watch(language, () => {
  syncLocale({ persist: false });
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
        <FlagIcon :locale="(item?.raw as SttLanguageOption)?.value" :size="18" class="mr-2" />
        <span>{{ (item?.raw as SttLanguageOption)?.label }}</span>
      </template>

      <template #item="{ props, item }">
        <v-list-item v-bind="props">
          <template #prepend>
            <FlagIcon :locale="(item?.raw as SttLanguageOption)?.value" :size="18" class="mr-2" />
          </template>
        </v-list-item>
      </template>
    </v-autocomplete>

    <div v-if="isMulti" class="text-caption text-medium-emphasis mt-2">
      {{ t('settings.language.multiHint') }}
    </div>
  </SettingGroup>
</template>
