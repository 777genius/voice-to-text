<script setup lang="ts">
usePageSeo("meta.homeTitle", "meta.homeDescription");
useTrackSections();
const { containerRef } = useParallaxSections();
</script>

<template>
  <div ref="containerRef" class="page">
    <PageBackground />
    <HeroSection />
    <SectionDivider />
    <LazyFeaturesSection />
    <SectionDivider :flip="true" />
    <LazyProvidersSection />
    <SectionDivider />
    <LazySupportedLanguages />
    <SectionDivider :flip="true" />
    <LazyScreenshotsSection />
    <SectionDivider />
    <LazyPricingSection />
    <SectionDivider :flip="true" />
    <LazyTestimonialsSection />
    <SectionDivider />
    <LazyDownloadSection />
    <SectionDivider :flip="true" />
    <LazyPrivacySection />
    <SectionDivider />
    <LazyFAQSection />
  </div>
</template>
