<script setup lang="ts">
const { content } = useLandingContent();
const { t } = useI18n();

// Deterministic waveform heights for SSR consistency
const waveformHeights = Array.from({ length: 32 }, (_, i) => {
  const idx = i + 1;
  // Seeded pseudo-random using a simple LCG-like approach
  const pseudo = ((idx * 2654435761) >>> 0) % 100;
  return 20 + Math.sin(idx * 0.6) * 40 + (pseudo / 100) * 30;
});
</script>

<template>
  <section id="hero" class="hero-section section anchor-offset">
    <!-- Background decoration -->
    <div class="hero-section__bg">
      <div class="hero-section__orb hero-section__orb--1" />
      <div class="hero-section__orb hero-section__orb--2" />
      <div class="hero-section__orb hero-section__orb--3" />
      <div class="hero-section__grid-pattern" />
    </div>

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
              {{ t('hero.ctaSecondary') }}
            </v-btn>
          </div>

          <!-- Trust indicators -->
          <div class="hero-section__trust">
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon">mdi-lightning-bolt</v-icon>
              <span>{{ t("hero.trust.realtime") }}</span>
            </div>
            <div class="hero-section__trust-divider" />
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon">mdi-translate</v-icon>
              <span>{{ t("hero.trust.multilingual") }}</span>
            </div>
            <div class="hero-section__trust-divider" />
            <div class="hero-section__trust-item">
              <v-icon size="16" class="hero-section__trust-icon">mdi-monitor-multiple</v-icon>
              <span>{{ t("hero.trust.crossPlatform") }}</span>
            </div>
          </div>
        </v-col>

        <!-- Right: Preview card -->
        <v-col cols="12" md="5">
          <div class="hero-section__preview">
            <!-- Card glow -->
            <div class="hero-section__preview-glow" />

            <div class="hero-section__preview-inner">
              <!-- Top bar -->
              <div class="hero-section__preview-bar">
                <div class="hero-section__preview-dots">
                  <span /><span /><span />
                </div>
                <span class="hero-section__preview-label">{{ t("hero.preview") }}</span>
              </div>

              <!-- Waveform visualization -->
              <div class="hero-section__waveform">
                <div
                  v-for="(height, i) in waveformHeights"
                  :key="i"
                  class="hero-section__waveform-bar"
                  :style="{
                    '--bar-index': i + 1,
                    '--bar-height': `${height}%`,
                  }"
                />
              </div>

              <!-- Transcription preview -->
              <div class="hero-section__transcription">
                <div class="hero-section__transcription-line hero-section__transcription-line--1">
                  <span class="hero-section__transcription-text">{{ t("hero.transcription.sample") }}</span>
                </div>
                <div class="hero-section__transcription-line hero-section__transcription-line--2">
                  <span class="hero-section__transcription-cursor" />
                </div>
              </div>

              <!-- Status bar -->
              <div class="hero-section__preview-status">
                <div class="hero-section__status-recording">
                  <span class="hero-section__status-dot" />
                  <span>{{ t("hero.status.recording") }}</span>
                </div>
                <span class="hero-section__status-time">0:04</span>
              </div>
            </div>
          </div>
        </v-col>
      </v-row>

    </v-container>
  </section>
</template>

<style scoped>
.hero-section {
  position: relative;
  overflow: hidden;
  min-height: 85vh;
  display: flex;
  align-items: center;
}

/* ─── Background ─── */
.hero-section__bg {
  position: absolute;
  inset: 0;
  pointer-events: none;
  overflow: hidden;
}

.hero-section__orb {
  position: absolute;
  border-radius: 50%;
  filter: blur(120px);
  opacity: 0.08;
}

.hero-section__orb--1 {
  width: 700px;
  height: 700px;
  background: #6366f1;
  top: -300px;
  right: -150px;
  animation: orbFloat1 20s ease-in-out infinite;
}

.hero-section__orb--2 {
  width: 500px;
  height: 500px;
  background: #ec4899;
  bottom: -200px;
  left: -100px;
  animation: orbFloat2 25s ease-in-out infinite;
}

.hero-section__orb--3 {
  width: 400px;
  height: 400px;
  background: #8b5cf6;
  top: 30%;
  left: 40%;
  opacity: 0.05;
  animation: orbFloat3 18s ease-in-out infinite;
}

