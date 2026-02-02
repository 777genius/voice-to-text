<script setup lang="ts">
import { ref, computed } from "vue";
import { screenshots } from "~/data/screenshots";

const { t } = useI18n();

// Local screenshot theme toggle (does NOT affect site theme)
const screenshotTheme = ref<"light" | "dark">("dark");
const isScreenshotDark = computed(() => screenshotTheme.value === "dark");

function toggleScreenshotTheme() {
  screenshotTheme.value = isScreenshotDark.value ? "light" : "dark";
}

// Slider state
const activeIndex = ref(0);
const totalSlides = screenshots.length;

function goTo(index: number) {
  activeIndex.value = index;
}

function prev() {
  activeIndex.value = (activeIndex.value - 1 + totalSlides) % totalSlides;
}

function next() {
  activeIndex.value = (activeIndex.value + 1) % totalSlides;
}

// Touch/swipe support
const touchStartX = ref(0);
const touchEndX = ref(0);

function onTouchStart(e: TouchEvent) {
  touchStartX.value = e.changedTouches[0].screenX;
}

function onTouchEnd(e: TouchEvent) {
  touchEndX.value = e.changedTouches[0].screenX;
  const diff = touchStartX.value - touchEndX.value;
  if (Math.abs(diff) > 50) {
    if (diff > 0) next();
    else prev();
  }
}
</script>

<template>
  <section id="screenshots" class="screenshots-section section anchor-offset">
    <!-- Background decoration -->
    <div class="screenshots-section__bg">
      <div class="screenshots-section__orb screenshots-section__orb--1" />
      <div class="screenshots-section__orb screenshots-section__orb--2" />
      <div class="screenshots-section__grid-pattern" />
    </div>

    <div class="screenshots-section__container">
      <!-- Header area -->
      <div class="screenshots-section__header">
        <span class="screenshots-section__badge">{{ t("nav.screenshots") }}</span>
        <h2 class="screenshots-section__title">
          {{ t("screenshots.sectionTitle") }}
        </h2>
        <p class="screenshots-section__subtitle">
          {{ t("screenshots.sectionSubtitle") }}
        </p>

        <!-- Controls row: toggle + navigation -->
        <div class="screenshots-section__controls">
          <!-- Screenshot theme toggle -->
          <div class="screenshots-section__toggle">
            <span
              class="screenshots-section__toggle-label"
              :class="{ 'screenshots-section__toggle-label--active': !isScreenshotDark }"
            >
              <v-icon size="18" icon="mdi-weather-sunny" />
              {{ t("screenshots.light") }}
            </span>
            <button
              class="screenshots-section__switch"
              :class="{ 'screenshots-section__switch--dark': isScreenshotDark }"
              role="switch"
              :aria-checked="isScreenshotDark"
              :aria-label="t('screenshots.toggleTheme')"
              @click="toggleScreenshotTheme"
            >
              <span class="screenshots-section__switch-thumb" />
            </button>
            <span
              class="screenshots-section__toggle-label"
              :class="{ 'screenshots-section__toggle-label--active': isScreenshotDark }"
            >
              <v-icon size="18" icon="mdi-weather-night" />
              {{ t("screenshots.dark") }}
            </span>
          </div>

          <!-- Navigation arrows (mobile/tablet only) -->
          <div class="screenshots-section__nav">
            <button class="screenshots-section__nav-btn" aria-label="Previous" @click="prev">
              <v-icon size="20" icon="mdi-chevron-left" />
            </button>
            <span class="screenshots-section__nav-count">
              {{ activeIndex + 1 }} / {{ totalSlides }}
            </span>
            <button class="screenshots-section__nav-btn" aria-label="Next" @click="next">
              <v-icon size="20" icon="mdi-chevron-right" />
            </button>
          </div>
        </div>
      </div>

      <!-- Screenshots grid / slider -->
      <div
        class="screenshots-section__gallery"
        @touchstart.passive="onTouchStart"
        @touchend.passive="onTouchEnd"
      >
        <div
          class="screenshots-section__track"
          :style="{ '--active-index': activeIndex }"
        >
          <div
            v-for="(shot, index) in screenshots"
            :key="shot.id"
            class="screenshots-section__slide"
            :class="{
              'screenshots-section__slide--active': activeIndex === index,
            }"
            @click="goTo(index)"
          >
            <div class="screenshots-section__card">
              <div class="screenshots-section__card-glow" />
              <div class="screenshots-section__card-inner">
                <div class="screenshots-section__card-header">
                  <div class="screenshots-section__card-dots">
                    <span /><span /><span />
                  </div>
                  <span class="screenshots-section__card-label">{{ t(shot.labelKey) }}</span>
                </div>
                <Transition name="screenshot-fade" mode="out-in">
                  <img
                    :key="`${shot.id}-${screenshotTheme}`"
                    class="screenshots-section__image"
                    :src="isScreenshotDark ? shot.darkSrc : shot.lightSrc"
                    :alt="t(shot.labelKey)"
                    :width="shot.width"
                    :height="shot.height"
                    loading="lazy"
                    decoding="async"
                  />
                </Transition>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- Dot indicators (mobile/tablet) -->
      <div class="screenshots-section__dots">
        <button
          v-for="(shot, index) in screenshots"
          :key="shot.id"
          class="screenshots-section__dot"
          :class="{ 'screenshots-section__dot--active': activeIndex === index }"
          :aria-label="`Go to slide ${index + 1}`"
          @click="goTo(index)"
        />
      </div>
    </div>
  </section>
