<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useUpdater } from '../../composables/useUpdater';
import { isTauriAvailable } from '../../utils/tauri';
import UpdateCard from './UpdateCard.vue';

const { t } = useI18n();
const { loadCachedAvailableUpdate } = useUpdater();

let unlistenOpened: UnlistenFn | null = null;
let isUnmounted = false;

async function refreshUpdateState() {
  await loadCachedAvailableUpdate();
}

async function hideWindow() {
  if (!isTauriAvailable()) return;

  try {
    await getCurrentWindow().hide();
  } catch (err) {
    console.error('Failed to hide update window:', err);
  }
}

onMounted(async () => {
  isUnmounted = false;
  await refreshUpdateState();
  if (isUnmounted) return;

  if (!isTauriAvailable()) return;

  try {
    const unlisten = await listen('update-window-opened', () => {
      void refreshUpdateState();
    });
    if (isUnmounted) {
      unlisten();
      return;
    }
    unlistenOpened = unlisten;
  } catch (err) {
    console.error('Failed to listen update window open event:', err);
  }
});

onUnmounted(() => {
  isUnmounted = true;
  if (unlistenOpened) {
    unlistenOpened();
    unlistenOpened = null;
  }
});
</script>

<template>
  <div class="update-window">
    <div class="update-window__header" data-tauri-drag-region>
      <div class="update-window__title">
        <v-icon color="success" size="20">mdi-download</v-icon>
        {{ t('settings.updates.dialogTitle') }}
      </div>
      <v-btn
        class="no-drag"
        icon="mdi-close"
        variant="text"
        size="small"
        @click="hideWindow"
      />
    </div>

    <div class="update-window__body">
      <UpdateCard :show-title="false" @later="hideWindow" />
    </div>
  </div>
</template>

<style scoped>
.update-window {
  width: 100%;
  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--glass-bg);
  border: 1px solid var(--glass-border);
  border-radius: var(--radius-xl);
  overflow: hidden;
}

:global(.theme-light) .update-window {
  background: rgba(255, 255, 255, 0.98);
}

.update-window__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 14px;
  border-bottom: 1px solid var(--glass-border);
}

.update-window__title {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  color: var(--color-text);
  font-size: 15px;
  font-weight: 600;
  white-space: nowrap;
}

.update-window__body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
}
</style>
