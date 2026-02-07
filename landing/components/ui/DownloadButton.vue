<script setup lang="ts">
const { t } = useI18n();
const { label } = usePlatform();
const downloadStore = useDownloadStore();
const { resolveUrlOrFallback, data: releaseData } = useReleaseDownloads();
const { trackDownloadClick } = useAnalytics();

onMounted(() => downloadStore.init());

const href = computed(() => {
  // Если не удалось определить платформу — пусть ведёт в секцию скачивания (старое поведение).
  if (downloadStore.os === "unknown") return "#download";
  if (downloadStore.os === "macos") {
    return resolveUrlOrFallback("macos", downloadStore.arch);
  }
  if (downloadStore.os === "windows") {
    return resolveUrlOrFallback("windows", "x64");
  }
  if (downloadStore.os === "linux") {
    return resolveUrlOrFallback("linux", "x64");
  }
  return "#download";
});
</script>

<template>
  <v-btn color="primary" size="large" :href="href" @click="trackDownloadClick({ os: downloadStore.os, arch: downloadStore.arch, version: releaseData?.version, source: 'hero' })">
    {{ t('hero.ctaPrimary', { platform: label }) }}
  </v-btn>
</template>
