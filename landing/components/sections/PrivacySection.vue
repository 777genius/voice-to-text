<script setup lang="ts">
import { mdiSourceBranch, mdiAccountGroupOutline, mdiUpdate, mdiGithub, mdiOpenSourceInitiative, mdiCheckCircleOutline } from '@mdi/js';

const { content } = useLandingContent();
const { t, locale } = useI18n();
const { data: releaseData } = useReleaseDownloads();

const releaseVersion = computed(() => releaseData.value?.version || null);
const releaseDate = computed(() => {
  if (!releaseData.value?.pubDate) return '';
  return new Date(releaseData.value.pubDate).toLocaleDateString(locale.value, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });
});

const openSourceIcons = [
  mdiSourceBranch,
  mdiAccountGroupOutline,
  mdiUpdate,
];
</script>

<template>
  <section id="opensource" class="opensource-section section anchor-offset">
    <v-container>
      <div class="opensource-section__layout">
        <!-- Left: Header content -->
        <div class="opensource-section__header">
          <h2 class="opensource-section__title">{{ content.openSource.title }}</h2>
          <p class="opensource-section__note">{{ content.openSource.note }}</p>

          <p v-if="releaseVersion" class="opensource-section__release-info">
            v{{ releaseVersion }} · {{ releaseDate }}
          </p>

          <v-btn
            href="https://github.com/777genius/voice-to-text"
            target="_blank"
            rel="noopener"
            variant="flat"
            class="opensource-section__btn mt-6"
            :prepend-icon="mdiGithub"
          >
            {{ t('nav.viewOnGithub') }}
          </v-btn>

          <!-- Code visual -->
          <div class="opensource-section__visual">
            <div class="opensource-section__visual-inner">
              <v-icon size="48" class="opensource-section__visual-icon" :icon="mdiOpenSourceInitiative" />
            </div>
            <div class="opensource-section__visual-ring opensource-section__visual-ring--1" />
            <div class="opensource-section__visual-ring opensource-section__visual-ring--2" />
            <div class="opensource-section__visual-ring opensource-section__visual-ring--3" />
          </div>
        </div>

        <!-- Right: Open Source cards -->
        <div class="opensource-section__cards">
          <div
            v-for="(item, index) in content.openSource.bullets"
            :key="index"
            class="opensource-section__card"
            :style="{ '--delay': `${index * 0.12}s` }"
          >
            <div class="opensource-section__card-icon-wrap">
              <v-icon size="28" class="opensource-section__card-icon" :icon="openSourceIcons[index] || mdiCheckCircleOutline" />
            </div>
            <div class="opensource-section__card-content">
              <span class="opensource-section__card-number">0{{ index + 1 }}</span>
              <p class="opensource-section__card-text">{{ item }}</p>
            </div>
            <div class="opensource-section__card-glow" />
          </div>
        </div>
      </div>
    </v-container>
  </section>
</template>

<style scoped>
.opensource-section {
  position: relative;
}

/* --- Layout --- */
.opensource-section__layout {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 64px;
  align-items: center;
  position: relative;
  z-index: 1;
}

/* --- Header (Left) --- */
.opensource-section__header {
  position: relative;
}

.opensource-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 16px;
  background: linear-gradient(135deg, currentColor 0%, rgba(139, 92, 246, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.opensource-section__note {
  font-size: 1.05rem;
  opacity: 0.55;
  line-height: 1.7;
  margin: 0;
  max-width: 400px;
}

.opensource-section__release-info {
  font-size: 0.78rem;
  font-weight: 500;
  opacity: 0.4;
  margin-top: 12px;
  letter-spacing: 0.01em;
}

.opensource-section__btn {
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.9), rgba(99, 102, 241, 0.9)) !important;
  color: white !important;
  font-weight: 600;
  letter-spacing: 0.02em;
  padding: 0 24px;
  height: 44px;
  border-radius: 12px;
  transition: transform 0.2s ease, box-shadow 0.2s ease;
}

.opensource-section__btn:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 24px rgba(139, 92, 246, 0.3);
}

/* --- Visual --- */
.opensource-section__visual {
  position: relative;
  width: 140px;
  height: 140px;
  margin-top: 40px;
}

.opensource-section__visual-inner {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.12), rgba(99, 102, 241, 0.12));
  border: 1px solid rgba(139, 92, 246, 0.2);
  z-index: 2;
}

.opensource-section__visual-icon {
  color: #8b5cf6;
}

.opensource-section__visual-ring {
  position: absolute;
  border-radius: 50%;
  border: 1px solid rgba(139, 92, 246, 0.1);
  animation: pulseRing 3s ease-in-out infinite;
}

.opensource-section__visual-ring--1 {
  inset: -16px;
  animation-delay: 0s;
}

.opensource-section__visual-ring--2 {
  inset: -36px;
  animation-delay: 0.6s;
}

.opensource-section__visual-ring--3 {
  inset: -58px;
  animation-delay: 1.2s;
}

@keyframes pulseRing {
  0%, 100% {
    opacity: 0.3;
    transform: scale(1);
  }
  50% {
    opacity: 0.8;
    transform: scale(1.04);
  }
}

