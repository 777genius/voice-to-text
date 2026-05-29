<script setup lang="ts">
import type { NeatConfig, NeatController } from '@firecms/neat';

const canvasRef = ref<HTMLCanvasElement | null>(null);
const { isDark } = useBrowserTheme();
const isLive = ref(false);

let gradient: NeatController | null = null;
let observer: IntersectionObserver | null = null;
let motionQuery: MediaQueryList | null = null;
let mobileQuery: MediaQueryList | null = null;
let isVisible = false;
let isInitializing = false;
let initToken = 0;
let revealTimer: number | null = null;

const voiceHeroConfig: NeatConfig = {
  colors: [
    { color: '#07111f', enabled: true },
    { color: '#0f3f66', enabled: true },
    { color: '#4a9eff', enabled: true },
    { color: '#22c55e', enabled: true },
    { color: '#6366f1', enabled: true },
    { color: '#ec4899', enabled: false },
  ],
  speed: 4.2,
  horizontalPressure: 6,
  verticalPressure: 3,
  waveFrequencyX: 0,
  waveFrequencyY: 0,
  waveAmplitude: 0,
  shadows: 4,
  highlights: 1,
  colorBrightness: 1.65,
  colorSaturation: 1.75,
  wireframe: false,
  colorBlending: 8,
  backgroundColor: '#050816',
  backgroundAlpha: 0,
  grainScale: 7,
  grainSparsity: 0,
  grainIntensity: 0.08,
  grainSpeed: 0,
  resolution: 0.32,
  yOffset: 110,
  flowDistortionA: 0.42,
  flowDistortionB: 8,
  flowScale: 3.1,
  flowEase: 0.36,
  enableProceduralTexture: false,
  textureVoidLikelihood: 0.06,
  textureVoidWidthMin: 10,
  textureVoidWidthMax: 500,
  textureBandDensity: 0.8,
  textureColorBlending: 0.06,
  textureSeed: 417,
  textureEase: 0.38,
  proceduralBackgroundColor: '#4a9eff',
  textureShapeTriangles: 16,
  textureShapeCircles: 16,
  textureShapeBars: 14,
  textureShapeSquiggles: 8,
  yOffsetWaveMultiplier: 4.2,
  yOffsetColorMultiplier: 4.6,
  yOffsetFlowMultiplier: 5,
  flowEnabled: true,
};

function supportsWebGl() {
  try {
    const canvas = document.createElement('canvas');
    const context = canvas.getContext('webgl2') || canvas.getContext('webgl');
    const isSupported = Boolean(context);
    context?.getExtension('WEBGL_lose_context')?.loseContext();
    return isSupported;
  } catch {
    return false;
  }
}

function canRenderGradient() {
  return Boolean(
    canvasRef.value &&
      isVisible &&
      !motionQuery?.matches &&
      !mobileQuery?.matches,
  );
}

function shouldStartGradient() {
  return canRenderGradient() && supportsWebGl();
}

function destroyGradient() {
  initToken += 1;

  if (revealTimer !== null) {
    window.clearTimeout(revealTimer);
    revealTimer = null;
  }

  gradient?.destroy();
  gradient = null;
  isLive.value = false;
}

async function initGradient() {
  if (gradient || isInitializing || !shouldStartGradient()) return;

  const token = initToken;
  isInitializing = true;

  try {
    const { NeatGradient } = await import('@firecms/neat');

    if (token !== initToken || !canvasRef.value || !canRenderGradient()) return;

    gradient = new NeatGradient({
      ref: canvasRef.value,
      ...voiceHeroConfig,
      resolution: window.devicePixelRatio > 1 ? 0.24 : 0.34,
    });

    revealTimer = window.setTimeout(() => {
      revealTimer = null;
      if (token === initToken && gradient && canRenderGradient()) {
        isLive.value = true;
      }
    }, 180);
  } catch (error) {
    console.warn('Hero canvas background is unavailable', error);
    destroyGradient();
  } finally {
    isInitializing = false;
  }
}

function syncGradient() {
  if (gradient && canRenderGradient()) return;

  if (shouldStartGradient()) {
    void initGradient();
    return;
  }

  destroyGradient();
}

onMounted(() => {
  motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
  mobileQuery = window.matchMedia('(max-width: 480px)');
  motionQuery.addEventListener('change', syncGradient);
  mobileQuery.addEventListener('change', syncGradient);

  observer = new IntersectionObserver(
    ([entry]) => {
      isVisible = Boolean(entry?.isIntersecting);
      syncGradient();
    },
    { rootMargin: '160px 0px', threshold: 0.01 },
  );

  const target = canvasRef.value?.closest('.hero-section');
  if (target) observer.observe(target);
});

