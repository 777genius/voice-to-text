<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { screenshots } from "~/data/screenshots";

const { t } = useI18n();

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

// Lightbox state
const lightboxOpen = ref(false);
const lightboxIndex = ref(0);

function openLightbox(index: number) {
  lightboxIndex.value = index;
  lightboxOpen.value = true;
  document.body.style.overflow = "hidden";
}

function closeLightbox() {
  lightboxOpen.value = false;
  document.body.style.overflow = "";
}

function lightboxPrev() {
  lightboxIndex.value = (lightboxIndex.value - 1 + totalSlides) % totalSlides;
}

function lightboxNext() {
  lightboxIndex.value = (lightboxIndex.value + 1) % totalSlides;
}

const currentLightboxScreenshot = computed(() => screenshots[lightboxIndex.value]);

// Keyboard navigation for lightbox
function handleKeydown(e: KeyboardEvent) {
  if (!lightboxOpen.value) return;
  if (e.key === "Escape") closeLightbox();
  if (e.key === "ArrowLeft") lightboxPrev();
  if (e.key === "ArrowRight") lightboxNext();
}

onMounted(() => {
  window.addEventListener("keydown", handleKeydown);
});

onUnmounted(() => {
  window.removeEventListener("keydown", handleKeydown);
  document.body.style.overflow = "";
});
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
            @click="openLightbox(index)"
          >
            <div class="screenshots-section__image-wrapper">
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
              <!-- Zoom icon overlay -->
              <div class="screenshots-section__zoom-overlay">
                <v-icon class="screenshots-section__zoom-icon" icon="mdi-magnify-plus-outline" />
              </div>
            </div>
            <span class="screenshots-section__caption">{{ t(shot.labelKey) }}</span>
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

    <!-- Lightbox Modal -->
    <Teleport to="body">
      <Transition name="lightbox-fade">
        <div
          v-if="lightboxOpen"
          class="lightbox"
          @click.self="closeLightbox"
        >
          <div class="lightbox__content">
            <!-- Close button -->
            <button
              class="lightbox__close"
              aria-label="Close lightbox"
              @click="closeLightbox"
            >
              <v-icon size="28" icon="mdi-close" />
            </button>

            <!-- Navigation arrows -->
            <button
              class="lightbox__nav lightbox__nav--prev"
              aria-label="Previous screenshot"
              @click="lightboxPrev"
            >
              <v-icon size="32" icon="mdi-chevron-left" />
            </button>
            <button
              class="lightbox__nav lightbox__nav--next"
              aria-label="Next screenshot"
              @click="lightboxNext"
            >
              <v-icon size="32" icon="mdi-chevron-right" />
            </button>

            <!-- Image container -->
            <div class="lightbox__image-wrapper">
              <!-- Theme toggle in lightbox -->
              <div class="lightbox__toggle">
                <span
                  class="lightbox__toggle-label"
                  :class="{ 'lightbox__toggle-label--active': !isScreenshotDark }"
                >
                  <v-icon size="16" icon="mdi-weather-sunny" />
                </span>
                <button
                  class="lightbox__switch"
                  :class="{ 'lightbox__switch--dark': isScreenshotDark }"
                  role="switch"
                  :aria-checked="isScreenshotDark"
                  :aria-label="t('screenshots.toggleTheme')"
                  @click="toggleScreenshotTheme"
                >
                  <span class="lightbox__switch-thumb" />
                </button>
                <span
                  class="lightbox__toggle-label"
                  :class="{ 'lightbox__toggle-label--active': isScreenshotDark }"
                >
                  <v-icon size="16" icon="mdi-weather-night" />
                </span>
              </div>

              <Transition name="screenshot-fade" mode="out-in">
                <img
                  :key="`lightbox-${currentLightboxScreenshot.id}-${screenshotTheme}`"
                  class="lightbox__image"
                  :src="isScreenshotDark ? currentLightboxScreenshot.darkSrc : currentLightboxScreenshot.lightSrc"
                  :alt="t(currentLightboxScreenshot.labelKey)"
                />
              </Transition>

              <!-- Caption -->
              <div class="lightbox__caption">
                <span class="lightbox__label">{{ t(currentLightboxScreenshot.labelKey) }}</span>
                <span class="lightbox__counter">{{ lightboxIndex + 1 }} / {{ totalSlides }}</span>
              </div>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </section>