</template>

<style scoped>
.screenshots-section {
  position: relative;
  overflow: hidden;
  padding-top: 48px !important;
  padding-bottom: 48px !important;
}

/* ─── Background ─── */
.screenshots-section__bg {
  position: absolute;
  inset: 0;
  pointer-events: none;
  overflow: hidden;
}

.screenshots-section__orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(120px);
  opacity: 0.07;
}

.screenshots-section__orb--1 {
  width: 550px;
  height: 550px;
  background: #f97316;
  top: -200px;
  left: -80px;
}

.screenshots-section__orb--2 {
  width: 450px;
  height: 450px;
  background: #06b6d4;
  bottom: -120px;
  right: -100px;
}

.screenshots-section__grid-pattern {
  position: absolute;
  inset: 0;
  background-image:
    linear-gradient(rgba(249, 115, 22, 0.03) 1px, transparent 1px),
    linear-gradient(90deg, rgba(249, 115, 22, 0.03) 1px, transparent 1px);
  background-size: 48px 48px;
  mask-image: radial-gradient(ellipse 70% 60% at 50% 40%, black, transparent);
}

/* ─── Container ─── */
.screenshots-section__container {
  position: relative;
  z-index: 1;
  max-width: 1400px;
  margin: 0 auto;
  padding: 0 clamp(16px, 4vw, 64px);
}

/* ─── Header ─── */
.screenshots-section__header {
  text-align: center;
  margin-bottom: 36px;
}

.screenshots-section__badge {
  display: inline-block;
  padding: 4px 14px;
  border-radius: 100px;
  font-size: 0.75rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(249, 115, 22, 0.15), rgba(6, 182, 212, 0.15));
  color: #f97316;
  margin-bottom: 12px;
  border: 1px solid rgba(249, 115, 22, 0.2);
}

