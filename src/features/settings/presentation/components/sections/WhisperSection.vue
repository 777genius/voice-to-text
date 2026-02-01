<script setup lang="ts">
/**
 * Секция настроек Whisper: выбор модели + ModelManager
 */

import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import ModelManager from '@/presentation/components/ModelManager.vue';
import { useSettings } from '../../composables/useSettings';
import type { WhisperModelOption } from '../../../domain/types';

const { t } = useI18n();
const { whisperModel, isWhisperProvider } = useSettings();

const whisperModels = computed<WhisperModelOption[]>(() => [
  { value: 'tiny', label: t('settings.whisper.models.tiny') },
  { value: 'base', label: t('settings.whisper.models.base') },
  { value: 'small', label: t('settings.whisper.models.small') },
  { value: 'medium', label: t('settings.whisper.models.medium') },
  { value: 'large', label: t('settings.whisper.models.large') },
]);
</script>

<template>
  <template v-if="isWhisperProvider">
    <!-- Выбор модели -->
    <SettingGroup :title="t('settings.whisper.label')">
      <v-select
        v-model="whisperModel"
        :items="whisperModels"
        item-title="label"
        item-value="value"
        density="comfortable"
        hide-details
        prepend-inner-icon="mdi-brain"
      />

      <template #hint>
        <div class="text-caption text-medium-emphasis mt-2">
          <p class="mb-1">{{ t('settings.whisper.hintLine1') }}</p>
          <p class="mb-0">{{ t('settings.whisper.hintLine2') }}</p>
        </div>
      </template>
    </SettingGroup>

    <!-- Model Manager -->
    <SettingGroup>
      <ModelManager />
    </SettingGroup>
  </template>
</template>
