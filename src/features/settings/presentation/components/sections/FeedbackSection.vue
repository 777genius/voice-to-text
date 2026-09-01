<script setup lang="ts">
import { useI18n } from 'vue-i18n';

const { t } = useI18n();
const feedbackEmail = 'quantjumppro@gmail.com';
</script>

<template>
  <div class="feedback-contact text-caption text-medium-emphasis">
    <v-icon size="small" aria-hidden="true">mdi-email-outline</v-icon>
    <span>{{ t('settings.feedback.label') }}:</span>
    <a class="feedback-contact__email" :href="`mailto:${feedbackEmail}`">
      {{ feedbackEmail }}
    </a>
  </div>
</template>

<style scoped>
.feedback-contact {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: var(--spacing-xs, 4px);
  padding: var(--spacing-sm, 8px) 0 var(--spacing-xs, 4px);
}

.feedback-contact__email {
  color: rgb(var(--v-theme-primary));
  text-decoration: none;
  user-select: text;
}

.feedback-contact__email:hover {
  text-decoration: underline;
}
</style>
