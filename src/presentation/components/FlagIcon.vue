<script setup lang="ts">
/**
 * Иконка флага страны по коду языка.
 * Поддерживает как UI-локали, так и STT-языки.
 * Использует CDN flagcdn.com (SVG).
 */

import { computed } from 'vue';
import { getSttFlagUrl } from '@/i18n.locales';

const props = withDefaults(defineProps<{
  locale: string;
  size?: number;
}>(), {
  size: 18,
});

const src = computed(() => getSttFlagUrl(props.locale));
const imgSize = computed(() => `${props.size}px`);
</script>

<template>
  <img
    :src="src"
    :width="size"
    :height="Math.round(size * 0.75)"
    :alt="locale"
    class="flag-icon"
    loading="lazy"
  />
</template>

<style scoped>
.flag-icon {
  width: v-bind(imgSize);
  border-radius: 2px;
  object-fit: cover;
  vertical-align: middle;
  flex-shrink: 0;
}
</style>
