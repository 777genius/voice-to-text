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

    const sampleRate = 16000;
    const audioContext = new AudioContext({ sampleRate });
    const buffer = audioContext.createBuffer(1, samples.length, sampleRate);

    // Конвертируем i16 в f32 (-1 до 1)
    const channelData = buffer.getChannelData(0);
    for (let i = 0; i < samples.length; i++) {
      channelData[i] = samples[i] / 32767.0;
    }

    const source = audioContext.createBufferSource();
    source.buffer = buffer;
    source.connect(audioContext.destination);
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
  });

  return {
    isTesting,
    audioLevel,
    error,
    start,
    stop,
    playAudio,
    cleanup,
  };
}
