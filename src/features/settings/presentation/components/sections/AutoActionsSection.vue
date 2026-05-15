<script setup lang="ts">
/**
 * Секция автоматических действий записи.
 */

import { useI18n } from 'vue-i18n';
import SettingGroup from '../shared/SettingGroup.vue';
import { useSettings } from '../../composables/useSettings';

const { t } = useI18n();
const {
  playCompletionSound,
  hideRecordingWindowOnHotkey,
  showMiniRecordingWindow,
  keepRecordingUntilManualStop,
} = useSettings();
</script>

<template>
  <SettingGroup :title="t('settings.autoActions.label')">
    <div class="auto-action-option">
      <v-checkbox
        v-model="playCompletionSound"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.completionSound') }}</span>
            <span class="auto-action-hint">{{ t('settings.autoActions.hintSoundBody') }}</span>
          </span>
        </template>
      </v-checkbox>
    </div>

    <div class="auto-action-option">
      <v-checkbox
        v-model="hideRecordingWindowOnHotkey"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.hideWindowOnHotkey') }}</span>
            <span class="auto-action-hint">{{ t('settings.autoActions.hintHotkeyWindowBody') }}</span>
          </span>
        </template>
      </v-checkbox>
    </div>

    <div class="auto-action-option">
      <v-checkbox
        v-model="showMiniRecordingWindow"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.showMiniWindow') }}</span>
            <span class="auto-action-hint">{{ t('settings.autoActions.hintWindowPositionBody') }}</span>
          </span>
        </template>
      </v-checkbox>
    </div>

    <div class="auto-action-option">
      <v-checkbox
        v-model="keepRecordingUntilManualStop"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.manualStopOnly') }}</span>
            <span class="auto-action-hint">{{ t('settings.autoActions.hintManualStopBody') }}</span>
          </span>
        </template>
      </v-checkbox>
    </div>
  </SettingGroup>
</template>

<style scoped>
.auto-action-option {
  margin-top: 8px;
}

.auto-action-option:first-child {
  margin-top: 0;
}

.auto-action-checkbox {
  min-height: 32px;
}

.auto-action-checkbox :deep(.v-selection-control) {
  align-items: flex-start;
  min-height: 32px;
}

.auto-action-checkbox :deep(.v-selection-control__wrapper) {
  margin-top: 0;
}

.auto-action-checkbox :deep(.v-label) {
  align-items: flex-start;
  opacity: 1;
}

.auto-action-copy {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding-top: 1px;
}

.auto-action-label {
  color: rgb(var(--v-theme-on-surface));
  line-height: 1.16;
}

.auto-action-hint {
  color: rgba(var(--v-theme-on-surface), 0.68);
  font-size: 11.5px;
  line-height: 1.22;
}
</style>
