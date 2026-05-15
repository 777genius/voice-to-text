<script setup lang="ts">
import { computed } from 'vue';
import UpdateCard from './UpdateCard.vue';

const props = defineProps<{
  modelValue: boolean;
}>();

const emit = defineEmits<{
  'update:modelValue': [value: boolean];
}>();

const isOpen = computed({
  get: () => props.modelValue,
  set: (value) => emit('update:modelValue', value),
});

function handleLater() {
  isOpen.value = false;
}
</script>

<template>
  <v-dialog v-model="isOpen" max-width="400" persistent>
    <v-card class="update-dialog">
      <UpdateCard @later="handleLater" />
    </v-card>
  </v-dialog>
</template>

<style scoped>
.update-dialog {
  border-radius: 12px !important;
}
</style>
