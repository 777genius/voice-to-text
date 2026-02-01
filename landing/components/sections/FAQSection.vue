<script setup lang="ts">
import { ref } from 'vue'

const { content } = useLandingContent();
const { t } = useI18n();

const openPanels = ref<number[]>([]);

const faqIcons = [
  'mdi-monitor-cellphone',
  'mdi-microphone-outline',
  'mdi-keyboard-settings-outline',
  'mdi-shield-check-outline',
];
</script>

<template>
  <section id="faq" class="faq-section section anchor-offset">
    <!-- Background decoration -->
    <div class="faq-section__bg">
      <div class="faq-section__orb faq-section__orb--1" />
      <div class="faq-section__orb faq-section__orb--2" />
      <div class="faq-section__grid-pattern" />
    </div>

    <v-container>
      <!-- Header -->
      <div class="faq-section__header">
        <span class="faq-section__badge">{{ t('nav.faq') }}</span>
        <h2 class="faq-section__title">{{ t('faq.sectionTitle') }}</h2>
        <p class="faq-section__subtitle">{{ t('faq.subtitle') }}</p>
      </div>

      <!-- FAQ Items -->
      <div class="faq-section__content">
        <div class="faq-section__list">
          <v-expansion-panels
            v-model="openPanels"
            multiple
            variant="accordion"
            class="faq-section__panels"
          >
            <v-expansion-panel
              v-for="(item, index) in content.faq"
              :key="item.id"
              class="faq-section__panel"
              :style="{ '--delay': `${index * 0.08}s` }"
              elevation="0"
            >
              <v-expansion-panel-title class="faq-section__panel-title">
                <div class="faq-section__panel-header">
                  <div class="faq-section__panel-icon-wrap">
                    <v-icon size="22" class="faq-section__panel-icon">
                      {{ faqIcons[index] || 'mdi-help-circle-outline' }}
                    </v-icon>
                  </div>
                  <span class="faq-section__panel-question">{{ item.question }}</span>
                </div>
              </v-expansion-panel-title>
              <v-expansion-panel-text class="faq-section__panel-text">
                <div class="faq-section__answer" v-html="item.answer" />
              </v-expansion-panel-text>
            </v-expansion-panel>
          </v-expansion-panels>
        </div>

        <!-- Decorative side element -->
        <div class="faq-section__decoration">
          <div class="faq-section__deco-circle">
            <v-icon size="40" class="faq-section__deco-icon">mdi-frequently-asked-questions</v-icon>
          </div>
          <div class="faq-section__deco-ring faq-section__deco-ring--1" />
          <div class="faq-section__deco-ring faq-section__deco-ring--2" />
          <div class="faq-section__deco-ring faq-section__deco-ring--3" />
          <div class="faq-section__deco-dots">
            <span v-for="n in 5" :key="n" class="faq-section__deco-dot" :style="{ '--dot-delay': `${n * 0.3}s` }" />
          </div>
        </div>
      </div>
    </v-container>
  </section>
</template>

<style scoped>
.faq-section {
  position: relative;
  overflow: hidden;
}

/* ─── Background ─── */
.faq-section__bg {
  position: absolute;
  inset: 0;
  pointer-events: none;
  overflow: hidden;
}

.faq-section__orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(120px);
  opacity: 0.07;
}

.faq-section__orb--1 {
  width: 500px;
  height: 500px;
  background: #f59e0b;
  top: -150px;
  right: -80px;
}

.faq-section__orb--2 {
  width: 450px;
  height: 450px;
  background: #8b5cf6;
  bottom: -120px;
  left: -100px;
}

.faq-section__grid-pattern {
  position: absolute;
  inset: 0;
  background-image:
    linear-gradient(rgba(245, 158, 11, 0.03) 1px, transparent 1px),
    linear-gradient(90deg, rgba(245, 158, 11, 0.03) 1px, transparent 1px);
  background-size: 48px 48px;
  mask-image: radial-gradient(ellipse 60% 60% at 50% 50%, black, transparent);
}

/* ─── Header ─── */
.faq-section__header {
  text-align: center;
  max-width: 640px;
  margin: 0 auto 56px;
  position: relative;
  z-index: 1;
}

.faq-section__badge {
  display: inline-block;
  padding: 6px 18px;
  border-radius: 100px;
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(245, 158, 11, 0.15), rgba(139, 92, 246, 0.15));
  color: #f59e0b;
  margin-bottom: 20px;
  border: 1px solid rgba(245, 158, 11, 0.2);
}

