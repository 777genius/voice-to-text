<script setup lang="ts">
import { ref } from "vue";
import { Swiper, SwiperSlide } from "swiper/vue";
import { A11y, Keyboard, Pagination } from "swiper/modules";
import "swiper/css";
import "swiper/css/pagination";
import { screenshots } from "~/data/screenshots";

const { t } = useI18n();
const activeIndex = ref(0);

function onSlideChange(swiper: { activeIndex: number }) {
  activeIndex.value = swiper.activeIndex;
}
</script>

<template>
  <ClientOnly>
    <div class="carousel">
      <!-- Tab navigation -->
      <div class="carousel__tabs">
        <button
          v-for="(shot, index) in screenshots"
          :key="shot.id"
          class="carousel__tab"
          :class="{ 'carousel__tab--active': activeIndex === index }"
          @click="activeIndex = index"
        >
          <span class="carousel__tab-dot" />
          <span class="carousel__tab-label">{{ t(shot.labelKey) }}</span>
        </button>
      </div>

      <!-- Swiper -->
      <Swiper
        :modules="[A11y, Keyboard, Pagination]"
        :slides-per-view="1.05"
        :space-between="20"
        :grab-cursor="true"
        :keyboard="{ enabled: true }"
        :a11y="{ enabled: true }"
        :pagination="{ clickable: true, el: '.carousel__pagination' }"
        :centered-slides="true"
        :breakpoints="{
          640: { slidesPerView: 1.3, spaceBetween: 24 },
          960: { slidesPerView: 1.6, spaceBetween: 32 },
          1280: { slidesPerView: 2.0, spaceBetween: 40 }
        }"
        @slide-change="onSlideChange"
      >
        <SwiperSlide v-for="(shot, index) in screenshots" :key="shot.id">
          <div
            class="carousel__card"
            :class="{ 'carousel__card--active': activeIndex === index }"
          >
            <div class="carousel__card-glow" />
            <div class="carousel__card-inner">
              <div class="carousel__card-header">
                <div class="carousel__card-dots">
                  <span /><span /><span />
                </div>
                <span class="carousel__card-label">{{ t(shot.labelKey) }}</span>
              </div>
              <img
                class="carousel__image"
                :src="shot.src"
                :alt="t(shot.labelKey)"
                :width="shot.width"
                :height="shot.height"
                loading="lazy"
                decoding="async"
              />
            </div>
          </div>
        </SwiperSlide>
      </Swiper>

      <!-- Custom pagination -->
      <div class="carousel__pagination" />
    </div>

    <template #fallback>
      <v-row class="ga-4" align="stretch">
        <v-col v-for="shot in screenshots" :key="shot.id" cols="12" md="6" lg="3">
          <div class="carousel__card carousel__card--active">
            <div class="carousel__card-inner">
              <div class="carousel__card-header">
                <div class="carousel__card-dots">
                  <span /><span /><span />
                </div>
                <span class="carousel__card-label">{{ t(shot.labelKey) }}</span>
              </div>
              <img
                class="carousel__image"
                :src="shot.src"
                :alt="t(shot.labelKey)"
                :width="shot.width"
                :height="shot.height"
                loading="lazy"
                decoding="async"
              />
            </div>
          </div>
        </v-col>
      </v-row>
    </template>
  </ClientOnly>
</template>

<style scoped>
.carousel {
  position: relative;
}

/* ─── Tabs ─── */
.carousel__tabs {
  display: flex;
  justify-content: center;
  gap: 8px;
  margin-bottom: 32px;
  flex-wrap: wrap;
}

.carousel__tab {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 20px;
  border-radius: 100px;
  border: 1px solid rgba(249, 115, 22, 0.12);
  background: rgba(255, 255, 255, 0.5);
  backdrop-filter: blur(8px);
  font-size: 0.85rem;
  font-weight: 600;
  color: inherit;
  opacity: 0.6;
  cursor: pointer;
  transition:
    opacity 0.3s ease,
    background 0.3s ease,
    border-color 0.3s ease,
    transform 0.25s ease,
    box-shadow 0.3s ease;
}

.carousel__tab:hover {
  opacity: 0.85;
  transform: translateY(-1px);
}

.carousel__tab--active {
  opacity: 1;
  background: linear-gradient(135deg, rgba(249, 115, 22, 0.1), rgba(6, 182, 212, 0.08));
  border-color: rgba(249, 115, 22, 0.3);
  box-shadow: 0 4px 16px rgba(249, 115, 22, 0.1);
}

.carousel__tab-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
  opacity: 0.4;
  transition: opacity 0.3s ease, background 0.3s ease;
}

.carousel__tab--active .carousel__tab-dot {
  opacity: 1;
  background: #f97316;
}

.carousel__tab-label {
  letter-spacing: 0.02em;
}

/* ─── Card ─── */
.carousel__card {
  position: relative;
  border-radius: 20px;
  overflow: hidden;
  transition:
    transform 0.45s cubic-bezier(0.4, 0, 0.2, 1),
    box-shadow 0.45s cubic-bezier(0.4, 0, 0.2, 1);
  animation: screenshotFadeUp 0.5s ease both;
}

.carousel__card:hover {
  transform: translateY(-4px) scale(1.01);
}

.carousel__card--active {
  box-shadow:
    0 24px 80px rgba(249, 115, 22, 0.1),
    0 8px 32px rgba(0, 0, 0, 0.06);
}

.carousel__card-glow {
  position: absolute;
  inset: 0;
  background: radial-gradient(
    ellipse 80% 40% at 50% 0%,
    rgba(249, 115, 22, 0.06),
    transparent 70%
  );
  pointer-events: none;
  opacity: 0;
  transition: opacity 0.4s ease;
  z-index: 1;
}

