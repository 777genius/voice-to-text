/**
 * Composable для тестирования микрофона
 * Управляет записью тестового звука и его воспроизведением
 */

import { ref, onUnmounted } from 'vue';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { tauriSettingsService } from '../../infrastructure/adapters/TauriSettingsService';

export function useMicrophoneTest() {
  const isTesting = ref(false);
  const audioLevel = ref(0);
  const error = ref<string | null>(null);

  // Listener для события уровня громкости
  let levelUnlisten: UnlistenFn | null = null;

  // AudioContext должен быть "разблокирован" жестом пользователя.
  // Если создавать/стартовать его после await (IPC/таймеры) — WebView может заблокировать звук.
  let audioContext: AudioContext | null = null;

  function ensureAudioContext(): AudioContext {
    if (!audioContext || audioContext.state === 'closed') {
      audioContext = new AudioContext();
    }
    if (audioContext.state === 'suspended') {
      void audioContext.resume().catch(() => {});
    }
    return audioContext;
  }

  /**
   * Важно вызывать синхронно в обработчике клика (до любых await),
   * чтобы Web Audio не был заблокирован политикой autoplay.
   */
  function preparePlayback(): void {
    ensureAudioContext();
  }

  function resampleI16ToF32(
    samples: number[],
    inSampleRate: number,
    outSampleRate: number
  ): Float32Array<ArrayBuffer> {
    if (!samples.length) return new Float32Array(0);
    if (inSampleRate === outSampleRate) {
      const out = new Float32Array(samples.length);
      for (let i = 0; i < samples.length; i++) out[i] = samples[i] / 32768.0;
      return out;
    }

    const ratio = outSampleRate / inSampleRate;
    const outLength = Math.max(1, Math.round(samples.length * ratio));
    const out = new Float32Array(outLength);

    for (let i = 0; i < outLength; i++) {
      const srcIndex = i / ratio;
      const i0 = Math.floor(srcIndex);
      const i1 = Math.min(samples.length - 1, i0 + 1);
      const frac = srcIndex - i0;
      const s0 = samples[i0] ?? 0;
      const s1 = samples[i1] ?? s0;
      const v = s0 + (s1 - s0) * frac;
      out[i] = v / 32768.0;
    }

    return out;
  }

  /**
   * Запустить тест микрофона
   */
  async function start(
    sensitivity: number,
    deviceName: string | null
  ): Promise<void> {
    try {
      error.value = null;
      audioLevel.value = 0;

      // Подписываемся на события уровня громкости
      levelUnlisten = await tauriSettingsService.listenMicrophoneLevel(
        (level) => {
          audioLevel.value = level;
        }
      );

      // Запускаем тест
      await tauriSettingsService.startMicrophoneTest(sensitivity, deviceName);
      isTesting.value = true;
    } catch (err) {
      console.error('Ошибка запуска теста микрофона:', err);
      error.value = String(err);
      cleanup();
    }
  }

  /**
   * Остановить тест и получить записанные семплы
   */
  async function stop(): Promise<number[]> {
    try {
      const audioSamples = await tauriSettingsService.stopMicrophoneTest();

      isTesting.value = false;
      audioLevel.value = 0;
      cleanup();

      return audioSamples;
    } catch (err) {
      console.error('Ошибка остановки теста микрофона:', err);
      error.value = String(err);
      isTesting.value = false;
      cleanup();
      return [];
    }
  }

  /**
   * Воспроизвести записанное аудио через Web Audio API
   */
  function playAudio(samples: number[]): void {
    if (!samples.length) return;

    const ctx = ensureAudioContext();
    const inSampleRate = 16000;
    const outSampleRate = ctx.sampleRate;
    const channelData = resampleI16ToF32(samples, inSampleRate, outSampleRate);

    const buffer = ctx.createBuffer(1, channelData.length, outSampleRate);
    buffer.copyToChannel(channelData, 0);

    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(ctx.destination);
    source.start();
  }

  /**
   * Очистить ресурсы
   */
  function cleanup(): void {
    if (levelUnlisten) {
      levelUnlisten();
      levelUnlisten = null;
    }
  }

  // Очистка при размонтировании компонента
  onUnmounted(() => {
    cleanup();
    if (audioContext && audioContext.state !== 'closed') {
      void audioContext.close().catch(() => {});
    }
    audioContext = null;
  });

  return {
    isTesting,
    audioLevel,
    error,
    start,
    stop,
    preparePlayback,
    playAudio,
    cleanup,
  };
}