</template>

<style scoped>
.screenshots-section {
  position: relative;
  overflow: hidden;
  padding-top: 32px !important;
  padding-bottom: 24px !important;
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
  max-width: 100%;
  margin: 0 auto;
  padding: 0;
}

/* ─── Header ─── */
.screenshots-section__header {
  text-align: center;
  margin-bottom: 24px;
  padding: 0 16px;
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
  gap: 2px;
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

/* ─── Image Wrapper ─── */
.screenshots-section__image-wrapper {
  position: relative;
  border-radius: 0;
  overflow: hidden;
  box-shadow: none;
  transition: none;
}

.screenshots-section__slide:hover .screenshots-section__image-wrapper {
  box-shadow: none;
}

/* ─── Zoom Overlay ─── */
.screenshots-section__zoom-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0;
  transition: opacity 0.3s ease;
  pointer-events: none;
}

.screenshots-section__slide:hover .screenshots-section__zoom-overlay {
  opacity: 1;
}

.screenshots-section__zoom-icon {
  font-size: 48px !important;
  color: #fff;
  filter: drop-shadow(0 2px 8px rgba(0, 0, 0, 0.3));
}

/* ─── Image ─── */
.screenshots-section__image {
  width: 100%;
  height: auto;
  object-fit: contain;
  display: block;
}

