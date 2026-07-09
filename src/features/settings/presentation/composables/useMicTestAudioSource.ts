/**
 * Источник данных для AudioVisualizer на основе уровня микрофонного теста.
 * Конвертирует одно значение level (0-1) в массив баров с вариацией,
 * чтобы визуализатор выглядел как полноценный спектр.
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { AudioVisualizerSource } from '../../../../composables/useAudioVisualizer';

const BAR_COUNT = 48;

type MicLevelPayload = {
  level: number;
};

export class MicTestAudioSource implements AudioVisualizerSource {
  private unlisten: UnlistenFn | null = null;
  private startPromise: Promise<void> | null = null;
  private desiredActive = false;
  private onBars: ((bars: number[]) => void) | null = null;
  // Фазы для каждого бара — создают уникальную волнообразную форму
  private readonly phases = Array.from(
    { length: BAR_COUNT },
    (_, i) => (i / BAR_COUNT) * Math.PI * 2 + Math.random() * 0.5
  );

  async start(onBars: (bars: number[]) => void): Promise<void> {
    this.desiredActive = true;
    this.onBars = onBars;
    if (this.unlisten) return;
    if (this.startPromise) return this.startPromise;

    const startPromise = listen<MicLevelPayload>('microphone_test:level', (event) => {
      const level = Math.min(1, Math.max(0, event.payload?.level ?? 0));
      const bars = this.levelToBars(level);
      this.onBars?.(bars);
    })
      .then((unlisten) => {
        if (!this.desiredActive || this.unlisten) {
          unlisten();
          return;
        }
        this.unlisten = unlisten;
      })
      .catch((err) => {
        console.error('Failed to listen microphone test level events:', err);
      })
      .finally(() => {
        if (this.startPromise === startPromise) {
          this.startPromise = null;
        }
      });
    this.startPromise = startPromise;
    return startPromise;
  }

  stop(): void {
    this.desiredActive = false;
    this.onBars = null;
    if (!this.unlisten) return;
    this.unlisten();
    this.unlisten = null;
  }

  /**
   * Раскладываем один level в массив баров.
   * Центральные бары — максимум, к краям затухают по гауссиану,
   * плюс синусоидальная вариация чтобы каждый бар немного отличался.
   */
  private levelToBars(level: number): number[] {
    const t = performance.now() / 1000;
    const bars: number[] = new Array(BAR_COUNT);
    const center = (BAR_COUNT - 1) / 2;

    for (let i = 0; i < BAR_COUNT; i++) {
      // Гауссово затухание от центра к краям
      const dist = Math.abs(i - center) / center;
      const envelope = Math.exp(-dist * dist * 2.5);

      // Синусоидальная вариация — каждый бар "дышит" чуть в своём ритме
      const wave = 0.7 + 0.3 * Math.sin(t * 3.5 + this.phases[i]);

      bars[i] = Math.min(1, level * envelope * wave);
    }

    return bars;
  }
}