.hero-section__grid-pattern {
  position: absolute;
  inset: 0;
  background-image:
    linear-gradient(rgba(99, 102, 241, 0.03) 1px, transparent 1px),
    linear-gradient(90deg, rgba(99, 102, 241, 0.03) 1px, transparent 1px);
  background-size: 48px 48px;
  mask-image: radial-gradient(ellipse 80% 70% at 50% 30%, black, transparent);
}

@keyframes orbFloat1 {
  0%, 100% { transform: translate(0, 0); }
  33% { transform: translate(30px, 20px); }
  66% { transform: translate(-20px, 10px); }
}

@keyframes orbFloat2 {
  0%, 100% { transform: translate(0, 0); }
  33% { transform: translate(-25px, -15px); }
  66% { transform: translate(15px, -25px); }
}

@keyframes orbFloat3 {
  0%, 100% { transform: translate(0, 0) scale(1); }
  50% { transform: translate(20px, -30px) scale(1.1); }
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
  max-width: 480px;
  margin-bottom: 36px;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.3s;
}

/* ─── Actions ─── */
.hero-section__actions {
  display: flex;
  gap: 14px;
  flex-wrap: wrap;
  margin-bottom: 40px;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.4s;
}

.hero-section__btn-secondary {
  border-color: rgba(99, 102, 241, 0.3) !important;
  color: #6366f1 !important;
  font-weight: 600 !important;
  transition: all 0.3s ease !important;
}

.hero-section__btn-secondary:hover {
  border-color: rgba(99, 102, 241, 0.5) !important;
  background: rgba(99, 102, 241, 0.06) !important;
}

/* ─── Trust indicators ─── */
.hero-section__trust {
  display: flex;
  align-items: center;
  gap: 16px;
  animation: heroFadeIn 0.8s ease both;
  animation-delay: 0.5s;
}

.hero-section__trust-item {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 0.82rem;
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

.hero-section__preview-inner {
  position: relative;
  z-index: 1;
  border-radius: 20px;
  padding: 24px;
  background: rgba(255, 255, 255, 0.6);
  border: 1px solid rgba(99, 102, 241, 0.1);
  backdrop-filter: blur(24px);
  overflow: hidden;
}

/* ─── Preview top bar ─── */
.hero-section__preview-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 24px;
}

.hero-section__preview-dots {
  display: flex;
  gap: 6px;
}

.hero-section__preview-dots span {
  width: 10px;
  height: 10px;
  border-radius: 50%;
}

.hero-section__preview-dots span:nth-child(1) {
  background: #ef4444;
}

.hero-section__preview-dots span:nth-child(2) {
  background: #f59e0b;
}

.hero-section__preview-dots span:nth-child(3) {
  background: #22c55e;
}

.hero-section__preview-label {
  font-size: 0.75rem;
  font-weight: 600;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  opacity: 0.4;
}

/* ─── Waveform ─── */
.hero-section__waveform {
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 3px;
  height: 80px;
  margin-bottom: 24px;
  padding: 0 8px;
}

.hero-section__waveform-bar {
  flex: 1;
  max-width: 6px;
  height: var(--bar-height, 30%);
  border-radius: 100px;
  background: linear-gradient(180deg, #6366f1, #ec4899);
  opacity: 0.7;
  animation: waveAnimate 1.5s ease-in-out infinite alternate;
  animation-delay: calc(var(--bar-index, 0) * 0.05s);
}

@keyframes waveAnimate {
  0% { transform: scaleY(0.4); opacity: 0.4; }
  100% { transform: scaleY(1); opacity: 0.8; }
}

/* ─── Transcription ─── */
.hero-section__transcription {
  margin-bottom: 20px;
  padding: 16px;
  border-radius: 12px;
  background: rgba(0, 0, 0, 0.03);
  min-height: 60px;
}

.hero-section__transcription-line {
  display: flex;
  align-items: center;
}

.hero-section__transcription-line--1 {
  margin-bottom: 8px;
}

.hero-section__transcription-text {
  font-size: 0.92rem;
  line-height: 1.5;
  opacity: 0.7;
  font-family: 'SF Mono', 'Fira Code', 'Cascadia Code', monospace;
}

.hero-section__transcription-cursor {
  width: 2px;
  height: 18px;
  background: #6366f1;
  border-radius: 1px;
  animation: cursorBlink 1s step-end infinite;
}

@keyframes cursorBlink {
  0%, 100% { opacity: 1; }
  50% { opacity: 0; }
}

/* ─── Status bar ─── */
.hero-section__preview-status {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-top: 16px;
  border-top: 1px solid rgba(0, 0, 0, 0.06);
}

.hero-section__status-recording {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 0.8rem;
  font-weight: 600;
  color: #ef4444;
}

.hero-section__status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #ef4444;
  animation: pulse 1.5s ease-in-out infinite;
}

