<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue';
import { mdiStop, mdiMicrophone, mdiWindowMinimize, mdiAccountCircleOutline, mdiCogOutline } from '@mdi/js';

// ─── Версия из GitHub releases ───
const { data: releaseData } = useReleaseDownloads();
const appVersion = computed(() => releaseData.value?.version ?? '');

// ─── Constants ───
const BAR_COUNT = 48;

const DEMO_SENTENCE = 'Hello, this is an example of real-time voice transcription powered by AI';

// ─── State machine: idle → recording → done → pause → repeat ───
type DemoState = 'idle' | 'recording' | 'done';
const state = ref<DemoState>('idle');

// ─── Text animation (character by character, like typing) ───
const animatedText = ref('');
const textFading = ref(false);
let charTimer: ReturnType<typeof setTimeout> | null = null;

function typeNextChar(text: string, index: number) {
  if (index >= text.length) { charTimer = null; return; }
  animatedText.value = text.slice(0, index + 1);
  // Variable delay: longer pause after punctuation, shorter for regular chars
  const ch = text[index];
  let delay = 35; // base typing speed
  if (ch === ',' || ch === '.') delay = 120;
  else if (ch === ' ') delay = 10;
  charTimer = setTimeout(() => typeNextChar(text, index + 1), delay);
}

function startTextAnimation() {
  animatedText.value = '';
  typeNextChar(DEMO_SENTENCE, 0);
}

function stopTextAnimation() {
  if (charTimer) { clearTimeout(charTimer); charTimer = null; }
}

// ─── Button ───
const glowClass = ref<'glow-blue' | 'glow-red' | ''>('');

const buttonClass = computed(() => ({
  'hero-demo__btn--recording': state.value === 'recording',
  [glowClass.value]: !!glowClass.value,
}));

const buttonIcon = computed(() => {
  return state.value === 'recording' ? mdiStop : mdiMicrophone;
});

// ─── Canvas visualizer (ported from AudioVisualizer.vue) ───
const canvasRef = ref<HTMLCanvasElement | null>(null);
const vizContainerRef = ref<HTMLElement | null>(null);
let rafId: number | null = null;
let dpr = 1;
let canvasSize = { width: 0, height: 0 };
let resizeObserver: ResizeObserver | null = null;

// Per-bar random phases (deterministic seed for SSR, randomized on mount)
const barPhases = Array.from({ length: BAR_COUNT }, (_, i) => {
  return ((i * 2654435761) >>> 0) % 628 / 100; // 0..~6.28
});

// Fake frequency data — smoothed per bar
const fakeBars = new Float32Array(BAR_COUNT);
const targetBars = new Float32Array(BAR_COUNT);
let isRecordingViz = false;
let recordingStartTime = 0; // timestamp when recording started

function generateFakeData() {
  const t = performance.now() / 1000;
  for (let i = 0; i < BAR_COUNT; i++) {
    if (isRecordingViz) {
      const elapsed = (performance.now() - recordingStartTime) / 1000;
      const ramp = Math.min(1, elapsed / 1.5);

      // Центр громче, края тише — как в реальном спектре речи
      const center = BAR_COUNT / 2;
      const dist = Math.abs(i - center) / center;
      const centerBoost = Math.pow(1 - dist, 3.0);
      const base = 0.03 + centerBoost * 0.55;
      const wave = Math.sin(t * 1.8 + barPhases[i]) * 0.15
                 + Math.sin(t * 0.9 + barPhases[i] * 2.3) * 0.10
                 + Math.sin(t * 3.0 + barPhases[i] * 0.7) * 0.07
                 + Math.sin(t * 0.4 + barPhases[i] * 1.1) * 0.05;
      // Мягкий потолок — единичные пики могут пробить ~0.75, но большинство ниже
      const raw = (base + wave) * ramp;
      targetBars[i] = Math.max(0, Math.min(1, raw > 0.65 ? 0.65 + (raw - 0.65) * 0.3 : raw));
    } else {
      // Idle: subtle breathing
      targetBars[i] = 0;
    }
  }
  // Smooth towards target (attack/release from AudioVisualizer.vue)
  for (let i = 0; i < BAR_COUNT; i++) {
    const current = fakeBars[i];
    const target = targetBars[i];
    if (target > current) {
      fakeBars[i] = current * 0.85 + target * 0.15; // attack — как в десктопе
    } else {
      fakeBars[i] = current * 0.95 + target * 0.05; // release
    }
  }
}

