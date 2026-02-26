<script setup lang="ts">
/**
 * Переиспользуемый контейнер для группы настроек
 * Обеспечивает единообразный стиль всех секций
 */

defineProps<{
  title?: string;
  hint?: string;
  sectionId?: string;
}>();
</script>

<template>
  <div class="setting-group" :data-settings-section="sectionId">
    <div v-if="title" class="setting-group__label text-body-1 font-weight-medium mb-2">
      {{ title }}
    </div>

    <slot />

    <div v-if="hint" class="setting-group__hint text-caption text-medium-emphasis mt-1">
      {{ hint }}
    </div>

    <slot name="hint" />
  </div>
</template>

<style scoped>
.setting-group {
  margin-bottom: var(--spacing-xl);
}

.setting-group__label {
  color: rgb(var(--v-theme-on-surface));
}

.setting-group__hint {
  line-height: 1.4;
}
</style>
