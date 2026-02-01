<script setup lang="ts">
/**
 * Секция автоматических действий (auto-copy, auto-paste)
 */

import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

const { t } = useI18n();
const {
  autoCopyToClipboard,
  autoPasteText,
  hasAccessibilityPermission,
  isMacOS,
  requestAccessibilityPermission,
} = useSettings();
</script>

<template>
  <SettingGroup :title="t('settings.autoActions.label')">
    <v-checkbox
      v-model="autoCopyToClipboard"
      :label="t('settings.autoActions.copy')"
      density="comfortable"
      hide-details
      color="primary"
    />

    <v-checkbox
      v-model="autoPasteText"
      :label="t('settings.autoActions.paste')"
      density="comfortable"
      hide-details
      color="primary"
      class="mt-1"
    />

    <!-- Предупреждение о разрешении Accessibility для macOS -->
    <v-alert
      v-if="autoPasteText && !hasAccessibilityPermission && isMacOS"
      type="warning"
      variant="tonal"
      class="mt-3"
    >
      <div class="d-flex flex-column">
        <div class="font-weight-medium mb-1">
          {{ t('settings.autoActions.accessibilityTitle') }}
        </div>
        <div class="text-body-2 mb-2">
          {{ t('settings.autoActions.accessibilityBody') }}
        </div>
        <v-btn
          color="warning"
          variant="flat"
          size="small"
          class="align-self-start"
          @click="requestAccessibilityPermission"
        >
          {{ t('settings.autoActions.accessibilityButton') }}
        </v-btn>
      </div>
    </v-alert>

    <template #hint>
      <div class="text-caption text-medium-emphasis mt-2">
        <p class="mb-1">
          <strong>{{ t('settings.autoActions.hintCopyTitle') }}</strong>
          {{ t('settings.autoActions.hintCopyBody') }}
        </p>
        <p class="mb-0">
          <strong>{{ t('settings.autoActions.hintPasteTitle') }}</strong>
          {{ t('settings.autoActions.hintPasteBody') }}
          {{ isMacOS ? t('settings.autoActions.hintMacPermission') : '' }}
        </p>
      </div>
    </template>
  </SettingGroup>
</template>
