<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { useUpdateStore } from '../../stores/update';

defineEmits<{
  click: [];
}>();

const { t } = useI18n();
const updateStore = useUpdateStore();
</script>

<template>
  <v-chip
    v-if="updateStore.availableVersion"
    color="success"
    variant="flat"
    size="small"
    prepend-icon="mdi-arrow-up-circle"
    class="update-indicator no-drag"
    @click="$emit('click')"
  >
    {{ t('settings.updates.indicator') }}
  </v-chip>
</template>

<style scoped>
.update-indicator {
  cursor: pointer;
  font-weight: 500;
  transition: transform 0.2s ease, box-shadow 0.2s ease;
}

.update-indicator:hover {
  transform: translateY(-1px);
  box-shadow: 0 4px 12px rgba(76, 175, 80, 0.3);
}
</style>
