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
  <v-btn
    v-if="updateStore.availableVersion"
    color="success"
    variant="flat"
    size="x-small"
    :title="t('settings.updates.badgeAvailable')"
    class="update-indicator no-drag"
    @click="$emit('click')"
  >
    <v-icon size="14" class="update-indicator__icon">mdi-arrow-up-circle</v-icon>
    {{ t('settings.updates.indicator') }}
  </v-btn>
</template>

<style scoped>
.update-indicator {
  cursor: pointer;
  font-weight: 600;
  min-width: 0;
  padding-inline: 6px;
  letter-spacing: 0.2px;
  text-transform: none;
  font-size: 11px;
  min-height: 18px;
}

.update-indicator__icon {
  margin-right: 4px;
}
</style>
