import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { playShowSound } from '../utils/sound';
import {
  RecordingStatus,
  PartialTranscriptionPayload,
  FinalTranscriptionPayload,
  RecordingStatusPayload,
  TranscriptionErrorPayload,
  EVENT_TRANSCRIPTION_PARTIAL,
  EVENT_TRANSCRIPTION_FINAL,
  EVENT_RECORDING_STATUS,
  EVENT_TRANSCRIPTION_ERROR,
} from '../types';

export const useTranscriptionStore = defineStore('transcription', () => {
  // State
  const status = ref<RecordingStatus>(RecordingStatus.Idle);
  const partialText = ref<string>(''); // текущий промежуточный сегмент
  const accumulatedText = ref<string>(''); // накопленные финализированные сегменты
  const finalText = ref<string>(''); // полный финальный результат (для копирования)
  const error = ref<string | null>(null);
  const lastFinalizedText = ref<string>(''); // последний финализированный текст (для дедупликации)

  // Анимированный текст для эффекта печати
  const animatedPartialText = ref<string>('');
  const animatedAccumulatedText = ref<string>('');

  // Таймеры для анимации
  let partialAnimationTimer: NodeJS.Timeout | null = null;
  let accumulatedAnimationTimer: NodeJS.Timeout | null = null;

  // Listeners
  type UnlistenFn = () => void;
  let unlistenPartial: UnlistenFn | null = null;
  let unlistenFinal: UnlistenFn | null = null;
  let unlistenStatus: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;

  // Computed
  const isStarting = computed(() => status.value === RecordingStatus.Starting);
  const isRecording = computed(() => status.value === RecordingStatus.Recording);
  const isIdle = computed(() => status.value === RecordingStatus.Idle);
  const isProcessing = computed(() => status.value === RecordingStatus.Processing);
  const hasError = computed(() => status.value === RecordingStatus.Error);

  const displayText = computed(() => {
    // Показываем: финальный текст + анимированный накопленный + анимированный промежуточный
    const final = finalText.value;
    const accumulated = animatedAccumulatedText.value;
    const partial = animatedPartialText.value;

    // Собираем все части которые есть
    const parts = [];
    if (final) parts.push(final);
    if (accumulated) parts.push(accumulated);
    if (partial) parts.push(partial);

    if (parts.length > 0) {
      return parts.join(' ');
    }

    // Показываем placeholder только когда в режиме Idle
    if (status.value === RecordingStatus.Idle) {
      return 'Press the button or use hotkey to start recording...';
    }

    // Во время Starting/Recording показываем пустую строку или "Listening..."
    if (status.value === RecordingStatus.Starting) {
      return 'Подключение...';
    }

    if (status.value === RecordingStatus.Recording) {
      return 'Говорите...';
    }

    return '';
  });

  // Функция для анимации partial текста посимвольно
  function animatePartialText(targetText: string): void {
    // Очищаем предыдущий таймер если есть
    if (partialAnimationTimer) {
      clearInterval(partialAnimationTimer);
      partialAnimationTimer = null;
    }

    // Если новый текст короче текущего - просто обновляем мгновенно
    if (targetText.length < animatedPartialText.value.length) {
      animatedPartialText.value = targetText;
      return;
    }

    // Если текст не изменился - ничего не делаем
    if (targetText === animatedPartialText.value) {
      return;
    }

    // Начинаем с текущей длины (чтобы не повторять уже показанные символы)
    let currentIndex = animatedPartialText.value.length;

    // Если текст полностью новый - начинаем с нуля
    if (!targetText.startsWith(animatedPartialText.value)) {
      currentIndex = 0;
      animatedPartialText.value = '';
    }

    // Посимвольная анимация каждые 30мс
    partialAnimationTimer = setInterval(() => {
      if (currentIndex < targetText.length) {
        animatedPartialText.value = targetText.slice(0, currentIndex + 1);
        currentIndex++;
      } else {
        // Анимация завершена - очищаем таймер
        if (partialAnimationTimer) {
          clearInterval(partialAnimationTimer);
          partialAnimationTimer = null;
        }
      }
    }, 30);
  }

  // Функция для анимации accumulated текста посимвольно
  function animateAccumulatedText(targetText: string): void {
    // Очищаем предыдущий таймер если есть
    if (accumulatedAnimationTimer) {
      clearInterval(accumulatedAnimationTimer);
      accumulatedAnimationTimer = null;
    }

    // Если новый текст короче текущего - просто обновляем мгновенно
    if (targetText.length < animatedAccumulatedText.value.length) {
      animatedAccumulatedText.value = targetText;
      return;
    }

    // Если текст не изменился - ничего не делаем
    if (targetText === animatedAccumulatedText.value) {
      return;
    }

    // Начинаем с текущей длины (чтобы не повторять уже показанные символы)
    let currentIndex = animatedAccumulatedText.value.length;

    // Если текст полностью новый - начинаем с нуля
    if (!targetText.startsWith(animatedAccumulatedText.value)) {
      currentIndex = 0;
      animatedAccumulatedText.value = '';
    }

    // Посимвольная анимация каждые 30мс
    accumulatedAnimationTimer = setInterval(() => {
      if (currentIndex < targetText.length) {
        animatedAccumulatedText.value = targetText.slice(0, currentIndex + 1);
        currentIndex++;
      } else {
        // Анимация завершена - очищаем таймер
        if (accumulatedAnimationTimer) {
          clearInterval(accumulatedAnimationTimer);
          accumulatedAnimationTimer = null;
        }
      }
    }, 30);
  }

  // Actions
  async function initialize() {
    console.log('Initializing transcription store');

    // Отписываемся от старых listeners перед регистрацией новых
    // Это предотвращает дублирование событий при повторной инициализации
    cleanup();

    try {
      // Listen to partial transcription events
      unlistenPartial = await listen<PartialTranscriptionPayload>(
        EVENT_TRANSCRIPTION_PARTIAL,
        (event) => {
          console.log('Received partial transcription:', event.payload);

          // если сегмент финализирован - добавляем к накопленному тексту
          if (event.payload.is_segment_final) {
            console.log('Segment finalized:', event.payload.text);
            console.log('Last finalized text:', lastFinalizedText.value);

            let newText = event.payload.text;

            // Deepgram может отправлять весь накопленный текст сессии
            // Проверяем, не дублируется ли текст
            if (lastFinalizedText.value && newText.startsWith(lastFinalizedText.value)) {
              // Текст начинается с уже обработанного - берем только новую часть
              const newPart = newText.slice(lastFinalizedText.value.length).trim();
              console.log('Detected duplicate text, extracted new part:', newPart);

              if (newPart) {
                accumulatedText.value = accumulatedText.value
                  ? `${accumulatedText.value} ${newPart}`
                  : newPart;
                lastFinalizedText.value = newText; // сохраняем полный текст для следующей проверки

                // Запускаем анимацию для accumulated текста
                animateAccumulatedText(accumulatedText.value);
              }
            } else {
              // Это новый независимый сегмент
              accumulatedText.value = accumulatedText.value
                ? `${accumulatedText.value} ${newText}`
                : newText;
              lastFinalizedText.value = newText;

              // Запускаем анимацию для accumulated текста
              animateAccumulatedText(accumulatedText.value);
            }

            partialText.value = ''; // очищаем промежуточный текст
            animatedPartialText.value = ''; // очищаем анимированный partial текст

            // Останавливаем анимацию partial текста
            if (partialAnimationTimer) {
              clearInterval(partialAnimationTimer);
              partialAnimationTimer = null;
            }
          } else {
            // промежуточный результат - просто обновляем
            partialText.value = event.payload.text;

            // Запускаем анимацию для partial текста
            animatePartialText(event.payload.text);
          }
        }
      );

      // Listen to final transcription events
      unlistenFinal = await listen<FinalTranscriptionPayload>(
        EVENT_TRANSCRIPTION_FINAL,
        async (event) => {
          console.log('Received final transcription:', event.payload);

          // Deepgram отправляет финальный сегмент когда вся речь завершена (speech_final=true)
          // К этому моменту все сегменты уже накоплены в accumulatedText
          if (event.payload.text) {
            // Если это первый final сегмент - используем весь накопленный текст
            // (финализированные сегменты уже есть в accumulatedText, не дублируем!)
            if (!finalText.value && accumulatedText.value) {
              finalText.value = accumulatedText.value;
            } else if (finalText.value) {
              // Уже есть финальный текст - добавляем новый сегмент
              finalText.value = `${finalText.value} ${event.payload.text}`;
            } else {
              // Нет ни накопленного, ни финального - просто используем пришедший
              finalText.value = event.payload.text;
            }

            // Очищаем промежуточные данные после финализации сегмента
            partialText.value = '';
            accumulatedText.value = '';
            lastFinalizedText.value = '';

            // Очищаем анимированные тексты
            animatedPartialText.value = '';
            animatedAccumulatedText.value = '';

            // Останавливаем все анимации
            if (partialAnimationTimer) {
              clearInterval(partialAnimationTimer);
              partialAnimationTimer = null;
            }
            if (accumulatedAnimationTimer) {
              clearInterval(accumulatedAnimationTimer);
              accumulatedAnimationTimer = null;
            }

            console.log('Updated final text:', finalText.value);

            // Auto-copy to clipboard с накопленным текстом
            try {
              await writeText(finalText.value);
              console.log('Copied to clipboard:', finalText.value);
            } catch (err) {
              console.error('Failed to copy to clipboard:', err);
            }
          }
        }
      );

      // Listen to recording status events
      unlistenStatus = await listen<RecordingStatusPayload>(
        EVENT_RECORDING_STATUS,
        (event) => {
          console.log('Recording status changed:', event.payload);

          // Звук теперь воспроизводится раньше - в handleHotkeyToggle
          // Оставляем этот код закомментированным для справки
          // if (event.payload.status === RecordingStatus.Starting && status.value !== RecordingStatus.Starting) {
          //   console.log('Recording starting - playing show sound');
          //   playShowSound();
          // }

          // Если статус стал Starting или Recording - очищаем весь текст
          // Это работает и для кнопки, и для hotkey (Ctrl+X)
          if ((event.payload.status === RecordingStatus.Starting || event.payload.status === RecordingStatus.Recording)
              && status.value !== RecordingStatus.Starting
              && status.value !== RecordingStatus.Recording) {
            console.log('Recording starting/started - clearing all text');
            partialText.value = '';
            accumulatedText.value = '';
            finalText.value = '';
            lastFinalizedText.value = '';
            error.value = null;

            // Очищаем анимированный текст
            animatedPartialText.value = '';
            animatedAccumulatedText.value = '';

            // Очищаем таймеры анимации
            if (partialAnimationTimer) {
              clearInterval(partialAnimationTimer);
              partialAnimationTimer = null;
            }
            if (accumulatedAnimationTimer) {
              clearInterval(accumulatedAnimationTimer);
              accumulatedAnimationTimer = null;
            }
          }

          status.value = event.payload.status;
        }
      );

      // Listen to transcription error events
      unlistenError = await listen<TranscriptionErrorPayload>(
        EVENT_TRANSCRIPTION_ERROR,
        (event) => {
          console.error('Transcription error received:', event.payload);

          // Останавливаем все анимации
          if (partialAnimationTimer) {
            clearInterval(partialAnimationTimer);
            partialAnimationTimer = null;
          }
          if (accumulatedAnimationTimer) {
            clearInterval(accumulatedAnimationTimer);
            accumulatedAnimationTimer = null;
          }

          // Формируем понятное сообщение на русском
          let errorMessage = '';
          switch (event.payload.error_type) {
            case 'timeout':
              errorMessage = 'Превышен таймаут ожидания. Проверьте подключение к интернету.';
              break;
            case 'connection':
              errorMessage = 'Проблема с подключением. Проверьте интернет и попробуйте снова.';
              break;
            case 'authentication':
              errorMessage = 'Ошибка авторизации. Проверьте API ключ в настройках.';
              break;
            case 'processing':
              errorMessage = 'Ошибка обработки аудио. Попробуйте перезапустить запись.';
              break;
            default:
              errorMessage = `Ошибка: ${event.payload.error}`;
          }

          error.value = errorMessage;
          status.value = RecordingStatus.Error;
        }
      );

      console.log('Event listeners initialized successfully');
    } catch (err) {
      console.error('Failed to initialize event listeners:', err);
      error.value = `Failed to initialize: ${err}`;
    }
  }

  async function startRecording() {
    try {
      // Очищаем весь предыдущий текст перед новой записью
      error.value = null;
      partialText.value = '';
      accumulatedText.value = '';
      finalText.value = '';
      lastFinalizedText.value = '';
      status.value = RecordingStatus.Recording;

      // Очищаем анимированный текст
      animatedPartialText.value = '';
      animatedAccumulatedText.value = '';

      // Очищаем таймеры анимации
      if (partialAnimationTimer) {
        clearInterval(partialAnimationTimer);
        partialAnimationTimer = null;
      }
      if (accumulatedAnimationTimer) {
        clearInterval(accumulatedAnimationTimer);
        accumulatedAnimationTimer = null;
      }

      console.log('Starting new recording - all text cleared');

      const result = await invoke<string>('start_recording');
      console.log('Recording started:', result);
    } catch (err) {
      console.error('Failed to start recording:', err);
      error.value = String(err);
      status.value = RecordingStatus.Error;
    }
  }

  async function stopRecording() {
    try {
      status.value = RecordingStatus.Processing;
      const result = await invoke<string>('stop_recording');
      console.log('Recording stopped:', result);
    } catch (err) {
      console.error('Failed to stop recording:', err);
      error.value = String(err);
      status.value = RecordingStatus.Error;
    }
  }

  async function toggleRecording() {
    if (isRecording.value) {
      await stopRecording();
    } else {
      await startRecording();
    }
  }

  function cleanup() {
    if (unlistenPartial) {
      unlistenPartial();
      unlistenPartial = null;
    }
    if (unlistenFinal) {
      unlistenFinal();
      unlistenFinal = null;
    }
    if (unlistenStatus) {
      unlistenStatus();
      unlistenStatus = null;
    }
    if (unlistenError) {
      unlistenError();
      unlistenError = null;
    }

    // Очищаем таймеры анимации
    if (partialAnimationTimer) {
      clearInterval(partialAnimationTimer);
      partialAnimationTimer = null;
    }
    if (accumulatedAnimationTimer) {
      clearInterval(accumulatedAnimationTimer);
      accumulatedAnimationTimer = null;
    }
  }

  return {
    // State
    status,
    partialText,
    accumulatedText,
    finalText,
    error,

    // Computed
    isStarting,
    isRecording,
    isIdle,
    isProcessing,
    hasError,
    displayText,

    // Actions
    initialize,
    startRecording,
    stopRecording,
    toggleRecording,
    cleanup,
  };
});
