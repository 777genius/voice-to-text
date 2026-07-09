<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isTauriAvailable } from '../../utils/tauri';

interface ErrorDetailsPayload {
  summary?: string;
  details?: string;
}

const { t } = useI18n();

const summary = ref('');
const details = ref('');
let unlistenOpened: UnlistenFn | null = null;
let isUnmounted = false;

async function hideWindow() {
  if (!isTauriAvailable()) return;

  try {
    await getCurrentWindow().hide();
  } catch (err) {
    console.error('Failed to hide error details window:', err);
  }
}

function applyPayload(payload?: ErrorDetailsPayload | null) {
  summary.value = payload?.summary || t('main.errorGeneric');
  details.value = payload?.details || summary.value;
}

function onKeyDown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    void hideWindow();
  }
}

onMounted(async () => {
  isUnmounted = false;
  applyPayload();
  window.addEventListener('keydown', onKeyDown);

  if (!isTauriAvailable()) return;

  try {
    const unlisten = await listen<ErrorDetailsPayload>('error-details-window-opened', (event) => {
      applyPayload(event.payload);
    });
    if (isUnmounted) {
      unlisten();
      return;
    }
    unlistenOpened = unlisten;
  } catch (err) {
    console.error('Failed to listen error details window open event:', err);
  }
});

onUnmounted(() => {
  isUnmounted = true;
  window.removeEventListener('keydown', onKeyDown);

  if (unlistenOpened) {
    unlistenOpened();
    unlistenOpened = null;
  }
});
</script>

<template>
  <div class="error-details-window">
    <div class="error-details-window__header" data-tauri-drag-region>
      <div class="error-details-window__title">
        <v-icon color="error" size="20">mdi-alert-circle-outline</v-icon>
        {{ t('errors.detailsTitle') }}
      </div>
      <v-btn
        class="no-drag"
        icon="mdi-close"
        variant="text"
        size="small"
        @click="hideWindow"
      />
    </div>

    <div class="error-details-window__body">
      <section class="error-details-window__section">
        <div class="error-details-window__label">{{ t('errors.detailsSummary') }}</div>
        <div class="error-details-window__summary">{{ summary }}</div>
      </section>

      <section class="error-details-window__section error-details-window__section--full">
        <div class="error-details-window__label">{{ t('errors.detailsFullMessage') }}</div>
        <pre class="error-details-window__details">{{ details }}</pre>
      </section>
    </div>
  </div>
</template>

<style scoped>
.error-details-window {
  width: 100%;
  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--glass-bg);
  border: 1px solid var(--glass-border);
  border-radius: var(--radius-xl);
  overflow: hidden;
}

:global(.theme-light) .error-details-window {
  background: rgba(255, 255, 255, 0.98);
}

.error-details-window__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 14px;
  border-bottom: 1px solid var(--glass-border);
}

.error-details-window__title {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  color: var(--color-text);
  font-size: 15px;
  font-weight: 600;
  white-space: nowrap;
}

.error-details-window__body {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 14px;
  overflow: hidden;
}

.error-details-window__section {
  display: flex;
  flex-direction: column;
  gap: 6px;
  min-width: 0;
}

.error-details-window__section--full {
  flex: 1;
  min-height: 0;
}

.error-details-window__label {
  color: var(--color-text-secondary);
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0;
  text-transform: uppercase;
}

.error-details-window__summary {
  color: var(--color-error);
  font-size: 14px;
  line-height: 1.4;
  overflow-wrap: anywhere;
}

.error-details-window__details {
  flex: 1;
  min-height: 0;
  margin: 0;
  padding: 10px;
  overflow: auto;
  border: 1px solid rgba(244, 67, 54, 0.22);
  border-radius: var(--radius-sm);
  background: rgba(0, 0, 0, 0.18);
  color: var(--color-text);
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
  line-height: 1.45;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

:global(.theme-light) .error-details-window__details {
  background: rgba(0, 0, 0, 0.04);
}
</style>