.carousel__card:hover .carousel__card-glow,
.carousel__card--active .carousel__card-glow {
  opacity: 1;
}

.carousel__card-inner {
  background: rgba(255, 255, 255, 0.6);
  border: 1px solid rgba(249, 115, 22, 0.1);
  border-radius: 20px;
  backdrop-filter: blur(16px);
  overflow: hidden;
}

/* ─── Card Header (window chrome) ─── */
.carousel__card-header {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 20px;
  border-bottom: 1px solid rgba(249, 115, 22, 0.06);
  background: rgba(255, 255, 255, 0.4);
}

.carousel__card-dots {
  display: flex;
  gap: 6px;
}

.carousel__card-dots span {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.1);
}

.carousel__card-dots span:nth-child(1) {
  background: #ff5f57;
}

.carousel__card-dots span:nth-child(2) {
  background: #febc2e;
}

.carousel__card-dots span:nth-child(3) {
  background: #28c840;
}

.carousel__card-label {
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.03em;
  opacity: 0.5;
}

/* ─── Image ─── */
.carousel__image {
  width: 100%;
  height: auto;
  display: block;
}

/* ─── Pagination ─── */
.carousel__pagination {
  display: flex;
  justify-content: center;
  gap: 8px;
  margin-top: 28px;
}

.carousel__pagination :deep(.swiper-pagination-bullet) {
  width: 8px;
  height: 8px;
  border-radius: 100px;
  background: rgba(249, 115, 22, 0.25);
  opacity: 1;
  transition: width 0.3s ease, background 0.3s ease;
}

.carousel__pagination :deep(.swiper-pagination-bullet-active) {
  width: 28px;
  background: linear-gradient(90deg, #f97316, #06b6d4);
}

@keyframes screenshotFadeUp {
  from {
    opacity: 0;
    transform: translateY(20px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

/* ─── Dark Theme ─── */
.v-theme--dark .carousel__tab {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(251, 146, 60, 0.1);
}

.v-theme--dark .carousel__tab--active {
  background: linear-gradient(135deg, rgba(251, 146, 60, 0.12), rgba(34, 211, 238, 0.08));
  border-color: rgba(251, 146, 60, 0.3);
  box-shadow: 0 4px 16px rgba(251, 146, 60, 0.08);
}

.v-theme--dark .carousel__tab--active .carousel__tab-dot {
  background: #fdba74;
}

.v-theme--dark .carousel__card-inner {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(251, 146, 60, 0.08);
}

.v-theme--dark .carousel__card--active {
  box-shadow:
    0 24px 80px rgba(0, 0, 0, 0.4),
    0 0 0 1px rgba(251, 146, 60, 0.1);
}

.v-theme--dark .carousel__card:hover {
  box-shadow:
    0 24px 80px rgba(0, 0, 0, 0.5),
    0 0 0 1px rgba(251, 146, 60, 0.15);
}

.v-theme--dark .carousel__card-glow {
  background: radial-gradient(
    ellipse 80% 40% at 50% 0%,
    rgba(251, 146, 60, 0.08),
    transparent 70%
  );
}

.v-theme--dark .carousel__card-header {
  border-bottom-color: rgba(255, 255, 255, 0.06);
  background: rgba(255, 255, 255, 0.03);
}

.v-theme--dark .carousel__card-dots span:nth-child(1) {
  background: #ff6b6b;
}

.v-theme--dark .carousel__card-dots span:nth-child(2) {
  background: #ffd93d;
}

.v-theme--dark .carousel__card-dots span:nth-child(3) {
  background: #6bcb77;
}

.v-theme--dark .carousel__card-label {
  color: #94a3b8;
}

.v-theme--dark .carousel__pagination :deep(.swiper-pagination-bullet) {
  background: rgba(251, 146, 60, 0.2);
}

.v-theme--dark .carousel__pagination :deep(.swiper-pagination-bullet-active) {
  background: linear-gradient(90deg, #fb923c, #22d3ee);
}

/* ─── Light Theme ─── */
.v-theme--light .carousel__card-inner {
  background: rgba(255, 255, 255, 0.85);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.04), 0 4px 16px rgba(0, 0, 0, 0.02);
}

.v-theme--light .carousel__card--active .carousel__card-inner {
  box-shadow:
    0 1px 3px rgba(0, 0, 0, 0.04),
    0 8px 32px rgba(249, 115, 22, 0.06);
}

.v-theme--light .carousel__card-header {
  background: rgba(249, 250, 251, 0.8);
}

.v-theme--light .carousel__card-label {
  color: #64748b;
}

.v-theme--light .carousel__tab {
  background: rgba(255, 255, 255, 0.75);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.03);
}

/* ─── Responsive ─── */
@media (max-width: 960px) {
  .carousel__tabs {
    margin-bottom: 24px;
  }

  .carousel__tab {
    padding: 6px 16px;
    font-size: 0.8rem;
  }
}

@media (max-width: 600px) {
  .carousel__tabs {
    gap: 6px;
    margin-bottom: 20px;
  }

  .carousel__tab {
    padding: 6px 14px;
    font-size: 0.75rem;
  }

  .carousel__card-inner {
    border-radius: 16px;
  }

  .carousel__card {
    border-radius: 16px;
  }

  .carousel__card-header {
    padding: 10px 16px;
  }

  .carousel__card-dots span {
    width: 8px;
    height: 8px;
  }

  .carousel__pagination {
    margin-top: 20px;
  }
}
</style>
