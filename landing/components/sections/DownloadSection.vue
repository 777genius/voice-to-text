<script setup lang="ts">
import { downloadAssets } from "~/data/downloads";
import type { DownloadArch } from "~/data/downloads";

const { content } = useLandingContent();
const { t } = useI18n();
const downloadStore = useDownloadStore();
const { resolve } = useReleaseDownloads();

onMounted(() => downloadStore.init());

const platformIcons: Record<string, string> = {
  macos: "mdi-apple",
  windows: "mdi-microsoft-windows",
  linux: "mdi-penguin",
};

const platformColors: Record<string, string> = {
  macos: "#a78bfa",
  windows: "#60a5fa",
  linux: "#fbbf24",
};

const getDownloadUrl = (asset: (typeof downloadAssets)[number]) => {
  if (asset.os === "macos") {
    const arch = (downloadStore.arch === "unknown" ? "x64" : downloadStore.arch) as DownloadArch;
    return resolve("macos", arch)?.url || asset.url;
  }
  return resolve(asset.os, asset.arch)?.url || asset.url;
};

const getDownloadVersion = (asset: (typeof downloadAssets)[number]) => {
  if (asset.os === "macos") {
    const arch = (downloadStore.arch === "unknown" ? "x64" : downloadStore.arch) as DownloadArch;
    return resolve("macos", arch)?.version || null;
  }
  return resolve(asset.os, asset.arch)?.version || null;
};

</script>

<template>
  <section id="download" class="download-section section anchor-offset">
    <!-- Background decoration -->
    <div class="download-section__bg">
      <div class="download-section__orb download-section__orb--1" />
      <div class="download-section__orb download-section__orb--2" />
      <div class="download-section__orb download-section__orb--3" />
      <div class="download-section__grid-pattern" />
    </div>

    <v-container>
      <!-- Header -->
      <div class="download-section__header">
        <span class="download-section__badge">{{ t("nav.download") }}</span>
        <h2 class="download-section__title">{{ content.download.title }}</h2>
        <p class="download-section__subtitle">{{ content.download.note }}</p>
      </div>

      <!-- Platform cards -->
      <div class="download-section__cards">
        <div
          v-for="(asset, index) in downloadAssets"
          :key="asset.id"
          class="download-section__card"
          :class="{ 'download-section__card--active': downloadStore.selectedId === asset.id }"
          :style="{
            '--delay': `${index * 0.1}s`,
            '--accent': platformColors[asset.os] || '#60a5fa',
          }"
          @click="downloadStore.setSelected(asset.id)"
        >
          <!-- Card glow effect -->
          <div class="download-section__card-glow" />

          <!-- Platform icon -->
          <div class="download-section__card-icon-wrap">
            <v-icon size="28" class="download-section__card-icon">
              {{ platformIcons[asset.os] || "mdi-download" }}
            </v-icon>
          </div>

          <!-- Platform info -->
          <div class="download-section__card-info">
            <h3 class="download-section__card-label">{{ asset.label }}</h3>
            <span class="download-section__card-arch">{{ asset.arch }}</span>
            <span v-if="getDownloadVersion(asset)" class="download-section__card-version">
              v{{ getDownloadVersion(asset) }}
            </span>
          </div>

          <!-- Download button -->
          <a
            class="download-section__btn"
            :href="getDownloadUrl(asset)"
            @click.stop="downloadStore.setSelected(asset.id)"
          >
            <v-icon size="18" class="download-section__btn-icon">mdi-download</v-icon>
            <span>{{ t("download.title") }}</span>
          </a>

          <!-- Active indicator -->
          <div
            v-if="downloadStore.selectedId === asset.id"
            class="download-section__card-indicator"
          >
            <v-icon size="16">mdi-check-circle</v-icon>
            <span>{{ t("download.detected") }}</span>
          </div>
        </div>
      </div>
    </v-container>
  </section>
</template>

<style scoped>
.download-section {
  position: relative;
  overflow: hidden;
}