/* ─── Caption ─── */
.screenshots-section__caption {
  display: block;
  text-align: center;
  margin-top: 8px;
  font-size: 0.85rem;
  font-weight: 600;
  opacity: 0.7;
  letter-spacing: 0.02em;
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

.v-theme--dark .screenshots-section__image-wrapper {
  box-shadow: none;
}

.v-theme--dark .screenshots-section__slide:hover .screenshots-section__image-wrapper {
  box-shadow: none;
}

.v-theme--dark .screenshots-section__caption {
  color: #94a3b8;
  opacity: 0.9;
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

.v-theme--light .screenshots-section__caption {
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
    gap: 2px;
    /* Each slide is 50% - 1px; shift by (50% + 1px) per index */
    transform: translateX(calc(var(--active-index, 0) * (-50% - 1px))) !important;
  }

  .screenshots-section__slide {
    flex: 0 0 calc(50% - 1px);
    min-width: calc(50% - 1px);
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
    gap: 2px;
    transform: translateX(calc(var(--active-index, 0) * (-100% - 2px))) !important;
  }

  .screenshots-section__slide {
    flex: 0 0 100%;
    min-width: 100%;
  }

  .screenshots-section__image-wrapper {
    border-radius: 0;
  }

  .screenshots-section__caption {
    font-size: 0.78rem;
    margin-top: 10px;
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

  .screenshots-section__nav-btn {
    width: 32px;
    height: 32px;
  }

  .screenshots-section__zoom-icon {
    font-size: 32px !important;
  }
}
</style>

<style>
/* ─── Lightbox (non-scoped for Teleport) ─── */
.lightbox {
  position: fixed;
  inset: 0;
  z-index: 9999;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.9);
  backdrop-filter: blur(8px);
  padding: 20px;
}

.lightbox__content {
  position: relative;
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
}

.lightbox__close {
  position: absolute;
  top: 10px;
  right: 10px;
  z-index: 10;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 44px;
  height: 44px;
  border-radius: 50%;
  border: none;
  background: rgba(255, 255, 255, 0.1);
  color: #fff;
  cursor: pointer;
  transition: background 0.2s ease, transform 0.2s ease;
}

.lightbox__close:hover {
  background: rgba(255, 255, 255, 0.2);
  transform: scale(1.1);
}

.lightbox__nav {
  position: absolute;
  top: 50%;
  transform: translateY(-50%);
  z-index: 10;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  border-radius: 50%;
  border: none;
  background: rgba(255, 255, 255, 0.1);
  color: #fff;
  cursor: pointer;
  transition: background 0.2s ease, transform 0.2s ease;
}

.lightbox__nav:hover {
  background: rgba(255, 255, 255, 0.2);
  transform: translateY(-50%) scale(1.1);
}

.lightbox__nav--prev {
  left: 10px;
}

.lightbox__nav--next {
  right: 10px;
}

.lightbox__image-wrapper {
  position: relative;
  max-width: 95vw;
  max-height: 90vh;
  display: flex;
  flex-direction: column;
  align-items: center;
}

.lightbox__toggle {
  position: absolute;
  top: -50px;
  left: 50%;
  transform: translateX(-50%);
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 6px 14px;
  border-radius: 100px;
  background: rgba(255, 255, 255, 0.1);
  backdrop-filter: blur(8px);
  border: 1px solid rgba(255, 255, 255, 0.15);
}

.lightbox__toggle-label {
  display: inline-flex;
  align-items: center;
  color: rgba(255, 255, 255, 0.4);
  transition: color 0.3s ease;
}

.lightbox__toggle-label--active {
  color: #f97316;
}

.lightbox__switch {
  position: relative;
  width: 36px;
  height: 20px;
  border-radius: 100px;
  border: none;
  background: linear-gradient(135deg, #fbbf24, #f97316);
  cursor: pointer;
  transition: background 0.3s ease;
  padding: 0;
}

.lightbox__switch--dark {
  background: linear-gradient(135deg, #6366f1, #3b82f6);
}

.lightbox__switch-thumb {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 4px rgba(0, 0, 0, 0.15);
  transition: transform 0.3s cubic-bezier(0.4, 0, 0.2, 1);
}

.lightbox__switch--dark .lightbox__switch-thumb {
  transform: translateX(16px);
}

.lightbox__image {
  max-width: 100%;
  max-height: 85vh;
  object-fit: contain;
  border-radius: 14px;
  box-shadow: 0 24px 70px rgba(0, 0, 0, 0.5);
}

.lightbox__caption {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  width: 100%;
  max-width: 400px;
  margin-top: 16px;
  padding: 10px 20px;
  background: rgba(255, 255, 255, 0.08);
  border-radius: 100px;
  backdrop-filter: blur(8px);
}

.lightbox__label {
  font-size: 0.85rem;
  font-weight: 600;
  color: rgba(255, 255, 255, 0.9);
}

.lightbox__counter {
  font-size: 0.78rem;
  font-weight: 500;
  color: rgba(255, 255, 255, 0.5);
  font-variant-numeric: tabular-nums;
}

/* ─── Lightbox Transitions ─── */
.lightbox-fade-enter-active,
.lightbox-fade-leave-active {
  transition: opacity 0.3s ease;
}

.lightbox-fade-enter-from,
.lightbox-fade-leave-to {
  opacity: 0;
}

/* ─── Lightbox Responsive ─── */
@media (max-width: 680px) {
  .lightbox {
    padding: 10px;
  }

  .lightbox__close {
    top: 5px;
    right: 5px;
    width: 40px;
    height: 40px;
  }

  .lightbox__nav {
    width: 40px;
    height: 40px;
  }

  .lightbox__nav--prev {
    left: 5px;
  }

  .lightbox__nav--next {
    right: 5px;
  }

  .lightbox__toggle {
    top: -45px;
    padding: 4px 10px;
    gap: 6px;
  }

  .lightbox__image {
    max-height: 60vh;
    border-radius: 8px;
  }

  .lightbox__caption {
    padding: 8px 16px;
    margin-top: 12px;
  }

  .lightbox__label {
    font-size: 0.78rem;
  }
}
</style>
