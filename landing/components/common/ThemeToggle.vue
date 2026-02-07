<script setup lang="ts">
const { t } = useI18n();
const { isDark, toggleTheme } = useBrowserTheme();
const { trackThemeToggle } = useAnalytics();

const tooltip = computed(() => isDark.value ? t('theme.light') : t('theme.dark'));

const onToggle = () => {
  toggleTheme();
  trackThemeToggle(isDark.value ? 'dark' : 'light');
};
</script>

<template>
  <v-tooltip :text="tooltip" location="bottom">
    <template #activator="{ props }">
      <v-btn
        v-bind="props"
        :icon="isDark ? 'mdi-weather-sunny' : 'mdi-weather-night'"
        variant="text"
        size="small"
        :aria-label="tooltip"
        @click="onToggle"
      />
    </template>
  </v-tooltip>
</template>
