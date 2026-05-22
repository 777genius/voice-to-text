<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { mdiAccountCircleOutline, mdiCogOutline, mdiWindowMinimize } from '@mdi/js';

const MINI_WINDOW_WIDTH = 236;
const MINI_WINDOW_HEIGHT = 38;
const BAR_COUNT = 32;
const HOTKEY_PROMPT = 'Press Cmd/Ctrl+Shift+X';
const TYPE_DELAY_MS = 42;

const { t } = useI18n();

type DemoState = 'prompt' | 'recording' | 'done';

const containerRef = ref<HTMLElement | null>(null);
const canvasRef = ref<HTMLCanvasElement | null>(null);
const visualizerRef = ref<HTMLElement | null>(null);
const transcriptionTextRef = ref<HTMLElement | null>(null);

const state = ref<DemoState>('prompt');
const animatedText = ref('');
const textFading = ref(false);
const isVisible = ref(false);
const isTextOverflowing = ref(false);
const recordingImpulse = ref(false);

const demoText = computed(() => t('hero.transcription.sample'));
const visualizerActive = computed(() => state.value === 'recording');
const statusDotClass = computed(() => ({
  recording: state.value === 'recording' || state.value === 'done',
}));
const displayText = computed(() =>
  state.value === 'prompt' ? HOTKEY_PROMPT : animatedText.value || t('hero.status.recording')
);

let rafId: number | null = null;
let dpr = 1;
let canvasSize = { width: MINI_WINDOW_WIDTH, height: MINI_WINDOW_HEIGHT };
let resizeObserver: ResizeObserver | null = null;
let intersectionObserver: IntersectionObserver | null = null;
let charTimer: number | null = null;
let alignRaf: number | null = null;
let cycleRunning = false;

const timers: number[] = [];
const bars = new Float32Array(BAR_COUNT);
const targetBars = new Float32Array(BAR_COUNT);
const barPhases = Array.from({ length: BAR_COUNT }, (_, i) => {
  return (((i + 1) * 2654435761) >>> 0) % 628 / 100;
});
let recordingStartTime = 0;
let gain = 1;

function safeTimeout(fn: () => void, ms: number) {
  const id = window.setTimeout(fn, ms);
  timers.push(id);
  return id;
}

function clearTimers() {
  timers.forEach((id) => window.clearTimeout(id));
  timers.length = 0;
  if (charTimer !== null) {
    window.clearTimeout(charTimer);
    charTimer = null;
  }
}

function triggerRecordingImpulse() {
  recordingImpulse.value = false;
  void nextTick(() => {
    if (!cycleRunning) return;

    recordingImpulse.value = true;
    safeTimeout(() => {
      recordingImpulse.value = false;
    }, 980);
  });
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function typeNextChar(text: string, index: number) {
  if (!cycleRunning || index >= text.length) {
    charTimer = null;
    return;
  }

  animatedText.value = text.slice(0, index + 1);
  const ch = text[index];
  const delay = ch === ',' || ch === '.' ? 120 : ch === ' ' ? 14 : TYPE_DELAY_MS;
  charTimer = window.setTimeout(() => typeNextChar(text, index + 1), delay);
}

function alignTextToEnd() {
  if (alignRaf !== null) {
    window.cancelAnimationFrame(alignRaf);
  }

  alignRaf = window.requestAnimationFrame(() => {
    alignRaf = null;
    const el = transcriptionTextRef.value;
    if (!el) return;

    const shouldShowTail = state.value !== 'prompt' && Boolean(displayText.value);
    const maxScroll = Math.max(0, el.scrollWidth - el.clientWidth);
    isTextOverflowing.value = shouldShowTail && maxScroll > 1;
    el.scrollLeft = shouldShowTail ? maxScroll : 0;
  });
}

function computeVisualBars(input: Float32Array): number[] {
  const out = Array.from({ length: BAR_COUNT }, () => 0);
  const centerLeft = BAR_COUNT / 2 - 1;
  const centerRight = BAR_COUNT / 2;

  for (let i = 0; i < BAR_COUNT; i++) {
    const raw = clamp(input[i] ?? 0, 0, 1);
    const t = i / (BAR_COUNT - 1);
    const shaped = clamp(raw * (0.9 + Math.pow(t, 0.9) * 0.35), 0, 1);
    const k = Math.floor(i / 2);
    const pos = i % 2 === 0 ? centerLeft - k : centerRight + k;
    if (pos >= 0 && pos < BAR_COUNT) out[pos] = shaped;
  }

  return out;
}

function generateFakeBars() {
  const now = performance.now();
  const t = now / 1000;

  for (let i = 0; i < BAR_COUNT; i++) {
    if (visualizerActive.value) {
      const elapsed = (now - recordingStartTime) / 1000;
      const ramp = Math.min(1, elapsed / 1.2);
      const center = BAR_COUNT / 2;
      const dist = Math.abs(i - center) / center;
      const centerBoost = Math.pow(1 - dist, 2.6);
      const base = 0.03 + centerBoost * 0.58;
      const wave =
        Math.sin(t * 2.0 + barPhases[i]) * 0.14 +
        Math.sin(t * 0.85 + barPhases[i] * 2.1) * 0.11 +
        Math.sin(t * 3.2 + barPhases[i] * 0.7) * 0.06;
      const raw = (base + wave) * ramp;
      targetBars[i] = clamp(raw > 0.65 ? 0.65 + (raw - 0.65) * 0.3 : raw, 0, 1);
    } else {
      targetBars[i] = 0;
    }
  }

  for (let i = 0; i < BAR_COUNT; i++) {
    const current = bars[i];
    const target = targetBars[i];
    bars[i] = target > current
      ? current * 0.85 + target * 0.15
      : current * 0.95 + target * 0.05;
  }
}

function drawRoundedTopRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
) {
  const r = Math.max(0, Math.min(radius, width / 2, height));
  ctx.beginPath();
  ctx.moveTo(x, y + height);
  ctx.lineTo(x, y + r);
  ctx.quadraticCurveTo(x, y, x + r, y);
  ctx.lineTo(x + width - r, y);
  ctx.quadraticCurveTo(x + width, y, x + width, y + r);
  ctx.lineTo(x + width, y + height);
  ctx.closePath();
}