.screenshots-section__title {
  font-size: 2rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 10px;
  background: linear-gradient(135deg, currentColor 0%, rgba(249, 115, 22, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.screenshots-section__subtitle {
  font-size: 0.95rem;
  opacity: 0.6;
  line-height: 1.5;
  margin: 0 auto 20px;
  max-width: 520px;
}

/* ─── Controls ─── */
.screenshots-section__controls {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
}

/* ─── Theme Toggle ─── */
.screenshots-section__toggle {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  padding: 6px 14px;
  border-radius: 100px;
  background: rgba(255, 255, 255, 0.5);
  backdrop-filter: blur(8px);
  border: 1px solid rgba(249, 115, 22, 0.12);
}

.screenshots-section__toggle-label {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font-size: 0.78rem;
  font-weight: 600;
  opacity: 0.4;
  transition: opacity 0.3s ease, color 0.3s ease;
  user-select: none;
}

.screenshots-section__toggle-label--active {
  opacity: 1;
  color: #f97316;
}

.screenshots-section__switch {
  position: relative;
  width: 40px;
  height: 22px;
  border-radius: 100px;
  border: none;
  background: linear-gradient(135deg, #fbbf24, #f97316);
  cursor: pointer;
  transition: background 0.3s ease;
  padding: 0;
}

.screenshots-section__switch--dark {
  background: linear-gradient(135deg, #6366f1, #3b82f6);
}

.screenshots-section__switch-thumb {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.15);
  transition: transform 0.3s cubic-bezier(0.4, 0, 0.2, 1);
}

.screenshots-section__switch--dark .screenshots-section__switch-thumb {
  transform: translateX(18px);
}

/* ─── Navigation (visible on mobile/tablet) ─── */
.screenshots-section__nav {
  display: none;
  align-items: center;
  gap: 12px;
}

.screenshots-section__nav-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  border-radius: 12px;
  border: 1px solid rgba(249, 115, 22, 0.15);
  background: rgba(255, 255, 255, 0.5);
  backdrop-filter: blur(8px);
  cursor: pointer;
  transition: background 0.2s ease, border-color 0.2s ease, transform 0.2s ease;
  color: inherit;
}

.screenshots-section__nav-btn:hover {
  background: rgba(249, 115, 22, 0.08);
  border-color: rgba(249, 115, 22, 0.3);
  transform: scale(1.05);
}

.screenshots-section__nav-count {
  font-size: 0.78rem;
  font-weight: 600;
  opacity: 0.5;
  font-variant-numeric: tabular-nums;
}

/* ─── Gallery (3-column grid on desktop) ─── */
.screenshots-section__gallery {
  overflow: hidden;
}

.screenshots-section__track {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 24px;
  transition: transform 0.5s cubic-bezier(0.4, 0, 0.2, 1);
  transform: none !important;
}

.screenshots-section__slide {
  min-width: 0;
  cursor: pointer;
  transition: transform 0.4s cubic-bezier(0.4, 0, 0.2, 1);
}

.screenshots-section__slide:hover {
  transform: translateY(-6px);
}

/* ─── Card ─── */
.screenshots-section__card {
  position: relative;
  border-radius: 16px;
  overflow: hidden;
  transition:
    transform 0.45s cubic-bezier(0.4, 0, 0.2, 1),
    box-shadow 0.45s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow:
    0 8px 32px rgba(249, 115, 22, 0.08),
    0 4px 16px rgba(0, 0, 0, 0.04);
}

.screenshots-section__slide:hover .screenshots-section__card {
  box-shadow:
    0 20px 60px rgba(249, 115, 22, 0.12),
    0 8px 32px rgba(0, 0, 0, 0.06);
}

.screenshots-section__card-glow {
  position: absolute;
  inset: 0;
  background: radial-gradient(
    ellipse 80% 40% at 50% 0%,
    rgba(249, 115, 22, 0.06),
    transparent 70%
  );
  pointer-events: none;
  z-index: 1;
}

.screenshots-section__card-inner {
  background: rgba(255, 255, 255, 0.6);
  border: 1px solid rgba(249, 115, 22, 0.1);
  border-radius: 16px;
  backdrop-filter: blur(16px);
  overflow: hidden;
}

/* ─── Card Header (window chrome) ─── */
.screenshots-section__card-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 14px;
  border-bottom: 1px solid rgba(249, 115, 22, 0.06);
  background: rgba(255, 255, 255, 0.4);
}

.screenshots-section__card-dots {
  display: flex;
  gap: 6px;
}

.screenshots-section__card-dots span {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.1);
}

.screenshots-section__card-dots span:nth-child(1) {
  background: #ff5f57;
}

.screenshots-section__card-dots span:nth-child(2) {
  background: #febc2e;
}

.screenshots-section__card-dots span:nth-child(3) {
  background: #28c840;
}

.screenshots-section__card-label {
  font-size: 0.72rem;
  font-weight: 600;
  letter-spacing: 0.03em;
  opacity: 0.5;
}

/* ─── Image ─── */
.screenshots-section__image {
  width: 100%;
  height: auto;
  object-fit: contain;
  display: block;
}

/* ─── Dot indicators (mobile/tablet) ─── */
.screenshots-section__dots {
  display: none;
  justify-content: center;
  gap: 8px;
  margin-top: 20px;
}

.screenshots-section__dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  border: none;
  background: rgba(249, 115, 22, 0.2);
  cursor: pointer;
  transition: all 0.3s ease;
  padding: 0;
}

.screenshots-section__dot--active {
  background: #f97316;
  width: 24px;
  border-radius: 100px;
}

/* ─── Screenshot Transition ─── */
.screenshot-fade-enter-active,
.screenshot-fade-leave-active {
  transition: opacity 0.35s ease;
}

.screenshot-fade-enter-from,
.screenshot-fade-leave-to {
  opacity: 0;
}

/* ─── Dark Theme (site theme) ─── */
.v-theme--dark .screenshots-section__orb {
  opacity: 0.12;
}

.v-theme--dark .screenshots-section__orb--1 {
  background: #fb923c;
}

.v-theme--dark .screenshots-section__orb--2 {
  background: #22d3ee;
}

.v-theme--dark .screenshots-section__grid-pattern {
  background-image:
    linear-gradient(rgba(251, 146, 60, 0.04) 1px, transparent 1px),
    linear-gradient(90deg, rgba(251, 146, 60, 0.04) 1px, transparent 1px);
}

.v-theme--dark .screenshots-section__badge {
  background: linear-gradient(135deg, rgba(251, 146, 60, 0.15), rgba(34, 211, 238, 0.15));
  color: #fdba74;
  border-color: rgba(251, 146, 60, 0.25);
}

.v-theme--dark .screenshots-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #fdba74 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .screenshots-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .screenshots-section__toggle {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(251, 146, 60, 0.1);
}

.v-theme--dark .screenshots-section__toggle-label--active {
  color: #fdba74;
}

.v-theme--dark .screenshots-section__nav-btn {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(251, 146, 60, 0.1);
}

