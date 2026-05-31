<script setup lang="ts">
const { t } = useI18n();
const { label } = usePlatform();
const downloadStore = useDownloadStore();
const { ensureLoaded, resolve, data: releaseData, fallbackUrl } = useReleaseDownloads();
const { trackDownloadClick } = useAnalytics();
const isResolving = ref(false);

onMounted(() => {
  void downloadStore.init();
});

const currentDownload = computed(() => {
  if (downloadStore.os === "macos") {
    return resolve("macos", downloadStore.arch);
  }
  if (downloadStore.os === "windows") {
    return resolve("windows", "x64");
  }
  if (downloadStore.os === "linux") {
    return resolve("linux", "x64");
  }
  return null;
});

const href = computed(() => currentDownload.value?.url || "#download");

const scrollToDownloads = () => {
  document.querySelector("#download")?.scrollIntoView({ behavior: "smooth" });
};

const handleClick = async () => {
  await downloadStore.init();

  if (downloadStore.os === "unknown") {
    scrollToDownloads();
    return;
  }

  if (isResolving.value) return;
  isResolving.value = true;

  try {
    await ensureLoaded();
    const resolved = currentDownload.value;
    const targetUrl = resolved?.url || fallbackUrl;

    trackDownloadClick({
      os: downloadStore.os,
      arch: downloadStore.arch,
      version: resolved?.version || releaseData.value?.version,
      source: "hero",
    });

    window.location.href = targetUrl;
  } finally {
    isResolving.value = false;
  }
};
</script>

<template>
  <v-btn
    class="download-button"
    color="primary"
    size="large"
    :href="href"
    :loading="isResolving"
    @click.prevent="handleClick"
  >
    <span class="download-button__ribbon">Free</span>
    <span class="download-button__title">{{ t('nav.download') }}</span>
    <span class="download-button__platform">{{ label }}</span>
  </v-btn>
</template>

<style scoped>
.download-button {
  position: relative;
  min-height: 64px !important;
  padding-inline: 24px !important;
  overflow: hidden;
  border: 1px solid rgba(255, 255, 255, 0.18) !important;
  background:
    linear-gradient(135deg, #4f46e5 0%, #6d6af7 42%, #4a9eff 100%) !important;
  box-shadow:
    0 16px 36px rgba(79, 70, 229, 0.32),
    0 0 0 1px rgba(255, 255, 255, 0.08) inset,
    0 0 34px rgba(74, 158, 255, 0.22);
  isolation: isolate;
  transition:
    transform 0.28s ease,
    box-shadow 0.28s ease,
    filter 0.28s ease;
  animation: downloadButtonGlow 4.8s ease-in-out infinite;
}

.download-button::before {
  content: "";
  position: absolute;
  inset: -46% -28%;
  z-index: 0;
  background:
    radial-gradient(circle at 24% 28%, rgba(255, 255, 255, 0.36), transparent 26%),
    linear-gradient(110deg, transparent 28%, rgba(255, 255, 255, 0.52) 46%, transparent 64%);
  opacity: 0.72;
  transform: translateX(-82%) rotate(8deg);
  animation: downloadButtonShine 3.6s cubic-bezier(0.22, 0.72, 0.2, 1) infinite;
}

.download-button::after {
  content: "";
  position: absolute;
  inset: 1px;
  z-index: 0;
  border-radius: inherit;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.24), transparent 42%),
    radial-gradient(circle at 74% 18%, rgba(255, 255, 255, 0.25), transparent 30%);
  pointer-events: none;
}

.download-button:hover {
  filter: saturate(1.12) brightness(1.04);
  transform: translateY(-1px);
  box-shadow:
    0 20px 44px rgba(79, 70, 229, 0.4),
    0 0 0 1px rgba(255, 255, 255, 0.12) inset,
    0 0 46px rgba(74, 158, 255, 0.3);
}

.download-button :deep(.v-btn__content) {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 3px;
  line-height: 1.1;
  position: static;
}

.download-button__title {
  position: relative;
  z-index: 2;
  font-size: 0.92rem;
  font-weight: 700;
  letter-spacing: 0.12em;
  text-transform: uppercase;
}

.download-button__ribbon {
  position: absolute;
  top: 7px;
  right: -20px;
  z-index: 3;
  width: 76px;
  height: 17px;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  background: linear-gradient(135deg, #22c55e 0%, #86efac 100%);
  color: #052e16;
  font-size: 0.56rem;
  font-weight: 900;
  letter-spacing: 0.1em;
  line-height: 1;
  text-transform: uppercase;
  box-shadow:
    0 3px 8px rgba(5, 46, 22, 0.22),
    0 0 0 1px rgba(255, 255, 255, 0.34) inset;
  transform: rotate(35deg);
  pointer-events: none;
}

.download-button__ribbon::before {
  content: "";
  position: absolute;
  inset: 0;
  background: linear-gradient(90deg, transparent 18%, rgba(255, 255, 255, 0.56) 48%, transparent 78%);
  opacity: 0.5;
  transform: translateX(-110%);
  animation: downloadRibbonGlint 4.4s ease-in-out infinite;
}

.download-button__platform {
  position: relative;
  z-index: 2;
  font-size: 0.74rem;
  font-weight: 600;
  letter-spacing: 0;
  opacity: 0.88;
  text-transform: none;
}

@keyframes downloadButtonShine {
  0% {
    transform: translateX(-86%) rotate(8deg);
  }
  44%,
  100% {
    transform: translateX(86%) rotate(8deg);
  }
}

@keyframes downloadButtonGlow {
  0%,
  100% {
    box-shadow:
      0 16px 36px rgba(79, 70, 229, 0.32),
      0 0 0 1px rgba(255, 255, 255, 0.08) inset,
      0 0 34px rgba(74, 158, 255, 0.22);
  }
  50% {
    box-shadow:
      0 18px 42px rgba(79, 70, 229, 0.42),
      0 0 0 1px rgba(255, 255, 255, 0.13) inset,
      0 0 52px rgba(34, 197, 94, 0.18);
  }
}

@keyframes downloadRibbonGlint {
  0%,
  64% {
    transform: translateX(-110%);
  }
  82%,
  100% {
    transform: translateX(110%);
  }
}

@media (prefers-reduced-motion: reduce) {
  .download-button,
  .download-button::before,
  .download-button__ribbon::before {
    animation: none;
  }
}
</style>
