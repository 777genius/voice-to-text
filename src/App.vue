<script setup lang="ts">
import RecordingPopover from './presentation/components/RecordingPopover.vue';
</script>

<template>
  <div class="app">
    <RecordingPopover />
  </div>
</template>

<style scoped>
.app {
  width: 100vw;
  height: 100vh;
  display: block;
  margin: 0;
  padding: 0;
  border-radius: var(--radius-xl);
  overflow: hidden;
  background: transparent;
  position: relative;
}
</style>
