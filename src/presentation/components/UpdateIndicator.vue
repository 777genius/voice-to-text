<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { useUpdateStore } from '../../stores/update';

const props = withDefaults(defineProps<{
  compact?: boolean;
}>(), {
  compact: false,
});

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
    :icon="props.compact"
    :title="t('settings.updates.badgeAvailable')"
    :aria-label="t('settings.updates.badgeAvailable')"
    class="update-indicator no-drag"
    :class="{ 'update-indicator--compact': props.compact }"
    @click="$emit('click')"
  >
    <v-icon :size="props.compact ? 16 : 14" class="update-indicator__icon">mdi-update</v-icon>
    <span v-if="!props.compact" class="update-indicator__label">
      {{ t('settings.updates.indicator') }}
    </span>
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

.update-indicator--compact {
  width: 22px;
  height: 22px;
  min-width: 22px;
  min-height: 22px;
  padding: 0;
  border-radius: 999px;
}

.update-indicator__icon {
  margin-right: 4px;
}

.update-indicator--compact .update-indicator__icon {
  margin-right: 0;
}
</style>
