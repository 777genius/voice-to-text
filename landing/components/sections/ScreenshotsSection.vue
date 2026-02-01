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
</script>

<template>
  <section id="screenshots" class="screenshots-section section anchor-offset">
    <!-- Background decoration -->
    <div class="screenshots-section__bg">
      <div class="screenshots-section__orb screenshots-section__orb--1" />
      <div class="screenshots-section__orb screenshots-section__orb--2" />
      <div class="screenshots-section__grid-pattern" />
    </div>

    <div class="screenshots-section__layout">
      <!-- Left: text + toggle -->
      <div class="screenshots-section__content">
        <span class="screenshots-section__badge">{{ t("nav.screenshots") }}</span>
        <h2 class="screenshots-section__title">
          {{ t("screenshots.sectionTitle") }}
        </h2>
        <p class="screenshots-section__subtitle">
          {{ t("screenshots.sectionSubtitle") }}
        </p>

        <!-- Screenshot theme toggle (local, not global) -->
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

        <!-- Slide indicators / tabs -->
        <div class="screenshots-section__tabs">
          <button
            v-for="(shot, index) in screenshots"
            :key="shot.id"
            class="screenshots-section__tab"
            :class="{ 'screenshots-section__tab--active': activeIndex === index }"
            @click="goTo(index)"
          >
            <span class="screenshots-section__tab-dot" />
            <span class="screenshots-section__tab-label">{{ t(shot.labelKey) }}</span>
          </button>
        </div>

        <!-- Navigation arrows -->
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

      <!-- Right: screenshot slider extending to edge -->
      <div class="screenshots-section__slider-wrap">
        <div
          class="screenshots-section__slider-track"
          :style="{ transform: `translateX(-${activeIndex * 100}%)` }"
        >
          <div
            v-for="shot in screenshots"
            :key="shot.id"
            class="screenshots-section__slide"
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
    </div>
  </section>
</template>