.faq-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 16px;
  background: linear-gradient(135deg, currentColor 0%, rgba(245, 158, 11, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.faq-section__subtitle {
  font-size: 1.1rem;
  opacity: 0.6;
  line-height: 1.6;
  margin: 0;
}

/* ─── Content Layout ─── */
.faq-section__content {
  display: grid;
  grid-template-columns: 1fr 280px;
  gap: 56px;
  align-items: start;
  position: relative;
  z-index: 1;
}

/* ─── FAQ Panels ─── */
.faq-section__list {
  min-width: 0;
}

.faq-section__panels {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.faq-section__panel {
  border-radius: 16px !important;
  background: rgba(255, 255, 255, 0.55) !important;
  border: 1px solid rgba(245, 158, 11, 0.08) !important;
  backdrop-filter: blur(12px);
  transition: transform 0.3s ease, box-shadow 0.3s ease, border-color 0.3s ease;
  overflow: hidden;
  animation: faqFadeIn 0.5s ease both;
  animation-delay: var(--delay, 0s);
}

.faq-section__panel:hover {
  transform: translateY(-2px);
  border-color: rgba(245, 158, 11, 0.2) !important;
  box-shadow: 0 8px 32px rgba(245, 158, 11, 0.06);
}

.faq-section__panel::after {
  display: none;
}

:deep(.faq-section__panel .v-expansion-panel__shadow) {
  display: none;
}

.faq-section__panel-title {
  padding: 20px 24px !important;
  min-height: unset !important;
}

:deep(.faq-section__panel-title .v-expansion-panel-title__overlay) {
  opacity: 0 !important;
}

.faq-section__panel-header {
  display: flex;
  align-items: center;
  gap: 16px;
  width: 100%;
}

.faq-section__panel-icon-wrap {
  flex-shrink: 0;
  width: 42px;
  height: 42px;
  border-radius: 12px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, rgba(245, 158, 11, 0.1), rgba(139, 92, 246, 0.08));
  border: 1px solid rgba(245, 158, 11, 0.12);
  transition: background 0.3s ease;
}

.faq-section__panel:hover .faq-section__panel-icon-wrap {
  background: linear-gradient(135deg, rgba(245, 158, 11, 0.16), rgba(139, 92, 246, 0.12));
}

.faq-section__panel-icon {
  color: #f59e0b;
}

.faq-section__panel-question {
  font-size: 1rem;
  font-weight: 600;
  line-height: 1.4;
  opacity: 0.9;
}

:deep(.faq-section__panel-text .v-expansion-panel-text__wrapper) {
  padding: 0 24px 20px 82px !important;
}

.faq-section__answer {
  font-size: 0.95rem;
  line-height: 1.7;
  opacity: 0.65;
}

.faq-section__answer :deep(a) {
  color: #f59e0b;
  text-decoration: none;
  font-weight: 500;
}

.faq-section__answer :deep(a:hover) {
  text-decoration: underline;
}

/* ─── Decoration ─── */
.faq-section__decoration {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  height: 280px;
  align-self: center;
}

.faq-section__deco-circle {
  position: relative;
  width: 100px;
  height: 100px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, rgba(245, 158, 11, 0.12), rgba(139, 92, 246, 0.12));
  border: 1px solid rgba(245, 158, 11, 0.2);
  z-index: 2;
}

.faq-section__deco-icon {
  color: #f59e0b;
}

.faq-section__deco-ring {
  position: absolute;
  border-radius: 50%;
  border: 1px solid rgba(245, 158, 11, 0.1);
  animation: faqPulseRing 3.5s ease-in-out infinite;
  left: 50%;
  top: 50%;
  transform: translate(-50%, -50%);
}

.faq-section__deco-ring--1 {
  width: 148px;
  height: 148px;
  animation-delay: 0s;
}

.faq-section__deco-ring--2 {
  width: 200px;
  height: 200px;
  animation-delay: 0.7s;
}

.faq-section__deco-ring--3 {
  width: 256px;
  height: 256px;
  animation-delay: 1.4s;
}

.faq-section__deco-dots {
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.faq-section__deco-dot {
  position: absolute;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: rgba(245, 158, 11, 0.35);
  animation: faqDotFloat 4s ease-in-out infinite;
  animation-delay: var(--dot-delay, 0s);
}

.faq-section__deco-dot:nth-child(1) { top: 10%; left: 20%; }
.faq-section__deco-dot:nth-child(2) { top: 25%; right: 10%; }
.faq-section__deco-dot:nth-child(3) { bottom: 30%; left: 8%; }
.faq-section__deco-dot:nth-child(4) { bottom: 15%; right: 22%; }
.faq-section__deco-dot:nth-child(5) { top: 50%; right: 2%; }

@keyframes faqFadeIn {
  from {
    opacity: 0;
    transform: translateY(16px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes faqPulseRing {
  0%, 100% {
    opacity: 0.3;
    transform: translate(-50%, -50%) scale(1);
  }
  50% {
    opacity: 0.7;
    transform: translate(-50%, -50%) scale(1.05);
  }
}

@keyframes faqDotFloat {
  0%, 100% {
    opacity: 0.3;
    transform: translateY(0);
  }
  50% {
    opacity: 0.8;
    transform: translateY(-8px);
  }
}

/* ─── Dark Theme ─── */
.v-theme--dark .faq-section__orb {
  opacity: 0.12;
}

.v-theme--dark .faq-section__orb--1 {
  background: #fbbf24;
}

.v-theme--dark .faq-section__orb--2 {
  background: #a78bfa;
}

.v-theme--dark .faq-section__grid-pattern {
  background-image:
    linear-gradient(rgba(251, 191, 36, 0.04) 1px, transparent 1px),
    linear-gradient(90deg, rgba(251, 191, 36, 0.04) 1px, transparent 1px);
}

.v-theme--dark .faq-section__badge {
  background: linear-gradient(135deg, rgba(251, 191, 36, 0.15), rgba(167, 139, 250, 0.15));
  color: #fbbf24;
  border-color: rgba(251, 191, 36, 0.25);
}

.v-theme--dark .faq-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #fbbf24 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .faq-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .faq-section__panel {
  background: rgba(255, 255, 255, 0.04) !important;
  border-color: rgba(251, 191, 36, 0.08) !important;
}

.v-theme--dark .faq-section__panel:hover {
  background: rgba(255, 255, 255, 0.06) !important;
  border-color: rgba(251, 191, 36, 0.2) !important;
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3), 0 0 0 1px rgba(251, 191, 36, 0.08);
}

.v-theme--dark .faq-section__panel-icon-wrap {
  background: linear-gradient(135deg, rgba(251, 191, 36, 0.1), rgba(167, 139, 250, 0.08));
  border-color: rgba(251, 191, 36, 0.15);
}

.v-theme--dark .faq-section__panel:hover .faq-section__panel-icon-wrap {
  background: linear-gradient(135deg, rgba(251, 191, 36, 0.16), rgba(167, 139, 250, 0.12));
}

.v-theme--dark .faq-section__panel-icon {
  color: #fbbf24;
}

.v-theme--dark .faq-section__panel-question {
  color: #e2e8f0;
  opacity: 1;
}

.v-theme--dark .faq-section__answer {
  color: #94a3b8;
  opacity: 0.85;
}

.v-theme--dark .faq-section__deco-circle {
  background: linear-gradient(135deg, rgba(251, 191, 36, 0.12), rgba(167, 139, 250, 0.1));
  border-color: rgba(251, 191, 36, 0.2);
}

.v-theme--dark .faq-section__deco-icon {
  color: #fbbf24;
}

.v-theme--dark .faq-section__deco-ring {
  border-color: rgba(251, 191, 36, 0.1);
}

.v-theme--dark .faq-section__deco-dot {
  background: rgba(251, 191, 36, 0.4);
}

/* ─── Light Theme ─── */
.v-theme--light .faq-section__orb {
  opacity: 0.05;
}

.v-theme--light .faq-section__badge {
  color: #d97706;
}

.v-theme--light .faq-section__panel {
  background: rgba(255, 255, 255, 0.75) !important;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04), 0 4px 16px rgba(0, 0, 0, 0.02);
}

.v-theme--light .faq-section__panel:hover {
  box-shadow: 0 8px 32px rgba(245, 158, 11, 0.08), 0 1px 3px rgba(0, 0, 0, 0.04);
}

.v-theme--light .faq-section__panel-question {
  color: #1e293b;
}

.v-theme--light .faq-section__answer {
  color: #475569;
}

.v-theme--light .faq-section__deco-dot {
  background: rgba(245, 158, 11, 0.3);
}

/* ─── Responsive ─── */
@media (max-width: 960px) {
  .faq-section__header {
    margin-bottom: 40px;
  }

  .faq-section__title {
    font-size: 1.85rem;
  }

  .faq-section__subtitle {
    font-size: 1rem;
  }

  .faq-section__content {
    grid-template-columns: 1fr;
    gap: 40px;
  }

  .faq-section__decoration {
    display: none;
  }
}

@media (max-width: 600px) {
  .faq-section__header {
    margin-bottom: 32px;
  }

  .faq-section__title {
    font-size: 1.6rem;
  }

  .faq-section__panel-title {
    padding: 16px 18px !important;
  }

  .faq-section__panel-icon-wrap {
    width: 36px;
    height: 36px;
    border-radius: 10px;
  }

  .faq-section__panel-header {
    gap: 12px;
  }

  .faq-section__panel-question {
    font-size: 0.92rem;
  }

  :deep(.faq-section__panel-text .v-expansion-panel-text__wrapper) {
    padding: 0 18px 16px 66px !important;
  }

  .faq-section__answer {
    font-size: 0.9rem;
  }
}
</style>
