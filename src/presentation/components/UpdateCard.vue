<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useUpdater } from '../../composables/useUpdater';
import { renderMarkdownToSafeHtml } from '@/utils/markdown';
import { normalizeAppUpdateNotes } from '@/utils/updateNotes';

withDefaults(
  defineProps<{
    showTitle?: boolean;
  }>(),
  {
    showTitle: true,
  }
);

const emit = defineEmits<{
  later: [];
}>();

const { t } = useI18n();
const { store, installUpdate } = useUpdater();

const appReleaseNotes = computed(() => {
  const notes = store.releaseNotes;
  if (!notes) return '';
  return normalizeAppUpdateNotes(notes);
});

const releaseNotesHtml = computed(() => {
  if (!appReleaseNotes.value) return '';
  return renderMarkdownToSafeHtml(appReleaseNotes.value);
});

async function handleInstall() {
  await installUpdate();
}
</script>

<template>
  <div class="update-card">
    <div v-if="showTitle" class="update-card__title">
      <v-icon color="success" size="24">mdi-download</v-icon>
      {{ t('settings.updates.dialogTitle') }}
    </div>

    <div class="update-card__body">
      <div v-if="store.availableVersion" class="version-info">
        <span class="version-label">v{{ store.availableVersion }}</span>
      </div>

      <v-alert
        v-else
        type="info"
        variant="tonal"
        density="compact"
        class="mb-3"
      >
        {{ t('settings.updates.latest') }}
      </v-alert>

      <div v-if="appReleaseNotes" class="release-notes" v-html="releaseNotesHtml">
      </div>

      <p v-if="store.availableVersion" class="update-hint">
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
    </div>

    <div class="update-card__actions">
      <v-btn
        variant="text"
        :disabled="store.isInstalling"
        @click="emit('later')"
      >
        {{ t('settings.updates.later') }}
      </v-btn>
      <v-btn
        color="success"
        variant="flat"
        :loading="store.isInstalling"
        :disabled="!store.availableVersion"
        @click="handleInstall"
      >
        {{ store.isInstalling ? t('settings.updates.installing') : t('settings.updates.update') }}
      </v-btn>
    </div>
  </div>
</template>

<style scoped>
.update-card {
  width: 100%;
}

.update-card__title {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 16px 16px 8px;
  font-size: 1.25rem;
  font-weight: 500;
  line-height: 1.6;
}

.update-card__body {
  padding: 16px;
}

.update-card__actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 0 16px 16px;
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
  padding: 10px 12px;
  background: rgba(34, 197, 94, 0.05);
  border: 1px solid rgba(34, 197, 94, 0.16);
  border-radius: 8px;
  font-size: 14px;
  line-height: 1.5;
  margin-bottom: 12px;
  max-height: 200px;
  overflow-y: auto;
}

:global(.theme-light) .release-notes {
  background: rgba(22, 163, 74, 0.06);
  border-color: rgba(22, 163, 74, 0.18);
}

.release-notes :deep(.md-h3) {
  font-weight: 700;
  margin: 10px 0 6px;
}

.release-notes :deep(.md-h3:first-child) {
  margin-top: 0;
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