// Center-weighted mapping — from AudioVisualizer.vue computeVisualBars()
function computeVisualBars(input: Float32Array): number[] {
  const out = Array.from({ length: BAR_COUNT }, () => 0);
  const centerLeft = BAR_COUNT / 2 - 1;
  const centerRight = BAR_COUNT / 2;

  for (let i = 0; i < BAR_COUNT; i++) {
    const v = Math.max(0, Math.min(1, input[i] ?? 0));
    const t = i / (BAR_COUNT - 1);
    const tilt = 0.9 + Math.pow(t, 0.9) * 0.35;
    const shaped = Math.min(1, v * tilt);

    const k = Math.floor(i / 2);
    const pos = i % 2 === 0 ? centerLeft - k : centerRight + k;
    if (pos >= 0 && pos < BAR_COUNT) out[pos] = shaped;
  }
  return out;
}

function clamp(v: number, min: number, max: number) {
  return Math.min(max, Math.max(min, v));
}

// Rounded-top rectangle — from AudioVisualizer.vue
function drawRoundedTopRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  const radius = Math.max(0, Math.min(r, w / 2, h));
  ctx.beginPath();
  ctx.moveTo(x, y + h);
  ctx.lineTo(x, y + radius);
  ctx.quadraticCurveTo(x, y, x + radius, y);
  ctx.lineTo(x + w - radius, y);
  ctx.quadraticCurveTo(x + w, y, x + w, y + radius);
  ctx.lineTo(x + w, y + h);
  ctx.closePath();
}

let gain = 1;

function render() {
  const canvas = canvasRef.value;
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  if (!canvasSize.width || !canvasSize.height) return;

  const { width, height } = canvasSize;
  ctx.save();
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  const maxBarHeight = height * 0.85;
  const baseY = height;
  const base = 0.02;
  const noiseAmp = 0.04;
  const t = performance.now() / 1000;

  let gap = Math.max(1, Math.round(width * 0.004));
  let barW = (width - gap * (BAR_COUNT - 1)) / BAR_COUNT;
  if (barW < 2) { gap = 1; barW = (width - gap * (BAR_COUNT - 1)) / BAR_COUNT; }
  if (barW < 1) { gap = 0; barW = width / BAR_COUNT; }

  const totalWidth = BAR_COUNT * barW + (BAR_COUNT - 1) * gap;
  const offsetX = (width - totalWidth) / 2;

  // В десктопе 0.08/0.55, но там визуализатор за стеклянным фоном (glass-bg 0.9),
  // а тут полосы и текст на одном слое — снижаем, чтобы текст легко читался
  const minAlpha = 0.05;
  const maxAlpha = 0.30;
  const radius = 3;

  generateFakeData();
  const visualBars = computeVisualBars(fakeBars);

  // AGC — from AudioVisualizer.vue
  let maxV = 0;
  for (let i = 0; i < BAR_COUNT; i++) maxV = Math.max(maxV, visualBars[i]);
  const desiredGain = maxV > 0 ? 0.95 / maxV : 1;
  gain = gain * 0.97 + clamp(desiredGain, 0.9, 2.0) * 0.03;

  for (let i = 0; i < BAR_COUNT; i++) {
    const v = (visualBars[i] ?? 0) * gain;
    const boosted = Math.min(1, Math.pow(Math.max(0, v), 0.65) * 1.35);
    const smoothNoise = (Math.sin(t * 2.2 + barPhases[i]) * 0.5 + 0.5) * noiseAmp;
    const withNoise = clamp(base + boosted + smoothNoise, 0, 1);

    const h = withNoise * maxBarHeight;
    if (h <= 1) continue;

    const x = offsetX + i * (barW + gap);
    const y = baseY - h;

    const alpha = minAlpha + withNoise * (maxAlpha - minAlpha);
    const grad = ctx.createLinearGradient(0, baseY, 0, baseY - maxBarHeight);
    grad.addColorStop(0, `rgba(76, 175, 80, 0)`);
    grad.addColorStop(1, `rgba(76, 175, 80, ${alpha})`);

    ctx.fillStyle = grad;
    drawRoundedTopRect(ctx, x, y, barW, h, radius);
    ctx.fill();
  }

  ctx.restore();
}

function vizLoop() {
  render();
  rafId = window.requestAnimationFrame(vizLoop);
}

function updateCanvasSize() {
  const canvas = canvasRef.value;
  const container = vizContainerRef.value;
  if (!canvas || !container) return;
  const rect = container.getBoundingClientRect();
  const w = rect.width || container.clientWidth;
  const h = rect.height || container.clientHeight;
  if (!w || !h) return;
  canvasSize = { width: w, height: h };
  dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(w * dpr));
  canvas.height = Math.max(1, Math.floor(h * dpr));
  canvas.style.width = `${w}px`;
  canvas.style.height = `${h}px`;
}

// ─── Timer management ───
const timers: number[] = [];
function safeTimeout(fn: () => void, ms: number) {
  const id = window.setTimeout(fn, ms);
  timers.push(id);
  return id;
}
function clearAllTimers() {
  timers.forEach(clearTimeout);
  timers.length = 0;
  stopTextAnimation();
}