/* ─── Background ─── */
.download-section__bg {
  position: absolute;
  inset: 0;
  pointer-events: none;
  overflow: hidden;
}

.download-section__orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(120px);
  opacity: 0.07;
}

.download-section__orb--1 {
  width: 500px;
  height: 500px;
  background: #8b5cf6;
  top: -180px;
  right: -60px;
}

.download-section__orb--2 {
  width: 400px;
  height: 400px;
  background: #3b82f6;
  bottom: -100px;
  left: -80px;
}

.download-section__orb--3 {
  width: 300px;
  height: 300px;
  background: #f59e0b;
  bottom: -80px;
  right: 30%;
  opacity: 0.04;
}

.download-section__grid-pattern {
  position: absolute;
  inset: 0;
  background-image:
    linear-gradient(rgba(139, 92, 246, 0.03) 1px, transparent 1px),
    linear-gradient(90deg, rgba(139, 92, 246, 0.03) 1px, transparent 1px);
  background-size: 48px 48px;
  mask-image: radial-gradient(ellipse 70% 60% at 50% 40%, black, transparent);
}

/* ─── Header ─── */
.download-section__header {
  text-align: center;
  max-width: 560px;
  margin: 0 auto 56px;
  position: relative;
  z-index: 1;
}

.download-section__badge {
  display: inline-block;
  padding: 6px 18px;
  border-radius: 100px;
  font-size: 0.8rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  background: linear-gradient(135deg, rgba(139, 92, 246, 0.15), rgba(59, 130, 246, 0.15));
  color: #8b5cf6;
  margin-bottom: 20px;
  border: 1px solid rgba(139, 92, 246, 0.2);
}

.download-section__title {
  font-size: 2.4rem;
  font-weight: 800;
  letter-spacing: -0.03em;
  line-height: 1.15;
  margin-bottom: 16px;
  background: linear-gradient(135deg, currentColor 0%, rgba(139, 92, 246, 0.8) 100%);
  -webkit-background-clip: text;
  background-clip: text;
}

.download-section__subtitle {
  font-size: 1.1rem;
  opacity: 0.6;
  line-height: 1.6;
  margin: 0;
}

/* ─── Cards Grid ─── */
.download-section__cards {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 18px;
  position: relative;
  z-index: 1;
  max-width: 840px;
  margin: 0 auto;
}

/* ─── Card ─── */
.download-section__card {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  padding: 26px 22px 24px;
  border-radius: 16px;
  background: rgba(255, 255, 255, 0.55);
  border: 1px solid rgba(139, 92, 246, 0.08);
  backdrop-filter: blur(16px);
  cursor: pointer;
  transition:
    transform 0.35s cubic-bezier(0.4, 0, 0.2, 1),
    box-shadow 0.35s cubic-bezier(0.4, 0, 0.2, 1),
    border-color 0.35s ease;
  overflow: hidden;
  animation: downloadFadeUp 0.5s ease both;
  animation-delay: var(--delay, 0s);
}

.download-section__card:hover {
  transform: translateY(-6px);
  border-color: rgba(139, 92, 246, 0.2);
  box-shadow:
    0 20px 60px rgba(139, 92, 246, 0.1),
    0 4px 16px rgba(0, 0, 0, 0.04);
}

.download-section__card--active {
  border-color: rgba(34, 197, 94, 0.4);
  background: rgba(34, 197, 94, 0.06);
  box-shadow:
    0 8px 32px rgba(34, 197, 94, 0.12),
    0 0 0 2px rgba(34, 197, 94, 0.2);
}

.download-section__card--active:hover {
  border-color: rgba(34, 197, 94, 0.5);
  box-shadow:
    0 20px 60px rgba(34, 197, 94, 0.18),
    0 0 0 2px rgba(34, 197, 94, 0.25);
}

