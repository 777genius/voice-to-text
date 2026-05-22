<script setup lang="ts">
usePageSeo("meta.homeTitle", "meta.homeDescription");
useTrackSections();
useRevealSections();
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
    <!-- Screenshots section is temporarily hidden while the visuals are refreshed.
    <SectionDivider :flip="true" />
    <LazyScreenshotsSection />
    -->
    <SectionDivider :flip="true" />
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
