<script setup lang="ts">
const { isDark, toggleTheme } = useBrowserTheme();
</script>

<template>
  <v-btn variant="text" @click="toggleTheme">
    <v-icon :icon="isDark ? 'mdi-weather-sunny' : 'mdi-weather-night'" />
  </v-btn>
</template>
