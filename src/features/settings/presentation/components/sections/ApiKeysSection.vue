<script setup lang="ts">
/**
 * Секция ввода API ключей для облачных провайдеров
 */

import { ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { SttProviderType } from '@/types';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

const { t } = useI18n();
const { provider, deepgramApiKey, assemblyaiApiKey } = useSettings();

const showDeepgramKey = ref(false);
const showAssemblyAIKey = ref(false);
</script>

<template>
  <SettingGroup
    v-if="provider === SttProviderType.Deepgram || provider === SttProviderType.AssemblyAI"
    :title="t('settings.apiKeys.label')"
  >
    <!-- Deepgram API Key -->
    <div v-if="provider === SttProviderType.Deepgram" class="mb-2">
      <div class="text-caption text-medium-emphasis mb-1">
        {{ t('settings.apiKeys.deepgramLabel') }}
      </div>
      <v-text-field
        v-model="deepgramApiKey"
        :type="showDeepgramKey ? 'text' : 'password'"
        :placeholder="t('settings.apiKeys.placeholder')"
        density="comfortable"
        hide-details
        :append-inner-icon="showDeepgramKey ? 'mdi-eye-off' : 'mdi-eye'"
        @click:append-inner="showDeepgramKey = !showDeepgramKey"
      />
    </div>

    <!-- AssemblyAI API Key -->
    <div v-if="provider === SttProviderType.AssemblyAI" class="mb-2">
      <div class="text-caption text-medium-emphasis mb-1">
        {{ t('settings.apiKeys.assemblyLabel') }}
      </div>
      <v-text-field
        v-model="assemblyaiApiKey"
        :type="showAssemblyAIKey ? 'text' : 'password'"
        :placeholder="t('settings.apiKeys.placeholder')"
        density="comfortable"
        hide-details
        :append-inner-icon="showAssemblyAIKey ? 'mdi-eye-off' : 'mdi-eye'"
        @click:append-inner="showAssemblyAIKey = !showAssemblyAIKey"
      />
    </div>

    <template #hint>
      <div class="text-caption text-medium-emphasis mt-2">
        <p class="mb-1">{{ t('settings.apiKeys.hintLine1') }}</p>
        <p class="mb-0">{{ t('settings.apiKeys.hintLine2') }}</p>
      </div>
    </template>
  </SettingGroup>
</template>