.v-theme--dark .screenshots-section__nav-btn:hover {
  background: rgba(251, 146, 60, 0.1);
  border-color: rgba(251, 146, 60, 0.25);
}

.v-theme--dark .screenshots-section__card {
  box-shadow:
    0 8px 32px rgba(0, 0, 0, 0.3),
    0 0 0 1px rgba(251, 146, 60, 0.08);
}

.v-theme--dark .screenshots-section__slide:hover .screenshots-section__card {
  box-shadow:
    0 20px 60px rgba(0, 0, 0, 0.5),
    0 0 0 1px rgba(251, 146, 60, 0.15);
}

.v-theme--dark .screenshots-section__card-glow {
  background: radial-gradient(
    ellipse 80% 40% at 50% 0%,
    rgba(251, 146, 60, 0.08),
    transparent 70%
  );
}

.v-theme--dark .screenshots-section__card-inner {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(251, 146, 60, 0.08);
}

.v-theme--dark .screenshots-section__card-header {
  border-bottom-color: rgba(255, 255, 255, 0.06);
  background: rgba(255, 255, 255, 0.03);
}

.v-theme--dark .screenshots-section__card-dots span:nth-child(1) {
  background: #ff6b6b;
}

.v-theme--dark .screenshots-section__card-dots span:nth-child(2) {
  background: #ffd93d;
}

.v-theme--dark .screenshots-section__card-dots span:nth-child(3) {
  background: #6bcb77;
}

.v-theme--dark .screenshots-section__card-label {
  color: #94a3b8;
}

.v-theme--dark .screenshots-section__dot {
  background: rgba(251, 146, 60, 0.2);
}

.v-theme--dark .screenshots-section__dot--active {
  background: #fdba74;
}

/* ─── Light Theme ─── */
.v-theme--light .screenshots-section__orb {
  opacity: 0.05;
}

.v-theme--light .screenshots-section__badge {
  color: #ea580c;
}

.v-theme--light .screenshots-section__subtitle {
  color: #475569;
}

.v-theme--light .screenshots-section__card-inner {
  background: rgba(255, 255, 255, 0.85);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04), 0 4px 16px rgba(0, 0, 0, 0.02);
}

.v-theme--light .screenshots-section__card-header {
  background: rgba(249, 250, 251, 0.8);
}

.v-theme--light .screenshots-section__card-label {
  color: #64748b;
}

.v-theme--light .screenshots-section__toggle {
  background: rgba(255, 255, 255, 0.75);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.03);
}

.v-theme--light .screenshots-section__nav-btn {
  background: rgba(255, 255, 255, 0.7);
}

/* ─── Responsive: Tablet (2 columns + slider) ─── */
@media (max-width: 1024px) {
  .screenshots-section__track {
    display: flex;
    gap: 16px;
    /* Each slide is 50% - 8px; shift by (50% + 8px) per index */
    transform: translateX(calc(var(--active-index, 0) * (-50% - 8px))) !important;
  }

  .screenshots-section__slide {
    flex: 0 0 calc(50% - 8px);
    min-width: calc(50% - 8px);
  }

  .screenshots-section__slide:hover {
    transform: none;
  }

  .screenshots-section__nav {
    display: inline-flex;
  }

  .screenshots-section__dots {
    display: flex;
  }
}

/* ─── Responsive: Mobile (1 column + slider) ─── */
@media (max-width: 680px) {
  .screenshots-section {
    padding-top: 32px !important;
    padding-bottom: 32px !important;
  }

  .screenshots-section__header {
    margin-bottom: 24px;
  }

  .screenshots-section__title {
    font-size: 1.5rem;
  }

  .screenshots-section__subtitle {
    font-size: 0.85rem;
    margin-bottom: 16px;
  }

  .screenshots-section__track {
    display: flex;
    gap: 12px;
    transform: translateX(calc(var(--active-index, 0) * (-100% - 12px))) !important;
  }

  .screenshots-section__slide {
    flex: 0 0 100%;
    min-width: 100%;
  }

  .screenshots-section__card {
    border-radius: 14px;
  }

  .screenshots-section__card-inner {
    border-radius: 14px;
  }

  .screenshots-section__toggle {
    gap: 6px;
    padding: 4px 10px;
  }

  .screenshots-section__toggle-label {
    font-size: 0.72rem;
  }

  .screenshots-section__switch {
    width: 36px;
    height: 20px;
  }

  .screenshots-section__switch-thumb {
    width: 16px;
    height: 16px;
  }

  .screenshots-section__switch--dark .screenshots-section__switch-thumb {
    transform: translateX(16px);
  }

  .screenshots-section__card-header {
    padding: 6px 10px;
  }

  .screenshots-section__card-dots span {
    width: 6px;
    height: 6px;
  }

  .screenshots-section__nav-btn {
    width: 32px;
    height: 32px;
  }
}
</style>
