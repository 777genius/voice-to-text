<script setup lang="ts">
import { providers } from "~/data/providers";

const { content } = useLandingContent();
const { t } = useI18n();

const items = computed(() =>
  providers.map((provider) => {
    const contentItem = content.value.providers.find((item) => item.id === provider.id);
    if (!contentItem) return null;
    return { ...contentItem, icon: provider.icon, accent: provider.accent };
  }).filter(Boolean)
);

const NOVA3_URL = "https://deepgram.com/learn/introducing-nova-3-speech-to-text-api";
</script>

<template>
  <section id="providers" class="providers-section section anchor-offset">
    <!-- Background decoration -->
    <div class="providers-section__bg">
      <div class="providers-section__orb providers-section__orb--1" />
      <div class="providers-section__orb providers-section__orb--2" />
      <div class="providers-section__mesh" />
    </div>

    <v-container>
      <!-- Section header -->
      <div class="providers-section__header">
        <span class="providers-section__badge">{{ t("nav.providers") }}</span>
        <h2 class="providers-section__title">
          {{ t("providers.sectionTitle") }}
        </h2>
        <p class="providers-section__subtitle">
          {{ t("providers.sectionSubtitle") }}
        </p>
      </div>

      <!-- Nova-3 feature cards — compact grid -->
      <div class="providers-section__grid">
        <div
          v-for="(item, index) in items"
          :key="item.id"
          class="providers-section__card"
          :style="{ '--accent': item.accent, '--delay': `${index * 0.08}s` }"
        >
          <!-- Glow effect -->
          <div class="providers-section__card-glow" />

          <!-- Icon -->
          <div class="providers-section__card-icon-wrap">
            <div class="providers-section__card-icon-bg" />
            <v-icon :icon="item.icon" size="24" class="providers-section__card-icon" />
          </div>

          <!-- Content -->
          <div class="providers-section__card-body">
            <h3 class="providers-section__card-name">{{ item.name }}</h3>
            <p class="providers-section__card-desc">{{ item.description }}</p>
          </div>

          <!-- Decorative bottom line -->
          <div class="providers-section__card-line" />
        </div>
      </div>

      <!-- Learn More button -->
      <div class="providers-section__action">
        <a
          :href="NOVA3_URL"
          target="_blank"
          rel="noopener noreferrer"
          class="providers-section__btn"
        >
          {{ t("providers.learnMoreBtn") }}
          <v-icon icon="mdi-open-in-new" size="16" class="providers-section__btn-icon" />
        </a>
      </div>
    </v-container>
  </section>
</template>

<style scoped>
.providers-section {
  position: relative;
  overflow: hidden;
}

/* ─── Background ─── */
.providers-section__bg {
  position: absolute;
  inset: 0;
  pointer-events: none;
  overflow: hidden;
}

.providers-section__orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(120px);
  opacity: 0.07;
}

.providers-section__orb--1 {
  width: 600px;
  height: 600px;
  background: #8b5cf6;
  top: -200px;
  left: 50%;
  transform: translateX(-50%);
}

.providers-section__orb--2 {
  width: 500px;
  height: 500px;
  background: #06b6d4;
  bottom: -180px;
  right: -100px;
}

.providers-section__mesh {
  position: absolute;
  inset: 0;
  background-image:
    radial-gradient(circle at 25% 25%, rgba(139, 92, 246, 0.03) 0%, transparent 50%),
    radial-gradient(circle at 75% 75%, rgba(6, 182, 212, 0.03) 0%, transparent 50%);
  mask-image: radial-gradient(ellipse 70% 60% at 50% 50%, black, transparent);
}

/* ─── Header ─── */
.providers-section__header {
  text-align: center;
  max-width: 640px;
  margin: 0 auto 48px;
  position: relative;
  z-index: 1;
}

.providers-section__badge {
  display: inline-block;
  padding: 6px 18px;
  border-radius: 100px;
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.15), rgba(6, 182, 212, 0.15));
  color: #8b5cf6;
  margin-bottom: 20px;
  border: 1px solid rgba(139, 92, 246, 0.2);
}