// ─── IntersectionObserver ───
const containerRef = ref<HTMLElement | null>(null);
const isVisible = ref(false);
let intObserver: IntersectionObserver | null = null;

// ─── Demo cycle ───
let cycleRunning = false;

function runCycle() {
  if (!cycleRunning) return;

  // 1. IDLE — mic, empty window, bars breathe
  state.value = 'idle';
  animatedText.value = '';
  textFading.value = false;
  isRecordingViz = false;
  glowClass.value = '';

  safeTimeout(() => {
    if (!cycleRunning) return;

    // 2. RECORDING — red button, bars active, text appears char-by-char
    state.value = 'recording';
    isRecordingViz = true;
    recordingStartTime = performance.now();
    glowClass.value = 'glow-blue';
    safeTimeout(() => { glowClass.value = ''; }, 1000);

    startTextAnimation();

    // Wait for text to finish (~35ms per char avg) + extra pause
    const textDuration = DEMO_SENTENCE.length * 38 + 800;
    safeTimeout(() => {
      if (!cycleRunning) return;

      // 3. DONE — text ready, button back to idle
      state.value = 'done';
      isRecordingViz = false;
      glowClass.value = 'glow-red';
      safeTimeout(() => { glowClass.value = ''; }, 1000);

      // 4. Pause, then fade text and repeat
      safeTimeout(() => {
        textFading.value = true;

        safeTimeout(() => {
          if (cycleRunning) runCycle();
        }, 700);
      }, 2500);
    }, textDuration);
  }, 2500);
}

function startDemo() {
  if (cycleRunning) return;
  cycleRunning = true;

  // Start canvas visualizer
  updateCanvasSize();
  if (typeof ResizeObserver !== 'undefined' && vizContainerRef.value) {
    resizeObserver = new ResizeObserver(() => updateCanvasSize());
    resizeObserver.observe(vizContainerRef.value);
  }
  vizLoop();

  runCycle();
}

function stopDemo() {
  cycleRunning = false;
  clearAllTimers();
  isRecordingViz = false;
  state.value = 'idle';
  animatedText.value = '';
  textFading.value = false;
  glowClass.value = '';

  // Stop canvas loop
  if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
  if (resizeObserver) { resizeObserver.disconnect(); resizeObserver = null; }
}

watch(isVisible, (visible) => {
  if (visible) startDemo();
  else stopDemo();
});

onMounted(() => {
  intObserver = new IntersectionObserver(
    ([entry]) => { isVisible.value = entry.isIntersecting; },
    { threshold: 0.1 },
  );
  if (containerRef.value) intObserver.observe(containerRef.value);
});

onUnmounted(() => {
  stopDemo();
  if (intObserver) { intObserver.disconnect(); intObserver = null; }
});
</script>

<template>
  <div ref="containerRef" class="hero-demo" role="img" aria-label="Application demo">
    <!-- Canvas visualizer (ported from AudioVisualizer.vue) -->
    <div ref="vizContainerRef" class="hero-demo__visualizer" aria-hidden="true">
      <canvas ref="canvasRef" />
    </div>

    <div class="hero-demo__content">
      <!-- Header -->
      <div class="hero-demo__header">
        <div class="hero-demo__title-row">
          <span class="hero-demo__title">VoicetextAI</span>
          <span v-if="appVersion" class="hero-demo__version">{{ appVersion }}</span>
        </div>
        <div class="hero-demo__header-icons">
          <v-icon :icon="mdiWindowMinimize" size="22" />
          <v-icon :icon="mdiAccountCircleOutline" size="22" />
          <v-icon :icon="mdiCogOutline" size="22" />
        </div>
      </div>

      <!-- Transcription area -->
      <div class="hero-demo__transcription">
        <p
          class="hero-demo__text"
          :class="{
            'hero-demo__text--idle': state === 'idle',
            'hero-demo__text--accent': state === 'recording' || state === 'done',
            'hero-demo__text--fading': textFading,
          }"
        >
          {{ state === 'idle' ? 'Press the button or use hotkey to start recording...' : animatedText
          }}<span v-if="state === 'recording'" class="hero-demo__cursor" />
        </p>
      </div>

      <!-- Record button (styles from RecordingPopover.vue) -->
      <div class="hero-demo__controls">
        <div class="hero-demo__btn" :class="buttonClass">
          <v-icon :icon="buttonIcon" size="28" />
        </div>
      </div>

      <!-- Footer -->
      <div class="hero-demo__footer">
        <span class="hero-demo__hint">Ctrl+Shift+S to start/stop recording</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* ─── Window (glass effect from style.css) ─── */
