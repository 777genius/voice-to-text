<script setup lang="ts">
import logoImg from '~/assets/logo-128.webp';
</script>

<template>
  <NuxtLink to="/" class="app-logo">
    <img :src="logoImg" alt="VoicetextAI" class="app-logo__icon" width="48" height="48" />
    VoicetextAI
  </NuxtLink>
</template>

<style scoped>
.app-logo {
  font-weight: 700;
  text-decoration: none;
  color: inherit;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  line-height: 1;
  font-size: 18px;
  letter-spacing: 0.2px;
  align-self: center;
}

.app-logo__icon {
  width: 48px;
  height: 48px;
  border-radius: 6px;
  flex-shrink: 0;
  margin-right: 16px;
}
</style>
