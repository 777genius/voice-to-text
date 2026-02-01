import { ref, watch, onUnmounted, type Ref } from 'vue';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { isTauriAvailable } from '../utils/tauri';

/**
 * Абстракция источника данных для визуализатора.
 * Сейчас используем Tauri event `audio:spectrum`, позже можно добавить WebAudio реализацию.
 */
export interface AudioVisualizerSource {
  start(onBars: (bars: number[]) => void): Promise<void>;
  stop(): void;
}

type AudioSpectrumPayload = {
  bars: number[];
};

class TauriAudioSpectrumSource implements AudioVisualizerSource {
  private unlisten: UnlistenFn | null = null;
  private receivedCount = 0;
  private lastLogAt = 0;

  async start(onBars: (bars: number[]) => void): Promise<void> {
    if (!isTauriAvailable()) return;
    if (this.unlisten) return;

    console.log('[AudioVisualizer] Подписываюсь на событие audio:spectrum (Tauri)');
    this.unlisten = await listen<AudioSpectrumPayload>('audio:spectrum', (event) => {
      const bars = Array.isArray(event.payload?.bars) ? event.payload.bars : [];
      this.receivedCount += 1;

      // Логируем не чаще раза в секунду, чтобы не спамить
      const now = Date.now();
      if (now - this.lastLogAt > 1000) {
        this.lastLogAt = now;
        const len = bars.length;
        let avg = 0;
        if (len) {
          for (let i = 0; i < len; i++) avg += Number(bars[i] ?? 0);
          avg /= len;
        }
        console.log(
          `[AudioVisualizer] audio:spectrum получен (#${this.receivedCount}), bars=${len}, avg=${avg.toFixed(3)}`
        );
      }

      onBars(bars);
    });
  }

  stop(): void {
    if (!this.unlisten) return;
    console.log('[AudioVisualizer] Отписываюсь от audio:spectrum (Tauri)');
    this.unlisten();
    this.unlisten = null;
    this.receivedCount = 0;
    this.lastLogAt = 0;
  }
}

function createDefaultSource(): AudioVisualizerSource {
  return new TauriAudioSpectrumSource();
}

/**
 * Главный composable для визуализатора.
 *
 * - `active` управляет подпиской на данные (start/stop)
 * - `bars` всегда имеет фиксированную длину (по умолчанию 48)
 */
export function useAudioVisualizer(
  active: Ref<boolean>,
  opts?: {
    barCount?: number;
    source?: AudioVisualizerSource;
    smoothing?: number; // 0..1, ближе к 1 = плавнее (fallback)
    attackSmoothing?: number; // 0..1, меньше = быстрее растёт
    releaseSmoothing?: number; // 0..1, больше = плавнее падает
  }
) {
  const barCount = opts?.barCount ?? 48;
  const source = opts?.source ?? createDefaultSource();
  const smoothing = opts?.smoothing ?? 0.8;
  const attackSmoothing =
    opts?.attackSmoothing ?? Math.min(0.75, Math.max(0.0, smoothing - 0.15));
  const releaseSmoothing =
    opts?.releaseSmoothing ?? Math.min(0.98, Math.max(0.0, smoothing + 0.1));

  const bars = ref<number[]>(Array.from({ length: barCount }, () => 0));
  let applyCount = 0;
  let lastApplyLogAt = 0;

  function applyBars(next: number[]) {
    // Гарантируем размер и делаем лёгкое сглаживание, чтобы на canvas не "дёргалось"
    const out = bars.value.slice();
    for (let i = 0; i < barCount; i++) {
      const v = Number(next[i] ?? 0);
      const clamped = Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0;
      const prev = out[i] ?? 0;
      const s = clamped > prev ? attackSmoothing : releaseSmoothing;
      out[i] = prev * s + clamped * (1 - s);
    }
    bars.value = out;

    applyCount += 1;
    const now = Date.now();
    if (now - lastApplyLogAt > 1500) {
      lastApplyLogAt = now;
      // Примерная "энергия" (среднее значение баров)
      let avg = 0;
      for (let i = 0; i < out.length; i++) avg += out[i];
      avg /= out.length || 1;
      console.log(`[AudioVisualizer] applyBars (#${applyCount}) avg=${avg.toFixed(3)}`);
    }
  }

  async function start() {
    console.log('[AudioVisualizer] start()');
    await source.start((next) => applyBars(next));
  }

  function stop() {
    console.log('[AudioVisualizer] stop()');
    source.stop();
    // Оставляем последние значения — они плавно "погаснут" через opacity в компоненте
  }

  watch(
    () => active.value,
    (isActive) => {
      console.log('[AudioVisualizer] active изменился →', isActive);
      if (isActive) {
        start();
      } else {
        stop();
      }
    },
    { immediate: true }
  );

  onUnmounted(() => {
    stop();
  });

  return {
    bars,
    start,
    stop,
  };
}