.hero-demo {
  position: relative;
  z-index: 1;
  border-radius: 16px;
  background: rgba(26, 26, 26, 0.9);
  backdrop-filter: blur(20px);
  -webkit-backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.08);
  overflow: hidden;
  min-height: 330px;
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.45);
}

/* ─── Canvas visualizer ─── */
.hero-demo__visualizer {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: none;
}

.hero-demo__visualizer canvas {
  width: 100%;
  height: 100%;
  display: block;
}

/* ─── Content ─── */
.hero-demo__content {
  position: relative;
  z-index: 1;
  display: flex;
  flex-direction: column;
  padding: 8px 12px;
  min-height: 330px;
}

/* ─── Header ─── */
.hero-demo__header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 8px 4px;
}

.hero-demo__title-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.hero-demo__title {
  font-size: 19px;
  font-weight: 600;
  color: #ffffff;
}

.hero-demo__version {
  font-size: 10px;
  color: rgba(255, 255, 255, 0.35);
}

.hero-demo__header-icons {
  display: flex;
  align-items: center;
  gap: 8px;
  color: #ffffff;
  font-size: 22px;
}

.hero-demo__header-icons span {
  padding: 2px 6px;
  border-radius: 4px;
  opacity: 0.8;
  transition: opacity 0.2s ease;
}

/* ─── Transcription ─── */
.hero-demo__transcription {
  flex: 1;
  display: flex;
  align-items: flex-start;
  padding: 8px;
  min-height: 80px;
}

.hero-demo__text {
  font-size: 18.5px;
  line-height: 1.5;
  color: #a0a0a0;
  word-wrap: break-word;
  overflow-wrap: break-word;
  white-space: pre-wrap;
  margin: 0;
  transition: opacity 0.5s ease, color 0.3s ease;
}

.hero-demo__text--idle {
  color: #a0a0a0;
}

.hero-demo__text--accent {
  color: #4a9eff;
}

.hero-demo__text--fading {
  opacity: 0;
}

.hero-demo__cursor {
  display: inline-block;
  width: 2px;
  height: 1em;
  background: #4a9eff;
  margin-left: 2px;
  vertical-align: text-bottom;
  animation: demoCursorBlink 1s step-end infinite;
}

@keyframes demoCursorBlink {
  0%, 100% { opacity: 1; }
  50% { opacity: 0; }
}

/* ─── Record Button (from RecordingPopover.vue) ─── */
.hero-demo__controls {
  display: flex;
  justify-content: center;
  padding: 8px 0 7px;
}

.hero-demo__btn {
  width: 64px;
  height: 64px;
  border-radius: 50%;
  background: #4a9eff;
  color: #fff;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 28px;
  transition: background 0.3s ease, box-shadow 0.3s ease;
}

.hero-demo__btn--recording {
  background: #f44336;
}

/* Glow pulse (from RecordingPopover.vue) */
.hero-demo__btn.glow-blue {
  animation: demoGlowBlue 1s cubic-bezier(0.2, 0, 0.2, 1) forwards;
}

.hero-demo__btn.glow-red {
  animation: demoGlowRed 1s cubic-bezier(0.2, 0, 0.2, 1) forwards;
}

@keyframes demoGlowBlue {
  0%   { box-shadow: 0 0 0 0 rgba(33, 150, 243, 0.5); }
  30%  { box-shadow: 0 0 16px 10px rgba(33, 150, 243, 0.35); }
  100% { box-shadow: 0 0 0 20px rgba(33, 150, 243, 0); }
}

@keyframes demoGlowRed {
  0%   { box-shadow: 0 0 0 0 rgba(244, 67, 54, 0.5); }
  30%  { box-shadow: 0 0 16px 10px rgba(244, 67, 54, 0.35); }
  100% { box-shadow: 0 0 0 20px rgba(244, 67, 54, 0); }
}

/* ─── Footer ─── */
.hero-demo__footer {
  display: flex;
  justify-content: center;
  padding-top: 4px;
  margin-top: 4px;
}

.hero-demo__hint {
  font-size: 13px;
  color: #a0a0a0;
}

/* ─── Responsive ─── */
@media (max-width: 960px) {
  .hero-demo {
    max-width: 460px;
    margin: 0 auto;
  }
}

@media (max-width: 600px) {
  .hero-demo {
    border-radius: 12px;
    min-height: 280px;
  }

  .hero-demo__content {
    padding: 8px 10px;
    min-height: 280px;
  }

  .hero-demo__title {
    font-size: 16px;
  }

  .hero-demo__text {
    font-size: 15px;
  }

  .hero-demo__btn {
    width: 52px;
    height: 52px;
    font-size: 24px;
  }

  .hero-demo__hint {
    font-size: 11px;
  }
}
</style>