function updateCanvasSize() {
  const canvas = canvasRef.value;
  const container = visualizerRef.value;
  if (!canvas || !container) return;

  const width = container.clientWidth || container.offsetWidth || MINI_WINDOW_WIDTH;
  const height = container.clientHeight || container.offsetHeight || MINI_WINDOW_HEIGHT;
  canvasSize = { width, height };
  dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(width * dpr));
  canvas.height = Math.max(1, Math.floor(height * dpr));
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;
}

function renderVisualizer() {
  const canvas = canvasRef.value;
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  const { width, height } = canvasSize;
  ctx.save();
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  generateFakeBars();
  const visualBars = computeVisualBars(bars);
  const baseY = height;
  const maxBarHeight = height * 0.98;
  const t = performance.now() / 1000;
  let gap = Math.max(1, Math.round(width * 0.004));
  let barWidth = (width - gap * (BAR_COUNT - 1)) / BAR_COUNT;
  if (barWidth < 2) {
    gap = 1;
    barWidth = (width - gap * (BAR_COUNT - 1)) / BAR_COUNT;
  }

  const totalWidth = BAR_COUNT * barWidth + (BAR_COUNT - 1) * gap;
  const offsetX = (width - totalWidth) / 2;

  let maxValue = 0;
  for (let i = 0; i < BAR_COUNT; i++) {
    maxValue = Math.max(maxValue, visualBars[i] ?? 0);
  }
  const desiredGain = maxValue > 0 ? 0.85 / maxValue : 1;
  gain = gain * 0.96 + clamp(desiredGain, 0.85, 3.2) * 0.04;

  for (let i = 0; i < BAR_COUNT; i++) {
    const v = (visualBars[i] ?? 0) * gain;
    const boosted = Math.min(1, Math.pow(Math.max(0, v), 0.65) * 1.35);
    const smoothNoise = (Math.sin(t * 2.2 + barPhases[i]) * 0.5 + 0.5) * 0.025;
    const withNoise = clamp(0.015 + boosted + smoothNoise, 0, 1);
    const barHeight = withNoise * maxBarHeight;
    if (barHeight <= 1) continue;

    const x = offsetX + i * (barWidth + gap);
    const y = baseY - barHeight;
    const alpha = 0.04 + withNoise * 0.3;
    const gradient = ctx.createLinearGradient(0, baseY, 0, baseY - maxBarHeight);
    gradient.addColorStop(0, 'rgba(34, 197, 94, 0)');
    gradient.addColorStop(1, `rgba(34, 197, 94, ${alpha})`);

    ctx.fillStyle = gradient;
    drawRoundedTopRect(ctx, x, y, barWidth, barHeight, 2);
    ctx.fill();
  }

  ctx.restore();
}

