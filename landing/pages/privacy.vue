<script setup lang="ts">
const { content } = useLandingContent();

usePageSeo("meta.openSourceTitle", "meta.openSourceDescription");
</script>

<template>
  <v-container class="section">
    <h1 class="text-h4 section-title">{{ content.openSource.title }}</h1>
    <PrivacySection />
  </v-container>
</template>