.providers-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 16px;
  background: linear-gradient(135deg, currentColor 0%, rgba(139, 92, 246, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.providers-section__subtitle {
  font-size: 1.1rem;
  opacity: 0.6;
  line-height: 1.6;
  margin: 0;
}

/* ─── Grid — compact 5-column on desktop, wraps on smaller ─── */
.providers-section__grid {
  display: grid;
  grid-template-columns: repeat(5, 1fr);
  gap: 16px;
  position: relative;
  z-index: 1;
}

/* ─── Card — compact ─── */
.providers-section__card {
  position: relative;
  overflow: hidden;
  padding: 20px 16px;
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.08);
  backdrop-filter: blur(16px);
  -webkit-backdrop-filter: blur(16px);
  transition: transform 0.4s cubic-bezier(0.4, 0, 0.2, 1),
    box-shadow 0.4s cubic-bezier(0.4, 0, 0.2, 1),
    border-color 0.4s ease;
  animation: providerFadeIn 0.6s ease both;
  animation-delay: var(--delay, 0s);
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
}

.providers-section__card:hover {
  transform: translateY(-6px);
  border-color: var(--accent);
  box-shadow:
    0 16px 32px -8px rgba(0, 0, 0, 0.2),
    0 0 0 1px var(--accent),
    0 0 60px -16px var(--accent);
}

/* Card glow */
.providers-section__card-glow {
  position: absolute;
  top: -50%;
  left: -50%;
  width: 200%;
  height: 200%;
  background: radial-gradient(
    circle at 50% 50%,
    var(--accent) 0%,
    transparent 60%
  );
  opacity: 0;
  transition: opacity 0.5s ease;
  pointer-events: none;
  filter: blur(60px);
}

.providers-section__card:hover .providers-section__card-glow {
  opacity: 0.06;
}

/* Icon — compact */
.providers-section__card-icon-wrap {
  position: relative;
  width: 48px;
  height: 48px;
  display: flex;
  align-items: center;
  justify-content: center;
  margin-bottom: 12px;
}

.providers-section__card-icon-bg {
  position: absolute;
  inset: 0;
  border-radius: 14px;
  background: var(--accent);
  opacity: 0.12;
  transition: opacity 0.4s ease, transform 0.4s ease, border-radius 0.4s ease;
}

.providers-section__card:hover .providers-section__card-icon-bg {
  opacity: 0.22;
  transform: scale(1.1) rotate(3deg);
  border-radius: 16px;
}

.providers-section__card-icon {
  position: relative;
  z-index: 1;
  color: var(--accent) !important;
  transition: transform 0.4s ease;
}

.providers-section__card:hover .providers-section__card-icon {
  transform: scale(1.1);
}

/* Body — compact */
.providers-section__card-body {
  position: relative;
  z-index: 1;
}

.providers-section__card-name {
  font-size: 0.95rem;
  font-weight: 700;
  letter-spacing: -0.01em;
  margin-bottom: 6px;
  line-height: 1.3;
}

.providers-section__card-desc {
  font-size: 0.8rem;
  line-height: 1.5;
  opacity: 0.65;
  margin: 0;
}

/* Decorative line */
.providers-section__card-line {
  position: absolute;
  bottom: 0;
  left: 0;
  width: 100%;
  height: 2px;
  background: linear-gradient(90deg, transparent, var(--accent), transparent);
  opacity: 0;
  transition: opacity 0.4s ease;
}

.providers-section__card:hover .providers-section__card-line {
  opacity: 0.6;
}

/* ─── Learn More Button ─── */
.providers-section__action {
  display: flex;
  justify-content: center;
  margin-top: 40px;
  position: relative;
  z-index: 1;
}

.providers-section__btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 12px 28px;
  border-radius: 12px;
  font-size: 0.95rem;
  font-weight: 600;
  letter-spacing: 0.01em;
  text-decoration: none;
  color: #fff;
  background: linear-gradient(135deg, #6366f1, #8b5cf6);
  border: 1px solid rgba(139, 92, 246, 0.3);
  transition: transform 0.3s ease, box-shadow 0.3s ease, background 0.3s ease;
  cursor: pointer;
}

.providers-section__btn:hover {
  transform: translateY(-2px);
  box-shadow:
    0 8px 24px -4px rgba(99, 102, 241, 0.4),
    0 0 0 1px rgba(139, 92, 246, 0.5);
  background: linear-gradient(135deg, #818cf8, #a78bfa);
}

.providers-section__btn-icon {
  opacity: 0.7;
  transition: opacity 0.3s ease;
}

.providers-section__btn:hover .providers-section__btn-icon {
  opacity: 1;
}

@keyframes providerFadeIn {
  from {
    opacity: 0;
    transform: translateY(20px) scale(0.97);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
}

/* ─── Dark Theme ─── */
.v-theme--dark .providers-section__orb {
  opacity: 0.12;
}

.v-theme--dark .providers-section__orb--1 {
  background: #a78bfa;
}

.v-theme--dark .providers-section__orb--2 {
  background: #22d3ee;
}

.v-theme--dark .providers-section__badge {
  background: linear-gradient(135deg, rgba(167, 139, 250, 0.15), rgba(34, 211, 238, 0.15));
  color: #c4b5fd;
  border-color: rgba(167, 139, 250, 0.25);
}

.v-theme--dark .providers-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #c4b5fd 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .providers-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .providers-section__card {
  background: rgba(30, 41, 59, 0.6);
  border-color: rgba(148, 163, 184, 0.1);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
}

.v-theme--dark .providers-section__card:hover {
  background: rgba(30, 41, 59, 0.8);
  box-shadow:
    0 16px 32px -8px rgba(0, 0, 0, 0.4),
    0 0 0 1px var(--accent),
    0 0 60px -16px var(--accent);
}

.v-theme--dark .providers-section__card:hover .providers-section__card-glow {
  opacity: 0.1;
}

.v-theme--dark .providers-section__card-name {
  color: #e2e8f0;
}

.v-theme--dark .providers-section__card-desc {
  color: #94a3b8;
  opacity: 0.85;
}

.v-theme--dark .providers-section__card-icon-bg {
  opacity: 0.18;
}

.v-theme--dark .providers-section__card:hover .providers-section__card-icon-bg {
  opacity: 0.3;
}

/* ─── Light Theme ─── */
.v-theme--light .providers-section__orb {
  opacity: 0.05;
}

.v-theme--light .providers-section__badge {
  color: #7c3aed;
}

.v-theme--light .providers-section__card {
  background: rgba(255, 255, 255, 0.7);
  border-color: rgba(0, 0, 0, 0.06);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04), 0 4px 16px rgba(0, 0, 0, 0.02);
}

.v-theme--light .providers-section__card:hover {
  background: rgba(255, 255, 255, 0.95);
  box-shadow:
    0 16px 32px -8px rgba(0, 0, 0, 0.1),
    0 0 0 1px var(--accent),
    0 0 60px -16px var(--accent);
}

.v-theme--light .providers-section__card:hover .providers-section__card-glow {
  opacity: 0.04;
}

.v-theme--light .providers-section__card-name {
  color: #1e293b;
}

.v-theme--light .providers-section__card-desc {
  color: #475569;
  opacity: 0.8;
}

.v-theme--light .providers-section__btn {
  box-shadow: 0 2px 8px rgba(99, 102, 241, 0.2);
}

.v-theme--light .providers-section__btn:hover {
  box-shadow:
    0 8px 24px -4px rgba(99, 102, 241, 0.3),
    0 0 0 1px rgba(139, 92, 246, 0.4);
}

/* ─── Responsive ─── */
@media (max-width: 1200px) {
  .providers-section__grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (max-width: 960px) {
  .providers-section__grid {
    grid-template-columns: repeat(2, 1fr);
    max-width: 560px;
    margin: 0 auto;
  }

  .providers-section__title {
    font-size: 1.85rem;
  }

  .providers-section__header {
    margin-bottom: 36px;
  }

  .providers-section__subtitle {
    font-size: 1rem;
  }
}

@media (max-width: 600px) {
  .providers-section__grid {
    grid-template-columns: 1fr;
    max-width: 400px;
  }

  .providers-section__card {
    flex-direction: row;
    text-align: left;
    align-items: center;
    padding: 16px 18px;
    gap: 14px;
  }

  .providers-section__card-icon-wrap {
    margin-bottom: 0;
    flex-shrink: 0;
  }

  .providers-section__title {
    font-size: 1.6rem;
  }

  .providers-section__header {
    margin-bottom: 28px;
  }

  .providers-section__action {
    margin-top: 32px;
  }

  .providers-section__btn {
    width: 100%;
    justify-content: center;
    padding: 14px 24px;
  }
}
</style>