/* Card glow */
.download-section__card-glow {
  position: absolute;
  top: 0;
  left: 0;
  width: 100%;
  height: 100%;
  background: radial-gradient(
    ellipse 80% 60% at 50% 0%,
    color-mix(in srgb, var(--accent) 8%, transparent),
    transparent 70%
  );
  pointer-events: none;
  opacity: 0;
  transition: opacity 0.35s ease;
}

.download-section__card:hover .download-section__card-glow {
  opacity: 1;
}

.download-section__card--active .download-section__card-glow {
  opacity: 0.7;
  background: radial-gradient(
    ellipse 80% 60% at 50% 0%,
    rgba(34, 197, 94, 0.1),
    transparent 70%
  );
}

/* Icon wrap */
.download-section__card-icon-wrap {
  width: 56px;
  height: 56px;
  border-radius: 16px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(
    135deg,
    color-mix(in srgb, var(--accent) 12%, transparent),
    color-mix(in srgb, var(--accent) 6%, transparent)
  );
  border: 1px solid color-mix(in srgb, var(--accent) 15%, transparent);
  margin-bottom: 14px;
  transition: transform 0.35s ease, box-shadow 0.35s ease;
}

.download-section__card:hover .download-section__card-icon-wrap {
  transform: scale(1.08);
  box-shadow: 0 8px 24px color-mix(in srgb, var(--accent) 15%, transparent);
}

.download-section__card-icon {
  color: var(--accent);
}

/* Info */
.download-section__card-info {
  margin-bottom: 16px;
}

.download-section__card-label {
  font-size: 1.05rem;
  font-weight: 700;
  margin-bottom: 3px;
  letter-spacing: -0.01em;
}

.download-section__card-arch {
  font-size: 0.72rem;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  opacity: 0.45;
}

.download-section__card-version {
  display: inline-block;
  margin-top: 6px;
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0.02em;
  opacity: 0.5;
}

/* Download button */
.download-section__btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 8px 22px;
  border-radius: 10px;
  font-size: 0.84rem;
  font-weight: 600;
  text-decoration: none;
  color: #fff;
  background: linear-gradient(135deg, #8b5cf6, #6366f1);
  transition:
    transform 0.25s ease,
    box-shadow 0.25s ease,
    filter 0.25s ease;
  box-shadow: 0 4px 16px rgba(139, 92, 246, 0.3);
}

.download-section__btn:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 24px rgba(139, 92, 246, 0.4);
  filter: brightness(1.08);
}

.download-section__btn:active {
  transform: translateY(0);
}

.download-section__btn-icon {
  color: inherit;
}

/* Active indicator */
.download-section__card-indicator {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 10px;
  font-size: 0.72rem;
  font-weight: 600;
  color: #22c55e;
  opacity: 0.9;
}

