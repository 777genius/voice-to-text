<script setup lang="ts">
import { mdiBullseye, mdiLightningBolt, mdiSpeedometer, mdiTranslate } from '@mdi/js';

const { content } = useLandingContent();
const { t, locale } = useI18n();
const { data: releaseData } = useReleaseDownloads();
const { isDark } = useBrowserTheme();

const releaseVersion = computed(() => releaseData.value?.version || null);
const releaseDate = computed(() => {
  if (!releaseData.value?.pubDate) return '';
  return new Date(releaseData.value.pubDate).toLocaleDateString(locale.value, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
});

</script>

<template>
  <section
    id="hero"
    class="hero-section section anchor-offset"
    :class="isDark ? 'hero-section--dark' : 'hero-section--light'"
  >
    <ClientOnly>
      <LazyHeroCanvasBackground />
    </ClientOnly>

    <v-container class="hero-section__container">
      <v-row align="center" justify="space-between">
        <!-- Left: Text content -->
        <v-col cols="12" md="6" class="hero-section__content">
          <h1 class="hero-section__title">
            {{ content.hero.title }}
          </h1>

          <p class="hero-section__subtitle">
            {{ content.hero.subtitle }}
          </p>

          <div class="hero-section__actions">
            <DownloadButton />
            <v-btn
              variant="outlined"
              size="large"
              href="#features"
              class="hero-section__btn-secondary"
            >
              {{ t('nav.features') }}
            </v-btn>
          </div>

          <p v-if="releaseVersion" class="hero-section__release-info">
            v{{ releaseVersion }} · {{ releaseDate }}
          </p>

          <!-- Trust indicators -->
          <div class="hero-section__trust">
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon" :icon="mdiSpeedometer" />
              <span>{{ t("hero.trust.fast") }}</span>
            </div>
            <div class="hero-section__trust-divider" />
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon" :icon="mdiBullseye" />
              <span>{{ t("hero.trust.accurate") }}</span>
            </div>
            <div class="hero-section__trust-divider" />
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon" :icon="mdiLightningBolt" />
              <span>{{ t("hero.trust.realtime") }}</span>
            </div>
            <div class="hero-section__trust-divider" />
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon" :icon="mdiTranslate" />
              <span>{{ t("hero.trust.multilingual") }}</span>
            </div>
          </div>
        </v-col>

        <!-- Right: Interactive demo -->
        <v-col cols="12" md="5" class="hero-section__demo-col">
          <div class="hero-section__preview">
            <div class="hero-section__preview-glow" />
            <ClientOnly>
              <Suspense>
                <!-- Old full-window animation is paused while the hero mirrors the real mini recording window.
                <LazyHeroDemo />
                -->
                <LazyHeroMiniWindowDemo />
                <template #fallback>
                  <div class="hero-mini-demo-fallback">
                    <div class="hero-mini-demo-fallback__window" />
                  </div>
                </template>
              </Suspense>
              <template #fallback>
                <div class="hero-mini-demo-fallback">
                  <div class="hero-mini-demo-fallback__window" />
                </div>
              </template>
            </ClientOnly>
          </div>
        </v-col>
      </v-row>

    </v-container>
  </section>
</template>

<style scoped>
.hero-section {
  position: relative;
  min-height: 85vh;
  display: flex;
  align-items: center;
  isolation: isolate;
  overflow: hidden;
  background: #050816;
}

/* ─── Content ─── */
.hero-section__container {
  position: relative;
  z-index: 1;
}

.hero-section__content {
  animation: heroFadeIn 0.8s ease both;
}

/* ─── Badge ─── */
.hero-section__badge {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 6px 18px;
  border-radius: 100px;
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(99, 102, 241, 0.12), rgba(236, 72, 153, 0.12));
  color: #6366f1;
  margin-bottom: 24px;
  border: 1px solid rgba(99, 102, 241, 0.18);
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.1s;
}

.hero-section__badge-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #22c55e;
  box-shadow: 0 0 8px rgba(34, 197, 94, 0.6);
  animation: pulse 2s ease-in-out infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.6; transform: scale(0.85); }
}

