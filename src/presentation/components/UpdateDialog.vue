<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useUpdater } from '../../composables/useUpdater';
import { renderMarkdownToSafeHtml } from '@/utils/markdown';

const props = defineProps<{
  modelValue: boolean;
}>();

const emit = defineEmits<{
  'update:modelValue': [value: boolean];
}>();

const { t } = useI18n();
const { store, installUpdate } = useUpdater();

const releaseNotesHtml = computed(() => {
  const notes = store.releaseNotes;
  if (!notes) return '';
  return renderMarkdownToSafeHtml(notes);
});

const isOpen = computed({
  get: () => props.modelValue,
  set: (value) => emit('update:modelValue', value),
});

// Закрытие диалога (позже)
function handleLater() {
  isOpen.value = false;
}

// Установка обновления
async function handleInstall() {
  await installUpdate();
}
</script>

<template>
  <v-dialog v-model="isOpen" max-width="400" persistent>
    <v-card class="update-dialog">
      <v-card-title class="d-flex align-center ga-2">
        <v-icon color="success" size="24">mdi-download</v-icon>
        {{ t('settings.updates.dialogTitle') }}
      </v-card-title>

      <v-card-text>
        <div class="version-info">
          <span class="version-label">v{{ store.availableVersion }}</span>
        </div>

        <div v-if="store.releaseNotes" class="release-notes" v-html="releaseNotesHtml">
        </div>

        <p class="update-hint">
          {{ t('settings.updates.availableSubtitle') }}
        </p>

        <div v-if="store.isInstalling" class="mt-3">
          <v-progress-linear
            v-if="store.downloadProgress !== null"
            :model-value="store.downloadProgress"
            height="6"
            rounded
            color="success"
          />
          <v-progress-linear
            v-else
            indeterminate
            height="6"
            rounded
            color="success"
          />

          <div
            v-if="store.downloadProgress !== null"
            class="text-caption text-medium-emphasis mt-1 text-center"
          >
            {{ store.downloadProgress }}%
          </div>
        </div>

        <v-alert
          v-if="store.error"
          type="error"
          variant="tonal"
          density="compact"
          class="mt-3"
        >
          {{ store.error }}
        </v-alert>
      </v-card-text>

      <v-card-actions class="justify-end pa-4 pt-0">
        <v-btn
          variant="text"
          :disabled="store.isInstalling"
          @click="handleLater"
        >
          {{ t('settings.updates.later') }}
        </v-btn>
        <v-btn
          color="success"
          variant="flat"
          :loading="store.isInstalling"
          @click="handleInstall"
        >
          {{ store.isInstalling ? t('settings.updates.installing') : t('settings.updates.update') }}
        </v-btn>
      </v-card-actions>
    </v-card>
  </v-dialog>
</template>

<style scoped>
.update-dialog {
  border-radius: 12px !important;
}

.version-info {
  margin-bottom: 12px;
}

.version-label {
  font-size: 20px;
  font-weight: 600;
  color: rgb(var(--v-theme-success));
}

.release-notes {
  padding: 12px;
  background: rgba(var(--v-theme-surface-variant), 0.5);
  border-radius: 8px;
  font-size: 14px;
  line-height: 1.5;
  margin-bottom: 12px;
  max-height: 200px;
  overflow-y: auto;
}

.release-notes :deep(.md-h3) {
  font-weight: 700;
  margin: 8px 0 6px;
}

.release-notes :deep(.md-p) {
  margin: 4px 0;
}

.release-notes :deep(.md-ul) {
  margin: 6px 0 10px;
  padding-left: 18px;
}

.release-notes :deep(.md-li) {
  margin: 2px 0;
}

.release-notes :deep(code) {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New',
    monospace;
  font-size: 0.92em;
  padding: 1px 5px;
  border-radius: 6px;
  background: rgba(var(--v-theme-on-surface), 0.08);
}

.release-notes :deep(a) {
  color: rgb(var(--v-theme-primary));
  text-decoration: none;
}

.release-notes :deep(a:hover) {
  text-decoration: underline;
}

.update-hint {
  font-size: 14px;
  color: rgba(var(--v-theme-on-surface), 0.7);
  margin: 0;
}
</style>