<style scoped>
.screenshots-section {
  position: relative;
  overflow: hidden;
  padding-top: 32px !important;
  padding-bottom: 32px !important;
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

/* ─── Layout ─── */
.screenshots-section__layout {
  display: flex;
  align-items: center;
  gap: 24px;
  position: relative;
  z-index: 1;
  max-width: 1400px;
  margin: 0 auto;
  padding-left: clamp(16px, 4vw, 64px);
  /* No padding-right so slider extends to edge */
}

/* ─── Left Content ─── */
.screenshots-section__content {
  flex: 0 0 260px;
  max-width: 280px;
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
  margin-bottom: 10px;
  border: 1px solid rgba(249, 115, 22, 0.2);
}

.screenshots-section__title {
  font-size: 1.6rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 8px;
  background: linear-gradient(135deg, currentColor 0%, rgba(249, 115, 22, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.screenshots-section__subtitle {
  font-size: 0.85rem;
  opacity: 0.6;
  line-height: 1.5;
  margin: 0 0 14px;
}

/* ─── Theme Toggle ─── */
.screenshots-section__toggle {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  padding: 6px 12px;
  border-radius: 100px;
  background: rgba(255, 255, 255, 0.5);
  backdrop-filter: blur(8px);
  border: 1px solid rgba(249, 115, 22, 0.12);
  margin-bottom: 12px;
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

/* ─── Slide Tabs ─── */
.screenshots-section__tabs {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 10px;
}

.screenshots-section__tab {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 10px;
  border-radius: 8px;
  border: 1px solid transparent;
  background: rgba(255, 255, 255, 0.3);
  backdrop-filter: blur(8px);
  font-size: 0.8rem;
  font-weight: 600;
  color: inherit;
  opacity: 0.5;
  cursor: pointer;
  transition:
    opacity 0.3s ease,
    background 0.3s ease,
    border-color 0.3s ease,
    transform 0.2s ease;
  text-align: left;
}

.screenshots-section__tab:hover {
  opacity: 0.75;
  transform: translateX(4px);
}

.screenshots-section__tab--active {
  opacity: 1;
  background: linear-gradient(135deg, rgba(249, 115, 22, 0.1), rgba(6, 182, 212, 0.08));
  border-color: rgba(249, 115, 22, 0.25);
}

.screenshots-section__tab-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
  opacity: 0.3;
  transition: opacity 0.3s ease, background 0.3s ease;
  flex-shrink: 0;
}

.screenshots-section__tab--active .screenshots-section__tab-dot {
  opacity: 1;
  background: #f97316;
}

.screenshots-section__tab-label {
  letter-spacing: 0.02em;
}

/* ─── Navigation ─── */
.screenshots-section__nav {
  display: inline-flex;
  align-items: center;
  gap: 12px;
}

.screenshots-section__nav-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 34px;
  height: 34px;
  border-radius: 10px;
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

/* ─── Right Slider ─── */
.screenshots-section__slider-wrap {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  /* Extend to right edge */
  margin-right: calc(-1 * clamp(16px, 4vw, 64px));
}

.screenshots-section__slider-track {
  display: flex;
  transition: transform 0.5s cubic-bezier(0.4, 0, 0.2, 1);
}

.screenshots-section__slide {
  flex: 0 0 100%;
  min-width: 0;
  padding-right: 12px;
}

/* ─── Card ─── */
.screenshots-section__card {
  position: relative;
  border-radius: 14px 0 0 14px;
  overflow: hidden;
  transition:
    transform 0.45s cubic-bezier(0.4, 0, 0.2, 1),
    box-shadow 0.45s cubic-bezier(0.4, 0, 0.2, 1);
  box-shadow:
    0 24px 80px rgba(249, 115, 22, 0.1),
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
  border-right: none;
  border-radius: 14px 0 0 14px;
  backdrop-filter: blur(16px);
  overflow: hidden;
}

/* ─── Card Header (window chrome) ─── */
.screenshots-section__card-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
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
  max-height: 340px;
  object-fit: contain;
  display: block;
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

.v-theme--dark .screenshots-section__tab {
  background: rgba(255, 255, 255, 0.04);
  border-color: transparent;
}

.v-theme--dark .screenshots-section__tab--active {
  background: linear-gradient(135deg, rgba(251, 146, 60, 0.12), rgba(34, 211, 238, 0.08));
  border-color: rgba(251, 146, 60, 0.25);
}

.v-theme--dark .screenshots-section__tab--active .screenshots-section__tab-dot {
  background: #fdba74;
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
    0 24px 80px rgba(0, 0, 0, 0.4),
    0 0 0 1px rgba(251, 146, 60, 0.1);
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

.v-theme--light .screenshots-section__tab {
  background: rgba(255, 255, 255, 0.6);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
}

.v-theme--light .screenshots-section__nav-btn {
  background: rgba(255, 255, 255, 0.7);
}

/* ─── Responsive ─── */
@media (max-width: 960px) {
  .screenshots-section {
    padding-top: 24px !important;
    padding-bottom: 24px !important;
  }

  .screenshots-section__layout {
    flex-direction: column;
    padding-left: clamp(16px, 4vw, 32px);
    padding-right: clamp(16px, 4vw, 32px);
    gap: 16px;
  }

  .screenshots-section__content {
    flex: none;
    max-width: 100%;
    text-align: center;
  }

  .screenshots-section__tabs {
    flex-direction: row;
    justify-content: center;
    flex-wrap: wrap;
  }

  .screenshots-section__tab:hover {
    transform: translateY(-1px);
  }

  .screenshots-section__nav {
    justify-content: center;
    width: 100%;
  }

  .screenshots-section__title {
    font-size: 1.4rem;
  }

  .screenshots-section__subtitle {
    font-size: 0.82rem;
  }

  .screenshots-section__image {
    max-height: 280px;
  }

  .screenshots-section__slider-wrap {
    margin-right: calc(-1 * clamp(16px, 4vw, 32px));
    width: calc(100% + clamp(16px, 4vw, 32px));
  }

  .screenshots-section__card {
    border-radius: 14px 0 0 14px;
  }

  .screenshots-section__card-inner {
    border-radius: 14px 0 0 14px;
  }
}

@media (max-width: 600px) {
  .screenshots-section {
    padding-top: 20px !important;
    padding-bottom: 20px !important;
  }

  .screenshots-section__layout {
    gap: 12px;
  }

  .screenshots-section__title {
    font-size: 1.2rem;
  }

  .screenshots-section__subtitle {
    font-size: 0.78rem;
    margin-bottom: 10px;
  }

  .screenshots-section__image {
    max-height: 220px;
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

  .screenshots-section__tab {
    padding: 5px 8px;
    font-size: 0.72rem;
  }

  .screenshots-section__nav-btn {
    width: 30px;
    height: 30px;
  }
}
</style>