function visualizerLoop() {
  renderVisualizer();
  rafId = window.requestAnimationFrame(visualizerLoop);
}

function runCycle() {
  if (!cycleRunning) return;

  state.value = 'prompt';
  animatedText.value = '';
  textFading.value = false;

  safeTimeout(() => {
    if (!cycleRunning) return;

    state.value = 'recording';
    recordingStartTime = performance.now();
    triggerRecordingImpulse();
    typeNextChar(demoText.value, 0);

    const textDuration = demoText.value.length * TYPE_DELAY_MS + 900;
    safeTimeout(() => {
      if (!cycleRunning) return;

      state.value = 'done';
      safeTimeout(() => {
        textFading.value = true;
        safeTimeout(() => {
          if (cycleRunning) runCycle();
        }, 650);
      }, 2300);
    }, textDuration);
  }, 850);
}

function startDemo() {
  if (cycleRunning) return;
  cycleRunning = true;

  updateCanvasSize();
  if (typeof ResizeObserver !== 'undefined' && visualizerRef.value) {
    resizeObserver = new ResizeObserver(() => updateCanvasSize());
    resizeObserver.observe(visualizerRef.value);
  }

  visualizerLoop();
  runCycle();
}

function stopDemo() {
  cycleRunning = false;
  clearTimers();
  state.value = 'prompt';
  animatedText.value = '';
  textFading.value = false;
  isTextOverflowing.value = false;
  recordingImpulse.value = false;

  if (rafId !== null) {
    window.cancelAnimationFrame(rafId);
    rafId = null;
  }
  if (resizeObserver) {
    resizeObserver.disconnect();
    resizeObserver = null;
  }
}

watch(isVisible, (visible) => {
  if (visible) startDemo();
  else stopDemo();
});

watch([displayText, state], () => {
  void nextTick(alignTextToEnd);
});

onMounted(() => {
  intersectionObserver = new IntersectionObserver(
    ([entry]) => {
      isVisible.value = Boolean(entry?.isIntersecting);
    },
    { threshold: 0.1 },
  );

  if (containerRef.value) {
    intersectionObserver.observe(containerRef.value);
  }
});

onUnmounted(() => {
  stopDemo();
  if (alignRaf !== null) {
    window.cancelAnimationFrame(alignRaf);
    alignRaf = null;
  }
  if (intersectionObserver) {
    intersectionObserver.disconnect();
    intersectionObserver = null;
  }
});
</script>

<template>
  <div ref="containerRef" class="hero-mini-demo" role="img" aria-label="Mini recording window demo">
    <div class="hero-mini-demo__stage">
      <div
        class="hero-mini-demo__window-frame"
        :class="{
          recording: state === 'recording',
          'recording-impulse': recordingImpulse,
        }"
      >
        <div
          class="hero-mini-demo__window"
          :class="{
            recording: state === 'recording',
            'recording-impulse': recordingImpulse,
          }"
          :style="{ width: `${MINI_WINDOW_WIDTH}px`, height: `${MINI_WINDOW_HEIGHT}px` }"
        >
          <div ref="visualizerRef" class="hero-mini-demo__visualizer" aria-hidden="true">
            <canvas ref="canvasRef" />
          </div>

          <div class="hero-mini-demo__content">
            <span class="hero-mini-demo__status-dot" :class="statusDotClass" />

            <div
              ref="transcriptionTextRef"
              class="hero-mini-demo__text"
              :class="{
                recording: state !== 'prompt',
                prompt: state === 'prompt',
                fading: textFading,
                overflowing: isTextOverflowing,
              }"
              :title="displayText"
            >
              <span class="hero-mini-demo__text-inner">{{ displayText }}</span>
            </div>

            <div class="hero-mini-demo__actions">
              <button class="hero-mini-demo__icon-button" type="button" aria-label="Profile">
                <v-icon :icon="mdiAccountCircleOutline" size="13" />
              </button>
              <button class="hero-mini-demo__icon-button" type="button" aria-label="Minimize">
                <v-icon :icon="mdiWindowMinimize" size="13" />
              </button>
              <button class="hero-mini-demo__icon-button" type="button" aria-label="Settings">
                <v-icon :icon="mdiCogOutline" size="13" />
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.hero-mini-demo {
  position: relative;
  z-index: 1;
  min-height: 330px;
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: visible;
}

