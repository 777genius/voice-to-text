<script setup lang="ts">
/**
 * Секция настройки ключевых терминов для streaming STT.
 * Позволяет указать слова через запятую для улучшения распознавания
 * (бренды, аббревиатуры, техтермины и т.д.).
 */

import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

const { t } = useI18n();
const { streamingKeyterms } = useSettings();

const TOKEN_LIMIT = 500;

function countKeytermsTokens(input: string): number {
  return input
    .split(',')
    .map(t => t.trim())
    .filter(Boolean)
    .reduce((sum, term) => sum + term.split(/\s+/).length, 0);
}

const tokenCount = computed(() => countKeytermsTokens(streamingKeyterms.value));
const isOverLimit = computed(() => tokenCount.value > TOKEN_LIMIT);
</script>

<template>
  <SettingGroup :title="t('settings.keyterms.label')">
    <v-textarea
      v-model="streamingKeyterms"
      :placeholder="t('settings.keyterms.placeholder')"
      rows="2"
      auto-grow
      density="comfortable"
      hide-details
      variant="outlined"
    />

    <div class="d-flex justify-space-between align-center mt-1">
      <span class="text-caption text-medium-emphasis">
        {{ t('settings.keyterms.hint') }}
      </span>
      <span
        class="text-caption"
        :class="isOverLimit ? 'text-error' : 'text-medium-emphasis'"
      >
        {{ t('settings.keyterms.tokenCount', { count: tokenCount }) }}
      </span>
    </div>

    <div v-if="isOverLimit" class="text-caption text-error mt-1">
      {{ t('settings.keyterms.limitExceeded') }}
    </div>
  </SettingGroup>
</template>
