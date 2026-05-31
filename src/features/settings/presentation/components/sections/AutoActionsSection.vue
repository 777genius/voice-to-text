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
  playCompletionSound,
  hideRecordingWindowOnHotkey,
  showMiniRecordingWindow,
  keepRecordingUntilManualStop,
  holdToRecord,
  hasAccessibilityPermission,
  isMacOS,
  requestAccessibilityPermission,
} = useSettings();
</script>

<template>
  <SettingGroup :title="t('settings.autoActions.label')">
    <div class="auto-action-option">
      <v-checkbox
        v-model="autoCopyToClipboard"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.copy') }}</span>
            <span class="auto-action-hint">{{ t('settings.autoActions.hintCopyBody') }}</span>
          </span>
        </template>
      </v-checkbox>
    </div>

    <div class="auto-action-option">
      <v-checkbox
        v-model="autoPasteText"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.paste') }}</span>
            <span class="auto-action-hint">
              {{ t('settings.autoActions.hintPasteBody') }}
              {{ t('settings.autoActions.hintPasteUnstablePlatforms') }}
              {{ isMacOS ? t('settings.autoActions.hintMacPermission') : '' }}
            </span>
          </span>
        </template>
      </v-checkbox>
    </div>

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

    <div class="auto-action-option">
      <v-checkbox
        v-model="holdToRecord"
        density="compact"
        hide-details
        color="primary"
        class="auto-action-checkbox"
      >
        <template #label>
          <span class="auto-action-copy">
            <span class="auto-action-label">{{ t('settings.autoActions.holdToRecord') }}</span>
            <span class="auto-action-hint">{{ t('settings.autoActions.hintHoldToRecordBody') }}</span>
          </span>
        </template>
      </v-checkbox>
    </div>

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