.hero-mini-demo__stage {
  position: relative;
  width: 100%;
  min-height: 180px;
  display: flex;
  align-items: center;
  justify-content: center;
  perspective: 980px;
  perspective-origin: 52% 42%;
}

.hero-mini-demo__window-frame {
  --hero-mini-scale: 1.9;
  position: relative;
  width: calc(236px * var(--hero-mini-scale));
  height: calc(38px * var(--hero-mini-scale));
  display: flex;
  align-items: center;
  justify-content: center;
  transform-style: preserve-3d;
  will-change: transform, filter;
  filter:
    drop-shadow(0 32px 42px rgba(0, 0, 0, 0.38))
    drop-shadow(0 0 42px rgba(74, 158, 255, 0.12));
  transition: filter 0.34s ease;
  animation: heroMiniDrift3d 8.5s ease-in-out infinite;
}

.hero-mini-demo__window-frame::before,
.hero-mini-demo__window-frame::after {
  content: "";
  position: absolute;
  pointer-events: none;
}

.hero-mini-demo__window-frame::before {
  inset: 14px 24px -18px;
  border-radius: 16px;
  background:
    radial-gradient(ellipse at 42% 45%, rgba(74, 158, 255, 0.24), transparent 62%),
    radial-gradient(ellipse at 72% 55%, rgba(34, 197, 94, 0.16), transparent 58%);
  filter: blur(18px);
  opacity: 0.7;
  transform: translateZ(-56px);
}

.hero-mini-demo__window-frame::after {
  inset: -2px;
  border: 1px solid rgba(34, 197, 94, 0);
  border-radius: 16px;
  opacity: 0;
  transform: translateZ(34px) scale(0.98);
  box-shadow:
    0 0 0 0 rgba(34, 197, 94, 0),
    0 0 0 0 rgba(74, 158, 255, 0);
}

.hero-mini-demo__window-frame.recording {
  filter:
    drop-shadow(0 34px 46px rgba(0, 0, 0, 0.4))
    drop-shadow(0 0 58px rgba(74, 158, 255, 0.16))
    drop-shadow(0 0 38px rgba(34, 197, 94, 0.16));
}

.hero-mini-demo__window-frame.recording-impulse::after {
  animation: heroMiniRecordingRing 980ms cubic-bezier(0.16, 1, 0.3, 1) both;
}

.hero-mini-demo__window {
  position: relative;
  flex: 0 0 auto;
  transform: translateZ(18px) scale(var(--hero-mini-scale));
  transform-origin: center;
  transform-style: preserve-3d;
  backface-visibility: hidden;
  border-radius: 7px;
  background: rgba(26, 26, 26, 0.9);
  backdrop-filter: blur(20px);
  -webkit-backdrop-filter: blur(20px);
  border: 1px solid rgba(255, 255, 255, 0.08);
  box-shadow:
    0 0 0 1px rgba(255, 255, 255, 0.06) inset,
    0 12px 28px rgba(0, 0, 0, 0.22);
  overflow: hidden;
  box-sizing: border-box;
  transition:
    border-color 0.28s ease,
    box-shadow 0.28s ease;
}

.hero-mini-demo__window.recording {
  border-color: rgba(34, 197, 94, 0.22);
  box-shadow:
    0 0 0 1px rgba(34, 197, 94, 0.16) inset,
    0 0 20px rgba(34, 197, 94, 0.1),
    0 12px 30px rgba(0, 0, 0, 0.24);
}

.hero-mini-demo__window.recording-impulse {
  animation: heroMiniRecordingKick 820ms cubic-bezier(0.16, 1, 0.3, 1) both;
}

.hero-mini-demo__window::before {
  content: "";
  position: absolute;
  inset: 0;
  z-index: 2;
  border-radius: inherit;
  background:
    linear-gradient(112deg, rgba(255, 255, 255, 0.15), transparent 25%, transparent 68%, rgba(74, 158, 255, 0.13)),
    linear-gradient(180deg, rgba(255, 255, 255, 0.08), transparent 42%);
  opacity: 0.42;
  pointer-events: none;
  mix-blend-mode: screen;
}

.hero-mini-demo__visualizer {
  position: absolute;
  inset: 1px;
  z-index: 0;
  border-radius: 7px;
  overflow: hidden;
  pointer-events: none;
}

.hero-mini-demo__visualizer canvas {
  width: 100%;
  height: 100%;
  display: block;
}

