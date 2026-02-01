<script setup lang="ts">
import { onMounted, onUnmounted, ref, toRefs, watch } from 'vue';
import { useAudioVisualizer } from '../../composables/useAudioVisualizer';

const props = defineProps<{
  active: boolean;
}>();

const { active } = toRefs(props);

const canvasRef = ref<HTMLCanvasElement | null>(null);
const containerRef = ref<HTMLElement | null>(null);

const { bars } = useAudioVisualizer(active, {
  barCount: 48,
  // Чуть спокойнее: вверх реагирует быстро, но без "перекача"
  attackSmoothing: 0.85,
  releaseSmoothing: 0.95,
});

let rafId: number | null = null;
let resizeObserver: ResizeObserver | null = null;
let dpr = 1;
let mo: MutationObserver | null = null;
let unlistenWindowResize: (() => void) | null = null;
let hasRenderErrorLogged = false;
let hasLoggedLoopStart = false;
let lastSizeLogAt = 0;

type Size = { width: number; height: number };
const size = ref<Size>({ width: 0, height: 0 });

const BAR_COUNT = 48;
const barPhases = Array.from({ length: BAR_COUNT }, () => Math.random() * Math.PI * 2);
let gain = 1;

function clamp(v: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, v));
}

function computeVisualBars(input: number[]): number[] {
  // Вход ожидаем как "частота слева → частота справа".
  // Для красивого визуала кладём низкие частоты в центр, высокие — к краям.
  const out = Array.from({ length: BAR_COUNT }, () => 0);
  const centerLeft = BAR_COUNT / 2 - 1; // 23
  const centerRight = BAR_COUNT / 2; // 24

  for (let i = 0; i < BAR_COUNT; i++) {
    const raw = Number(input[i] ?? 0);
    const v = Number.isFinite(raw) ? clamp(raw, 0, 1) : 0;

    // Лёгкая "эквализация": немного усиливаем верх, чтобы справа не было "пусто".
    const t = i / (BAR_COUNT - 1);
    const tilt = 0.9 + Math.pow(t, 0.9) * 0.35; // 0.90..1.25
    const shaped = clamp(v * tilt, 0, 1);

    const k = Math.floor(i / 2);
    const pos = i % 2 === 0 ? centerLeft - k : centerRight + k;
    if (pos >= 0 && pos < BAR_COUNT) {
      out[pos] = shaped;
    }
  }

  return out;
}

function parseColorToRgba(color: string, alpha: number): string {
  const c = color.trim();

  if (c.startsWith('#')) {
    const hex = c.slice(1);
    const full = hex.length === 3
      ? hex.split('').map((x) => x + x).join('')
      : hex;

    if (full.length === 6) {
      const r = parseInt(full.slice(0, 2), 16);
      const g = parseInt(full.slice(2, 4), 16);
      const b = parseInt(full.slice(4, 6), 16);
      return `rgba(${r}, ${g}, ${b}, ${alpha})`;
    }
  }

  if (c.startsWith('rgb(')) {
    return c.replace(/^rgb\(/, 'rgba(').replace(/\)$/, `, ${alpha})`);
  }

  if (c.startsWith('rgba(')) {
    // Переписываем альфу на нужную
    const parts = c.replace(/^rgba\(/, '').replace(/\)$/, '').split(',').map((x) => x.trim());
    if (parts.length >= 3) {
      return `rgba(${parts[0]}, ${parts[1]}, ${parts[2]}, ${alpha})`;
    }
  }

  // fallback - пусть браузер сам парсит, альфу не гарантируем
  return c;
}

function readAccentColor(): string {
  const el = document.documentElement;
  // Для визуализатора используем "успех" — он зелёный и хорошо читается на стекле.
  const success = getComputedStyle(el).getPropertyValue('--color-success').trim();
  return success || '#4caf50';
}

let accentColor = readAccentColor();

function updateCanvasSize() {
  const canvas = canvasRef.value;
  const container = containerRef.value;
  if (!canvas || !container) return;

  const rect = container.getBoundingClientRect();
  const width = rect.width || container.clientWidth || container.offsetWidth;
  const height = rect.height || container.clientHeight || container.offsetHeight;
  if (!width || !height) return;

  size.value = { width, height };

  dpr = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(width * dpr));
  canvas.height = Math.max(1, Math.floor(height * dpr));
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;

  const now = Date.now();
  if (now - lastSizeLogAt > 1500) {
    lastSizeLogAt = now;
    console.log(
      `[AudioVisualizer] canvas size: css=${Math.round(width)}x${Math.round(height)}, px=${canvas.width}x${canvas.height}, dpr=${dpr}`
    );
  }
}

function drawRoundedTopRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number
) {
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

function render() {
  const canvas = canvasRef.value;
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  // Иногда в момент mount размеры ещё не готовы — пробуем пересчитать и рисуем на следующем кадре.
  if (!size.value.width || !size.value.height) {
    updateCanvasSize();
    return;
  }

  const { width, height } = size.value;

  ctx.save();
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  // Почти на всю высоту окна (оставляем микромаржин, чтобы не упираться в пиксель в пиксель)
  const maxBarHeight = height * 0.98;
  const baseY = height;

  // Чуть-чуть "дышим" даже в тишине, чтобы фон не выглядел мёртвым.
  // Важно: без random() — иначе кончики будут дергаться каждый кадр.
  const base = 0.02;
  const noiseAmp = 0.04;
  const t = performance.now() / 1000;

  // Размеры баров
  let gap = Math.max(1, Math.round(width * 0.004)); // ~0.4% ширины
  let barW = (width - gap * (BAR_COUNT - 1)) / BAR_COUNT;
  if (barW < 2) {
    gap = 1;
    barW = (width - gap * (BAR_COUNT - 1)) / BAR_COUNT;
  }
  if (barW < 1) {
    gap = 0;
    barW = width / BAR_COUNT;
  }

  const totalWidth = BAR_COUNT * barW + (BAR_COUNT - 1) * gap;
  const offsetX = (width - totalWidth) / 2;

  const minAlpha = 0.08;
  const maxAlpha = 0.55;
  const radius = 3;

  const visualBars = computeVisualBars(bars.value);

  // AGC: подстраиваем коэффициент так, чтобы пики доходили выше (но без "прыжков").
  let maxV = 0;
  for (let i = 0; i < BAR_COUNT; i++) {
    maxV = Math.max(maxV, visualBars[i] ?? 0);
  }
  const desiredGain = maxV > 0 ? 0.85 / maxV : 1;
  // Медленнее подстраиваем и сильнее ограничиваем, чтобы не было "пампинга"
  gain = gain * 0.96 + clamp(desiredGain, 0.85, 3.2) * 0.04;

  for (let i = 0; i < BAR_COUNT; i++) {
    const v = (visualBars[i] ?? 0) * gain;
    // Делаем "больше" визуально: речь обычно даёт небольшие значения, поэтому усиливаем.
    // pow(<1) поднимает низкие значения, clamp не даёт уйти в 1 постоянно.
    const boosted = Math.min(1, Math.pow(Math.max(0, v), 0.65) * 1.35);
    const smoothNoise =
      (Math.sin(t * 2.2 + barPhases[i]) * 0.5 + 0.5) * noiseAmp; // 0..noiseAmp
    const withNoise = Math.min(1, Math.max(0, base + boosted + smoothNoise));

    const h = withNoise * maxBarHeight;
    if (h <= 1) continue;

    const x = offsetX + i * (barW + gap);
    const y = baseY - h;

    const alpha = minAlpha + withNoise * (maxAlpha - minAlpha);
    const top = parseColorToRgba(accentColor, alpha);
    const bottom = parseColorToRgba(accentColor, 0.0);
    const grad = ctx.createLinearGradient(0, baseY, 0, baseY - maxBarHeight);
    grad.addColorStop(0, bottom);
    grad.addColorStop(1, top);

    ctx.fillStyle = grad;
    drawRoundedTopRect(ctx, x, y, barW, h, radius);
    ctx.fill();
  }

  ctx.restore();
}

function loop() {
  try {
    render();
  } catch (err) {
    // Если что-то пошло не так (например, странный цвет), не "убиваем" цикл навсегда.
    if (!hasRenderErrorLogged) {
      hasRenderErrorLogged = true;
      console.error('[AudioVisualizer] Render error:', err);
    }
  }
  if (!hasLoggedLoopStart) {
    hasLoggedLoopStart = true;
    console.log('[AudioVisualizer] RAF loop запущен');
  }
  rafId = window.requestAnimationFrame(loop);
}

onMounted(() => {
  console.log('[AudioVisualizer] mounted');
  updateCanvasSize();
  // На всякий случай: после первого layout ещё раз пересчитаем размеры.
  window.requestAnimationFrame(() => updateCanvasSize());

  // ResizeObserver не везде доступен (особенно в старом WebKit), поэтому делаем безопасный fallback
  if (typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(() => updateCanvasSize());
    if (containerRef.value) {
      resizeObserver.observe(containerRef.value);
    }
    console.log('[AudioVisualizer] ResizeObserver включён');
  } else {
    const onResize = () => updateCanvasSize();
    window.addEventListener('resize', onResize, { passive: true });
    unlistenWindowResize = () => window.removeEventListener('resize', onResize);
    console.log('[AudioVisualizer] ResizeObserver нет, использую window.resize');
  }

  // Если тема переключилась (меняется class/html), перечитаем акцент
  mo = new MutationObserver(() => {
    accentColor = readAccentColor();
  });
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['class', 'style'] });
  console.log('[AudioVisualizer] MutationObserver включён (слушаю смену темы)');

  loop();
});

onUnmounted(() => {
  if (rafId) {
    cancelAnimationFrame(rafId);
    rafId = null;
  }
  if (resizeObserver) {
    resizeObserver.disconnect();
    resizeObserver = null;
  }
  if (unlistenWindowResize) {
    unlistenWindowResize();
    unlistenWindowResize = null;
  }
  if (mo) {
    mo.disconnect();
    mo = null;
  }
});

watch(
  () => active.value,
  () => {
    // При старте/остановке нам не нужно дергать loop — opacity сделает плавный эффект.
    // Просто обновим цвет на всякий случай.
    accentColor = readAccentColor();
  }
);
</script>

<template>
  <div
    ref="containerRef"
    class="audio-visualizer"
    :class="{ active }"
    aria-hidden="true"
  >
    <canvas ref="canvasRef" />
  </div>
</template>

<style scoped>
.audio-visualizer {
  position: absolute;
  inset: 0;
  z-index: 0;
  pointer-events: none;
  opacity: 0;
  transition: opacity 0.35s ease;
}

.audio-visualizer.active {
  opacity: 1;
}

canvas {
  width: 100%;
  height: 100%;
  display: block;
}
</style>

