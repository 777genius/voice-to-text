<script setup lang="ts">
import type { Component } from "vue";
import { resolveComponent } from "vue";
import { sectionOrder } from "~/data/sections";

usePageSeo("meta.homeTitle", "meta.homeDescription");
useTrackSections();

const sectionComponents: Record<string, Component> = {
  hero: resolveComponent("HeroSection"),
  features: resolveComponent("FeaturesSection"),
  providers: resolveComponent("ProvidersSection"),
  languages: resolveComponent("SupportedLanguages"),
  screenshots: resolveComponent("ScreenshotsSection"),
  pricing: resolveComponent("PricingSection"),
  download: resolveComponent("DownloadSection"),
  opensource: resolveComponent("PrivacySection"),
  faq: resolveComponent("FAQSection")
};
</script>

<template>
  <div class="page">
    <component
      :is="sectionComponents[section]"
      v-for="section in sectionOrder"
      :key="section"
    />
  </div>
</template>
