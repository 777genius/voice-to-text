<script setup lang="ts">
import RecordingPopover from './presentation/components/RecordingPopover.vue';

// Простая поддержка перетаскивания мышью по всей карточке
async function onDragMouseDown(e: MouseEvent) {
  if (e.button !== 0) return;
  let el = e.target as HTMLElement | null;
  while (el && el !== (e.currentTarget as HTMLElement)) {
    if (el.classList && el.classList.contains('no-drag')) return;
    el = el.parentElement;
  }
  try {
    const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow');
    await getCurrentWebviewWindow().startDragging();
  } catch (err) {
    console.error('Failed to start dragging:', err);
  }
}
</script>

<template>
  <div class="app">
    <div class="window" data-tauri-drag-region @mousedown="onDragMouseDown">
      <RecordingPopover />
    </div>
  </div>
</template>

<style scoped>
.app {
  width: 100vw;
  height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
}

.window {
  background: var(--glass-bg);
  backdrop-filter: blur(var(--glass-blur));
  -webkit-backdrop-filter: blur(var(--glass-blur));
  border: 1px solid var(--glass-border);
  border-radius: var(--radius-xl);
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.45);
  width: 400px;
  min-width: 400px;
  max-width: 400px;
  box-sizing: border-box;
  animation: windowAppear 400ms cubic-bezier(0.34, 1.56, 0.64, 1) forwards;
  transform-origin: center center;
}

@keyframes windowAppear {
  0% {
    opacity: 0;
    transform: scale(0.5);
  }
  70% {
    opacity: 1;
    transform: scale(1.05);
  }
  100% {
    opacity: 1;
    transform: scale(1);
  }
}
</style>