.hero-mini-demo__content {
  position: relative;
  z-index: 1;
  width: 100%;
  height: 100%;
  box-sizing: border-box;
  display: grid;
  grid-template-columns: 8px minmax(0, 1fr) auto;
  align-items: center;
  gap: 5px;
  padding: 2px 5px 2px 7px;
  cursor: default;
  user-select: none;
}

.hero-mini-demo__status-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #a0a0a0;
  opacity: 0.7;
}

.hero-mini-demo__status-dot.recording {
  background: #22c55e;
  opacity: 1;
  animation: heroMiniStatusPulse 1.4s ease-in-out infinite;
}

.hero-mini-demo__text {
  min-width: 0;
  color: #a0a0a0;
  font-size: 12.5px;
  line-height: 1.1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: clip;
  direction: ltr;
  unicode-bidi: plaintext;
  text-align: left;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.35);
  transition: color 0.25s ease, opacity 0.45s ease;
}

.hero-mini-demo__text.recording {
  color: #4a9eff;
}

.hero-mini-demo__text.prompt {
  font-size: 11px;
  color: #a0a0a0;
  opacity: 0.78;
}

.hero-mini-demo__text.fading {
  opacity: 0.35;
}

.hero-mini-demo__text.overflowing:not(.prompt) {
  -webkit-mask-image: linear-gradient(to right, transparent 0, #000 14px, #000 100%);
  mask-image: linear-gradient(to right, transparent 0, #000 14px, #000 100%);
}

.hero-mini-demo__text-inner {
  display: inline-block;
  min-width: max-content;
  direction: ltr;
  unicode-bidi: plaintext;
  white-space: nowrap;
}

.hero-mini-demo__actions {
  display: inline-flex;
  align-items: center;
  gap: 1px;
}

.hero-mini-demo__icon-button {
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: 4px;
  background: transparent;
  color: #a0a0a0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  line-height: 1;
  cursor: default;
}

@keyframes heroMiniStatusPulse {
  0%, 100% {
    transform: scale(0.9);
  }
  50% {
    transform: scale(1.12);
  }
}

@keyframes heroMiniRecordingKick {
  0% {
    transform: translateZ(18px) scale(var(--hero-mini-scale));
  }
  28% {
    transform: translateZ(34px) scale(calc(var(--hero-mini-scale) * 1.085));
  }
  58% {
    transform: translateZ(22px) scale(calc(var(--hero-mini-scale) * 0.992));
  }
  100% {
    transform: translateZ(18px) scale(var(--hero-mini-scale));
  }
}

@keyframes heroMiniRecordingRing {
  0% {
    opacity: 0;
    border-color: rgba(34, 197, 94, 0);
    transform: translateZ(34px) scale(0.98);
    box-shadow:
      0 0 0 0 rgba(34, 197, 94, 0),
      0 0 0 0 rgba(74, 158, 255, 0);
  }
  18% {
    opacity: 0.96;
    border-color: rgba(34, 197, 94, 0.7);
    box-shadow:
      0 0 20px 2px rgba(34, 197, 94, 0.34),
      0 0 0 5px rgba(74, 158, 255, 0.18);
  }
  100% {
    opacity: 0;
    border-color: rgba(34, 197, 94, 0);
    transform: translateZ(34px) scale(1.24);
    box-shadow:
      0 0 32px 4px rgba(34, 197, 94, 0),
      0 0 0 30px rgba(74, 158, 255, 0);
  }
}

@keyframes heroMiniDrift3d {
  0%, 100% {
    transform: translate3d(0, 0, 0) rotateX(3deg) rotateY(-7deg) rotateZ(-0.4deg);
  }
  33% {
    transform: translate3d(0, -9px, 26px) rotateX(6deg) rotateY(6deg) rotateZ(0.6deg);
  }
  66% {
    transform: translate3d(0, -4px, 10px) rotateX(1deg) rotateY(9deg) rotateZ(-0.3deg);
  }
}

@media (max-width: 1180px) {
  .hero-mini-demo__window-frame {
    --hero-mini-scale: 1.7;
  }
}

@media (max-width: 1020px) {
  .hero-mini-demo__window-frame {
    --hero-mini-scale: 1.55;
  }
}

@media (prefers-reduced-motion: reduce) {
  .hero-mini-demo__window-frame {
    animation: none;
    transform: rotateX(3deg) rotateY(-6deg);
  }
}
</style>