/* --- Cards (Right) --- */
.opensource-section__cards {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.opensource-section__card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 20px;
  padding: 24px 28px;
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.55);
  border: 1px solid rgba(139, 92, 246, 0.1);
  backdrop-filter: blur(12px);
  transition: transform 0.3s ease, box-shadow 0.3s ease, border-color 0.3s ease;
  overflow: hidden;
  animation: fadeSlideIn 0.5s ease both;
  animation-delay: var(--delay, 0s);
}

.opensource-section__card:hover {
  transform: translateY(-3px);
  border-color: rgba(139, 92, 246, 0.25);
  box-shadow: 0 12px 40px rgba(139, 92, 246, 0.08);
}

.opensource-section__card-glow {
  position: absolute;
  top: 0;
  left: 0;
  width: 100%;
  height: 100%;
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.04), transparent 60%);
  pointer-events: none;
  opacity: 0;
  transition: opacity 0.3s ease;
}

.opensource-section__card:hover .opensource-section__card-glow {
  opacity: 1;
}

.opensource-section__card-icon-wrap {
  flex-shrink: 0;
  width: 52px;
  height: 52px;
  border-radius: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.1), rgba(99, 102, 241, 0.1));
  border: 1px solid rgba(139, 92, 246, 0.15);
}

.opensource-section__card-icon {
  color: #8b5cf6;
}

.opensource-section__card-content {
  flex: 1;
  min-width: 0;
}

.opensource-section__card-number {
  display: block;
  font-size: 0.7rem;
  font-weight: 700;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: #8b5cf6;
  margin-bottom: 4px;
  opacity: 0.7;
}

.opensource-section__card-text {
  font-size: 1rem;
  font-weight: 500;
  line-height: 1.5;
  margin: 0;
  opacity: 0.85;
}

@keyframes fadeSlideIn {
  from {
    opacity: 0;
    transform: translateX(24px);
  }
  to {
    opacity: 1;
    transform: translateX(0);
  }
}

/* --- Dark Theme --- */
.v-theme--dark .opensource-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #c4b5fd 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .opensource-section__note {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .opensource-section__release-info {
  color: #64748b;
}

.v-theme--dark .opensource-section__visual-inner {
  background: linear-gradient(135deg, rgba(167, 139, 250, 0.12), rgba(129, 140, 248, 0.1));
  border-color: rgba(167, 139, 250, 0.2);
}

.v-theme--dark .opensource-section__visual-icon {
  color: #c4b5fd;
}

.v-theme--dark .opensource-section__visual-ring {
  border-color: rgba(167, 139, 250, 0.12);
}

.v-theme--dark .opensource-section__card {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(167, 139, 250, 0.08);
}

.v-theme--dark .opensource-section__card:hover {
  background: rgba(255, 255, 255, 0.06);
  border-color: rgba(167, 139, 250, 0.2);
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3), 0 0 0 1px rgba(167, 139, 250, 0.1);
}

.v-theme--dark .opensource-section__card-glow {
  background: linear-gradient(135deg, rgba(167, 139, 250, 0.06), transparent 60%);
}

.v-theme--dark .opensource-section__card-icon-wrap {
  background: linear-gradient(135deg, rgba(167, 139, 250, 0.1), rgba(129, 140, 248, 0.08));
  border-color: rgba(167, 139, 250, 0.15);
}

.v-theme--dark .opensource-section__card-icon {
  color: #c4b5fd;
}

.v-theme--dark .opensource-section__card-number {
  color: #c4b5fd;
}

.v-theme--dark .opensource-section__card-text {
  color: #e2e8f0;
  opacity: 0.9;
}

/* --- Light Theme --- */
.v-theme--light .opensource-section__card {
  background: rgba(255, 255, 255, 0.75);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04), 0 4px 16px rgba(0, 0, 0, 0.02);
}

.v-theme--light .opensource-section__card:hover {
  box-shadow: 0 12px 40px rgba(139, 92, 246, 0.1), 0 1px 3px rgba(0, 0, 0, 0.04);
}

.v-theme--light .opensource-section__card-text {
  color: #1e293b;
}

.v-theme--light .opensource-section__note {
  color: #475569;
}

.v-theme--light .opensource-section__release-info {
  color: #94a3b8;
}

/* --- Responsive --- */
@media (max-width: 960px) {
  .opensource-section__layout {
    grid-template-columns: 1fr;
    gap: 48px;
    text-align: center;
  }

  .opensource-section__header {
    display: flex;
    flex-direction: column;
    align-items: center;
  }

  .opensource-section__title {
    font-size: 1.85rem;
  }

  .opensource-section__note {
    max-width: 480px;
  }

  .opensource-section__visual {
    margin-top: 32px;
  }
}

@media (max-width: 600px) {
  .opensource-section__title {
    font-size: 1.6rem;
  }

  .opensource-section__card {
    padding: 18px 20px;
    gap: 16px;
  }

  .opensource-section__card-icon-wrap {
    width: 44px;
    height: 44px;
    border-radius: 12px;
  }

  .opensource-section__visual {
    width: 110px;
    height: 110px;
  }
}
</style>