.hero-section__status-time {
  font-size: 0.8rem;
  font-weight: 600;
  opacity: 0.45;
  font-family: 'SF Mono', 'Fira Code', 'Cascadia Code', monospace;
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
.v-theme--dark .hero-section__orb {
  opacity: 0.14;
}

.v-theme--dark .hero-section__orb--1 {
  background: #818cf8;
}

.v-theme--dark .hero-section__orb--2 {
  background: #f472b6;
}

.v-theme--dark .hero-section__orb--3 {
  background: #a78bfa;
  opacity: 0.08;
}

.v-theme--dark .hero-section__grid-pattern {
  background-image:
    linear-gradient(rgba(129, 140, 248, 0.04) 1px, transparent 1px),
    linear-gradient(90deg, rgba(129, 140, 248, 0.04) 1px, transparent 1px);
}

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

.v-theme--dark .hero-section__preview-inner {
  background: rgba(255, 255, 255, 0.04);
  border-color: rgba(129, 140, 248, 0.12);
}

.v-theme--dark .hero-section__preview-glow {
  opacity: 0.25;
}

.v-theme--dark .hero-section__transcription {
  background: rgba(255, 255, 255, 0.04);
}

.v-theme--dark .hero-section__transcription-text {
  color: #cbd5e1;
}

.v-theme--dark .hero-section__transcription-cursor {
  background: #a5b4fc;
}

.v-theme--dark .hero-section__preview-status {
  border-top-color: rgba(255, 255, 255, 0.06);
}

.v-theme--dark .hero-section__status-time {
  color: #94a3b8;
}

.v-theme--dark .hero-section__waveform-bar {
  background: linear-gradient(180deg, #818cf8, #f472b6);
}

.v-theme--dark .hero-section__preview-label {
  color: #94a3b8;
}

/* ─── Light Theme ─── */
.v-theme--light .hero-section__orb {
  opacity: 0.06;
}

.v-theme--light .hero-section__badge {
  color: #4f46e5;
}

.v-theme--light .hero-section__title {
  background: linear-gradient(135deg, #1e293b 0%, #4f46e5 50%, #db2777 100%);
  -webkit-background-clip: text;
  background-clip: text;
  -webkit-text-fill-color: transparent;
}

.v-theme--light .hero-section__subtitle {
  color: #475569;
}

.v-theme--light .hero-section__preview-inner {
  background: rgba(255, 255, 255, 0.75);
  box-shadow:
    0 4px 32px rgba(99, 102, 241, 0.08),
    0 1px 4px rgba(0, 0, 0, 0.04);
}

.v-theme--light .hero-section__transcription {
  background: rgba(99, 102, 241, 0.04);
}

.v-theme--light .hero-section__transcription-text {
  color: #334155;
}

.v-theme--light .hero-section__trust-item {
  color: #475569;
}

.v-theme--light .hero-section__status-time {
  color: #64748b;
}

/* ─── Responsive ─── */
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
    margin-bottom: 28px;
  }

  .hero-section__trust {
    gap: 10px;
  }

  .hero-section__trust-divider {
    display: none;
  }

  .hero-section__trust-item {
    font-size: 0.75rem;
  }

  .hero-section__preview-inner {
    padding: 18px;
  }

  .hero-section__waveform {
    height: 60px;
    margin-bottom: 18px;
  }

  .hero-section__badge {
    font-size: 0.72rem;
    padding: 5px 14px;
  }
}
</style>