@keyframes downloadFadeUp {
  from {
    opacity: 0;
    transform: translateY(28px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

/* ─── Dark Theme ─── */
.v-theme--dark .download-section__orb {
  opacity: 0.12;
}

.v-theme--dark .download-section__orb--1 {
  background: #a78bfa;
}

.v-theme--dark .download-section__orb--2 {
  background: #60a5fa;
}

.v-theme--dark .download-section__orb--3 {
  background: #fbbf24;
  opacity: 0.06;
}

.v-theme--dark .download-section__grid-pattern {
  background-image:
    linear-gradient(rgba(167, 139, 250, 0.04) 1px, transparent 1px),
    linear-gradient(90deg, rgba(167, 139, 250, 0.04) 1px, transparent 1px);
}

.v-theme--dark .download-section__badge {
  background: linear-gradient(135deg, rgba(167, 139, 250, 0.15), rgba(96, 165, 250, 0.15));
  color: #c4b5fd;
  border-color: rgba(167, 139, 250, 0.25);
}

.v-theme--dark .download-section__title {
  background: linear-gradient(135deg, #e2e8f0 0%, #c4b5fd 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--dark .download-section__subtitle {
  color: #94a3b8;
  opacity: 0.8;
}

.v-theme--dark .download-section__card {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(167, 139, 250, 0.08);
}

.v-theme--dark .download-section__card:hover {
  background: rgba(255, 255, 255, 0.06);
  border-color: rgba(167, 139, 250, 0.2);
  box-shadow:
    0 20px 60px rgba(0, 0, 0, 0.4),
    0 0 0 1px rgba(167, 139, 250, 0.1);
}

.v-theme--dark .download-section__card--active {
  border-color: rgba(74, 222, 128, 0.35);
  background: rgba(34, 197, 94, 0.08);
  box-shadow:
    0 8px 32px rgba(0, 0, 0, 0.3),
    0 0 0 2px rgba(74, 222, 128, 0.18);
}

.v-theme--dark .download-section__card--active:hover {
  border-color: rgba(74, 222, 128, 0.45);
  box-shadow:
    0 20px 60px rgba(0, 0, 0, 0.45),
    0 0 0 2px rgba(74, 222, 128, 0.25);
}

.v-theme--dark .download-section__card-label {
  color: #e2e8f0;
}

.v-theme--dark .download-section__card-arch {
  color: #94a3b8;
}

.v-theme--dark .download-section__btn {
  background: linear-gradient(135deg, #a78bfa, #818cf8);
  box-shadow: 0 4px 16px rgba(167, 139, 250, 0.25);
}

.v-theme--dark .download-section__btn:hover {
  box-shadow: 0 8px 24px rgba(167, 139, 250, 0.35);
}

.v-theme--dark .download-section__card-indicator {
  color: #4ade80;
}

/* ─── Light Theme ─── */
.v-theme--light .download-section__orb {
  opacity: 0.05;
}

.v-theme--light .download-section__badge {
  color: #7c3aed;
}

.v-theme--light .download-section__card {
  background: rgba(255, 255, 255, 0.75);
  box-shadow:
    0 1px 3px rgba(0, 0, 0, 0.04),
    0 4px 16px rgba(0, 0, 0, 0.02);
}

.v-theme--light .download-section__card:hover {
  box-shadow:
    0 20px 60px rgba(139, 92, 246, 0.1),
    0 4px 16px rgba(0, 0, 0, 0.04);
}

.v-theme--light .download-section__card--active {
  background: rgba(240, 253, 244, 0.9);
  border-color: rgba(34, 197, 94, 0.35);
}

.v-theme--light .download-section__card-label {
  color: #1e293b;
}

.v-theme--light .download-section__card-arch {
  color: #64748b;
}

.v-theme--light .download-section__subtitle {
  color: #475569;
}

.v-theme--light .download-section__card-indicator {
  color: #16a34a;
}

/* ─── Responsive ─── */
@media (max-width: 960px) {
  .download-section__cards {
    grid-template-columns: 1fr;
    max-width: 420px;
    margin: 0 auto;
  }

  .download-section__card {
    flex-direction: row;
    text-align: left;
    padding: 24px 28px;
    gap: 20px;
  }

  .download-section__card-icon-wrap {
    margin-bottom: 0;
    width: 60px;
    height: 60px;
    flex-shrink: 0;
  }

  .download-section__card-info {
    margin-bottom: 0;
    flex: 1;
    min-width: 0;
  }

  .download-section__card-indicator {
    position: absolute;
    top: 12px;
    right: 16px;
    margin-top: 0;
  }

  .download-section__title {
    font-size: 1.85rem;
  }

  .download-section__header {
    margin-bottom: 40px;
  }

  .download-section__subtitle {
    font-size: 1rem;
  }
}

@media (max-width: 600px) {
  .download-section__title {
    font-size: 1.6rem;
  }

  .download-section__header {
    margin-bottom: 32px;
  }

  .download-section__card {
    padding: 20px 22px;
    gap: 16px;
    border-radius: 16px;
  }

  .download-section__card-icon-wrap {
    width: 52px;
    height: 52px;
    border-radius: 14px;
  }

  .download-section__card-label {
    font-size: 1.05rem;
  }

  .download-section__btn {
    padding: 8px 20px;
    font-size: 0.85rem;
  }
}
</style>
