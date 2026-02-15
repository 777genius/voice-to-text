<script setup lang="ts">
usePageSeo("meta.homeTitle", "meta.homeDescription");
useTrackSections();
</script>

<template>
  <div class="page">
    <HeroSection />
    <LazyFeaturesSection />
    <LazyProvidersSection />
    <LazySupportedLanguages />
    <LazyScreenshotsSection />
    <LazyPricingSection />
    <LazyTestimonialsSection />
    <LazyDownloadSection />
    <LazyPrivacySection />
    <LazyFAQSection />
  </div>
</template>