/* ─── Title ─── */
.hero-section__title {
  font-size: 3.5rem;
  font-weight: 800;
  letter-spacing: -0.04em;
  line-height: 1.1;
  margin-bottom: 20px;
  background: linear-gradient(135deg, currentColor 0%, #6366f1 50%, #ec4899 100%);
  -webkit-background-clip: text;
  background-clip: text;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.2s;
}

/* ─── Subtitle ─── */
.hero-section__subtitle {
  font-size: 1.2rem;
  line-height: 1.7;
  opacity: 0.65;
  max-width: 640px;
  margin-bottom: 36px;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.3s;
}

/* ─── Actions ─── */
.hero-section__actions {
  --hero-action-width: clamp(238px, 24vw, 270px);
  --hero-action-gap: 14px;

  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: var(--hero-action-gap);
  align-items: stretch;
  width: min(100%, calc(var(--hero-action-width) + var(--hero-action-width) + var(--hero-action-gap)));
  margin-bottom: 40px;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.4s;
}

.hero-section__actions :deep(.v-btn) {
  width: 100%;
  min-width: 0;
  min-height: 64px !important;
  border-radius: 7px !important;
}

.hero-section__actions :deep(.v-btn__content) {
  width: 100%;
  white-space: normal;
}

.hero-section__btn-secondary {
  border-color: rgba(165, 180, 252, 0.34) !important;
  color: #c7d2fe !important;
  padding-inline: 12px !important;
  font-size: clamp(0.78rem, 1vw, 0.94rem) !important;
  font-weight: 600 !important;
  letter-spacing: 0.08em !important;
  background:
    linear-gradient(135deg, rgba(99, 102, 241, 0.08), rgba(74, 158, 255, 0.05)) !important;
  box-shadow:
    0 0 0 1px rgba(255, 255, 255, 0.03) inset,
    0 10px 30px rgba(15, 23, 42, 0.22);
  transition:
    transform 0.28s ease,
    border-color 0.28s ease,
    background 0.28s ease,
    box-shadow 0.28s ease !important;
}

.hero-section__btn-secondary :deep(.v-btn__content) {
  white-space: nowrap;
}

.hero-section__btn-secondary:hover {
  border-color: rgba(165, 180, 252, 0.55) !important;
  background:
    linear-gradient(135deg, rgba(99, 102, 241, 0.14), rgba(74, 158, 255, 0.1)) !important;
  box-shadow:
    0 0 0 1px rgba(255, 255, 255, 0.05) inset,
    0 14px 34px rgba(15, 23, 42, 0.28);
  transform: translateY(-1px);
}

/* ─── Release info ─── */
.hero-section__release-info {
  font-size: 0.78rem;
  font-weight: 500;
  opacity: 0.4;
  margin: -24px 0 32px;
  letter-spacing: 0.01em;
}

/* ─── Trust indicators ─── */
.hero-section__trust {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.5s;
}

.hero-section__trust-item {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 0.78rem;
  font-weight: 500;
  opacity: 0.55;
}

.hero-section__trust-icon {
  opacity: 0.7;
}

.hero-section__trust-divider {
  width: 1px;
  height: 16px;
  background: currentColor;
  opacity: 0.15;
}

/* ─── Preview Card ─── */
.hero-section__preview {
  position: relative;
  width: 100%;
  animation: heroSlideUp 0.9s ease both;
  animation-delay: 0.3s;
}

.hero-section__preview-glow {
  position: absolute;
  inset: -2px;
  border-radius: 22px;
  background: linear-gradient(135deg, rgba(99, 102, 241, 0.25), rgba(236, 72, 153, 0.25), rgba(139, 92, 246, 0.25));
  filter: blur(20px);
  opacity: 0.4;
  z-index: 0;
  animation: glowPulse 4s ease-in-out infinite;
}

@keyframes glowPulse {
  0%, 100% { opacity: 0.3; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(1.02); }
}

/* ─── SSR Fallback ─── */
.hero-mini-demo-fallback {
  position: relative;
  z-index: 1;
  min-height: 330px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.hero-mini-demo-fallback__window {
  width: 448px;
  max-width: 100%;
  aspect-ratio: 236 / 38;
  border-radius: 14px;
  background: rgba(26, 26, 26, 0.9);
  border: 1px solid rgba(255, 255, 255, 0.08);
  box-shadow: 0 24px 70px rgba(0, 0, 0, 0.42);
}

.hero-demo-fallback {
  border-radius: 16px;
  background: #1a1a1a;
  min-height: 330px;
}

@media (max-width: 600px) {
  .hero-demo-fallback {
    min-height: 280px;
  }
}

/* ─── Entrance animations ─── */
@keyframes heroFadeIn {
  from {
    opacity: 0;
    transform: translateY(20px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes heroSlideUp {
  from {
    opacity: 0;
    transform: translateY(40px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

/* ─── Dark Theme ─── */
.v-theme--dark .hero-section__badge {
  background: linear-gradient(135deg, rgba(129, 140, 248, 0.15), rgba(244, 114, 182, 0.15));
  color: #a5b4fc;
  border-color: rgba(129, 140, 248, 0.25);
}

.v-theme--dark .hero-section__title {
  background: linear-gradient(135deg, #f1f5f9 0%, #a5b4fc 50%, #f9a8d4 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .hero-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .hero-section__release-info {
  color: #64748b;
}

.v-theme--dark .hero-section__btn-secondary {
  border-color: rgba(165, 180, 252, 0.3) !important;
  color: #a5b4fc !important;
}

.v-theme--dark .hero-section__btn-secondary:hover {
  border-color: rgba(165, 180, 252, 0.5) !important;
  background: rgba(165, 180, 252, 0.08) !important;
}

.v-theme--dark .hero-section__trust-item {
  color: #94a3b8;
}

.v-theme--dark .hero-section__preview-glow {
  opacity: 0.25;
}

/* ─── Light Theme ─── */
.v-theme--light .hero-section,
.hero-section--light {
  background:
    radial-gradient(circle at 80% 18%, rgba(74, 158, 255, 0.14), transparent 34%),
    radial-gradient(circle at 16% 56%, rgba(99, 102, 241, 0.08), transparent 34%),
    linear-gradient(180deg, #f8fbff 0%, #ffffff 72%);
}

.v-theme--light .hero-section__badge,
.hero-section--light .hero-section__badge {
  color: #4f46e5;
}

.v-theme--light .hero-section__title,
.hero-section--light .hero-section__title {
  background: linear-gradient(135deg, #1e293b 0%, #4f46e5 50%, #db2777 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--light .hero-section__subtitle,
.hero-section--light .hero-section__subtitle {
  color: #475569;
}

.v-theme--light .hero-section__release-info,
.hero-section--light .hero-section__release-info {
  color: #94a3b8;
}

.v-theme--light .hero-section__trust-item,
.hero-section--light .hero-section__trust-item {
  color: #475569;
}

/* ─── Demo column: скрыта на мобильных через media query (SSR-safe) ─── */
.hero-section__demo-col {
  display: flex;
}

@media (max-width: 959px) {
  .hero-section__demo-col {
    display: none;
  }
}

/* ─── Responsive ─── */
@media (min-width: 961px) {
  .hero-section__subtitle {
    font-size: clamp(1.05rem, 1.16vw, 1.2rem);
    white-space: nowrap;
  }
}

@media (max-width: 960px) {
  .hero-section {
    min-height: auto;
    padding-top: 40px;
  }

  .hero-section__title {
    font-size: 2.4rem;
  }

  .hero-section__subtitle {
    font-size: 1.05rem;
  }

  .hero-section__trust {
    flex-wrap: wrap;
    gap: 12px;
  }

  .hero-section__preview {
    margin-top: 40px;
  }
}

@media (max-width: 600px) {
  .hero-section__title {
    font-size: 2rem;
  }

  .hero-section__subtitle {
    font-size: 0.95rem;
    margin-bottom: 28px;
  }

  .hero-section__actions {
    --hero-action-gap: 10px;

    margin-bottom: 28px;
  }

  .hero-section__btn-secondary {
    padding-inline: 8px !important;
    font-size: 0.72rem !important;
    letter-spacing: 0.04em !important;
  }

  .hero-section__trust {
    gap: 8px;
  }

  .hero-section__trust-divider {
    display: none;
  }

  .hero-section__trust-item {
    font-size: 0.75rem;
  }

  .hero-section__badge {
    font-size: 0.72rem;
    padding: 5px 14px;
  }
}
</style>