onBeforeUnmount(() => {
  observer?.disconnect();
  motionQuery?.removeEventListener('change', syncGradient);
  mobileQuery?.removeEventListener('change', syncGradient);
  destroyGradient();
});
</script>

<template>
  <div
    class="hero-canvas-background"
    :class="{
      'hero-canvas-background--live': isLive,
      'hero-canvas-background--light': !isDark,
    }"
    aria-hidden="true"
  >
    <canvas ref="canvasRef" class="hero-canvas-background__canvas" />
  </div>
</template>

<style scoped>
.hero-canvas-background {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  overflow: hidden;
  background:
    radial-gradient(circle at 74% 30%, rgba(74, 158, 255, 0.17), transparent 34%),
    radial-gradient(circle at 82% 72%, rgba(34, 197, 94, 0.12), transparent 34%),
    linear-gradient(180deg, rgba(5, 8, 22, 0.15), rgba(5, 8, 22, 0.7));
}

.hero-canvas-background::before,
.hero-canvas-background::after {
  content: "";
  position: absolute;
  inset: 0;
  pointer-events: none;
}

.hero-canvas-background::before {
  z-index: 2;
  background:
    radial-gradient(circle at 24% 38%, rgba(5, 8, 22, 0.68), rgba(5, 8, 22, 0.22) 36%, transparent 64%),
    linear-gradient(90deg, rgba(5, 8, 22, 0.46) 0%, rgba(5, 8, 22, 0.18) 44%, rgba(5, 8, 22, 0.08) 66%, rgba(5, 8, 22, 0.36) 100%);
}

.hero-canvas-background::after {
  z-index: 3;
  background:
    linear-gradient(180deg, rgba(5, 8, 22, 0.94) 0%, rgba(5, 8, 22, 0.5) 17%, rgba(5, 8, 22, 0.05) 46%, rgba(5, 8, 22, 0.78) 100%),
    radial-gradient(circle at 66% 45%, transparent 0 28%, rgba(5, 8, 22, 0.28) 70%, rgba(5, 8, 22, 0.58) 100%);
}

.hero-canvas-background__canvas {
  position: absolute;
  inset: 0;
  z-index: 1;
  width: 100%;
  height: 100%;
  opacity: 0;
  filter: blur(3px) saturate(1.06) brightness(0.9) contrast(1.08);
  transform: scale(1.065);
  transition: opacity 1.65s cubic-bezier(0.22, 0.72, 0.2, 1);
}

.hero-canvas-background--live .hero-canvas-background__canvas {
  opacity: 0.36;
}

.v-theme--light .hero-canvas-background,
.hero-canvas-background--light {
  background:
    radial-gradient(circle at 74% 30%, rgba(74, 158, 255, 0.24), transparent 36%),
    radial-gradient(circle at 82% 72%, rgba(34, 197, 94, 0.14), transparent 34%),
    radial-gradient(circle at 58% 18%, rgba(99, 102, 241, 0.18), transparent 30%),
    linear-gradient(180deg, rgba(248, 251, 255, 0.8), rgba(255, 255, 255, 0.98));
}

.v-theme--light .hero-canvas-background::before,
.hero-canvas-background--light::before {
  background:
    radial-gradient(circle at 24% 38%, rgba(255, 255, 255, 0.62), rgba(255, 255, 255, 0.28) 36%, transparent 64%),
    linear-gradient(90deg, rgba(255, 255, 255, 0.44) 0%, rgba(255, 255, 255, 0.16) 44%, rgba(255, 255, 255, 0.04) 66%, rgba(255, 255, 255, 0.32) 100%);
}

.v-theme--light .hero-canvas-background::after,
.hero-canvas-background--light::after {
  background:
    linear-gradient(180deg, rgba(248, 251, 255, 0.72) 0%, rgba(248, 251, 255, 0.42) 20%, rgba(255, 255, 255, 0.12) 48%, rgba(255, 255, 255, 0.68) 100%),
    radial-gradient(circle at 66% 45%, rgba(255, 255, 255, 0) 0 28%, rgba(255, 255, 255, 0.26) 70%, rgba(255, 255, 255, 0.46) 100%);
}

.v-theme--light .hero-canvas-background--live .hero-canvas-background__canvas,
.hero-canvas-background--light.hero-canvas-background--live .hero-canvas-background__canvas {
  opacity: 0.34;
  filter: blur(2.6px) saturate(1.55) brightness(1.35) contrast(0.9);
  mix-blend-mode: multiply;
}

.hero-canvas-background :deep(a[data-n]) {
  display: none !important;
  visibility: hidden !important;
  pointer-events: none !important;
}

@media (max-width: 480px), (prefers-reduced-motion: reduce) {
  .hero-canvas-background__canvas {
    display: none;
  }
}
</style>
