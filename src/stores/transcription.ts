import { defineStore } from 'pinia';
import { ref, computed, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { playShowSound } from '../utils/sound';
import {
  RecordingStatus,
  ConnectionQuality,
  PartialTranscriptionPayload,
  FinalTranscriptionPayload,
  RecordingStatusPayload,
  TranscriptionErrorPayload,
  ConnectionQualityPayload,
  EVENT_TRANSCRIPTION_PARTIAL,
  EVENT_TRANSCRIPTION_FINAL,
  EVENT_RECORDING_STATUS,
  EVENT_TRANSCRIPTION_ERROR,
  EVENT_CONNECTION_QUALITY,
} from '../types';

export const useTranscriptionStore = defineStore('transcription', () => {
  // State
  const status = ref<RecordingStatus>(RecordingStatus.Idle);
  const partialText = ref<string>(''); // текущий промежуточный сегмент
  const accumulatedText = ref<string>(''); // накопленные финализированные сегменты
  const finalText = ref<string>(''); // полный финальный результат (для копирования)
  const error = ref<string | null>(null);
  const lastFinalizedText = ref<string>(''); // последний финализированный текст (для дедупликации)
  const connectionQuality = ref<ConnectionQuality>(ConnectionQuality.Good);

  // Config flags
  const autoCopyEnabled = ref<boolean>(true);
  const autoPasteEnabled = ref<boolean>(false);

  // Флаг для защиты от дублирования auto-paste
  // Хранит значение finalText на момент последней успешной вставки
  const lastPastedFinalText = ref<string>('');

  // Отслеживание utterances по start времени
  const currentUtteranceStart = ref<number>(-1); // start время текущей utterance (-1 = нет активной)

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
  let unlistenConnectionQuality: UnlistenFn | null = null;

  // Computed
  const isStarting = computed(() => status.value === RecordingStatus.Starting);
  const isRecording = computed(() => status.value === RecordingStatus.Recording);
  const isIdle = computed(() => status.value === RecordingStatus.Idle);
  const isProcessing = computed(() => status.value === RecordingStatus.Processing);
  const hasError = computed(() => status.value === RecordingStatus.Error);
  const hasConnectionIssue = computed(() =>
    connectionQuality.value !== ConnectionQuality.Good
  );

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

  // Функция для анимации partial текста пословно (избегаем дергания при переносах)
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

    // Если текст полностью новый - начинаем с нуля
    if (!targetText.startsWith(animatedPartialText.value)) {
      animatedPartialText.value = '';
    }

    // Находим добавленную часть текста
    const addedText = targetText.slice(animatedPartialText.value.length);

    // Разбиваем добавленный текст на слова (сохраняя пробелы)
    const words = addedText.split(/(\s+)/);
    let wordIndex = 0;

    // Пословная анимация каждые 15мс (быстрее и без дерганий)
    partialAnimationTimer = setInterval(() => {
      if (wordIndex < words.length) {
        animatedPartialText.value += words[wordIndex];
        wordIndex++;
      } else {
        // Анимация завершена - очищаем таймер
        if (partialAnimationTimer) {
          clearInterval(partialAnimationTimer);
          partialAnimationTimer = null;
        }
      }
    }, 15);
  }

  // Функция для анимации accumulated текста пословно (избегаем дергания при переносах)
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

    // Если текст полностью новый - начинаем с нуля
    if (!targetText.startsWith(animatedAccumulatedText.value)) {
      animatedAccumulatedText.value = '';
    }

    // Находим добавленную часть текста
    const addedText = targetText.slice(animatedAccumulatedText.value.length);

    // Разбиваем добавленный текст на слова (сохраняя пробелы)
    const words = addedText.split(/(\s+)/);
    let wordIndex = 0;

    // Пословная анимация каждые 15мс (быстрее и без дерганий)
    accumulatedAnimationTimer = setInterval(() => {
      if (wordIndex < words.length) {
        animatedAccumulatedText.value += words[wordIndex];
        wordIndex++;
      } else {
        // Анимация завершена - очищаем таймер
        if (accumulatedAnimationTimer) {
          clearInterval(accumulatedAnimationTimer);
          accumulatedAnimationTimer = null;
        }
      }
    }, 15);
  }

  // Actions
  async function initialize() {
    console.log('Initializing transcription store');

    // Отписываемся от старых listeners перед регистрацией новых
    // Это предотвращает дублирование событий при повторной инициализации
    cleanup();

    // Загружаем настройки auto-copy/paste из конфига
    try {
      const appConfig = await invoke<any>('get_app_config');
      autoCopyEnabled.value = appConfig.auto_copy_to_clipboard ?? true;
      autoPasteEnabled.value = appConfig.auto_paste_text ?? false;
      console.log('Config loaded: autoCopy=', autoCopyEnabled.value, 'autoPaste=', autoPasteEnabled.value);
    } catch (err) {
      console.error('Failed to load auto-paste config:', err);
    }

    try {
      // Listen to partial transcription events
      unlistenPartial = await listen<PartialTranscriptionPayload>(
        EVENT_TRANSCRIPTION_PARTIAL,
        async (event) => {
          // Детальное логирование для отладки
          console.log('📝 PARTIAL EVENT:', {
            text: event.payload.text,
            is_segment_final: event.payload.is_segment_final,
            start: event.payload.start,
            duration: event.payload.duration,
            timestamp: event.payload.timestamp,
            current_utterance_start: currentUtteranceStart.value,
            current_accumulated: accumulatedText.value,
            current_partial: partialText.value,
            last_finalized: lastFinalizedText.value
          });

          // Если сегмент финализирован (is_final=true, но не speech_final)
          if (event.payload.is_segment_final) {
            const newText = event.payload.text;

            // Проверка на точный дубликат (защита от повторной отправки того же сегмента)
            if (newText === lastFinalizedText.value) {
              console.log('⚠️ Exact duplicate segment detected, skipping:', newText);
              return;
            }

            // Финализировали utterance - добавляем к накопленному тексту
            const oldAccumulated = accumulatedText.value;
            console.log('🔒 [BEFORE ACCUMULATE] accumulated:', oldAccumulated);
            console.log('🔒 [BEFORE ACCUMULATE] newText:', newText);

            accumulatedText.value = accumulatedText.value
              ? `${accumulatedText.value} ${newText}`
              : newText;

            lastFinalizedText.value = newText;

            console.log('🔒 [AFTER ACCUMULATE] accumulated:', accumulatedText.value);
            console.log('🔒 Utterance finalized and accumulated:', {
              utterance: newText,
              start: event.payload.start,
              total_accumulated: accumulatedText.value,
              currentUtteranceStart: currentUtteranceStart.value
            });

            // Запускаем анимацию для accumulated текста
            animateAccumulatedText(accumulatedText.value);

            // Очищаем промежуточный текст (НЕ сбрасываем utterance start!)
            // currentUtteranceStart сохраняется чтобы определить когда придет новая utterance
            partialText.value = '';
            animatedPartialText.value = '';

            // Останавливаем анимацию partial текста
            if (partialAnimationTimer) {
              clearInterval(partialAnimationTimer);
              partialAnimationTimer = null;
            }
          } else {
            // Промежуточный результат (is_final=false)
            // Deepgram отправляет НАКОПЛЕННЫЙ текст utterance, поэтому просто ЗАМЕНЯЕМ

            // Если это та же utterance (start совпадает) - просто обновляем partial текст
            if (currentUtteranceStart.value === event.payload.start || currentUtteranceStart.value === -1) {
              currentUtteranceStart.value = event.payload.start;
              partialText.value = event.payload.text;

              console.log('📝 Interim update (same utterance):', {
                start: event.payload.start,
                text: event.payload.text
              });

              // Запускаем анимацию для partial текста
              animatePartialText(event.payload.text);
            } else {
              // Новая utterance началась (start изменился)
              // Это означает что предыдущая utterance должна была быть финализирована, но не была
              console.warn('⚠️ Utterance start changed without finalization!', {
                old_start: currentUtteranceStart.value,
                new_start: event.payload.start,
                old_partial: partialText.value,
                new_text: event.payload.text,
                accumulated_text: accumulatedText.value
              });

              // Сохраняем accumulated текст от предыдущей utterance если он есть
              if (accumulatedText.value) {
                const oldFinalText = finalText.value;
                console.log('💾 [BEFORE SAVE] finalText:', oldFinalText);
                console.log('💾 [BEFORE SAVE] accumulated:', accumulatedText.value);

                finalText.value = finalText.value
                  ? `${finalText.value} ${accumulatedText.value}`
                  : accumulatedText.value;

                console.log('💾 [AFTER SAVE] finalText:', finalText.value);
                console.log('💾 Successfully saved accumulated text to finalText');

                accumulatedText.value = '';
                animatedAccumulatedText.value = '';
                lastFinalizedText.value = '';
              } else {
                console.log('💾 [SKIP] No accumulated text to save (already empty)');
              }

              // Начинаем новую utterance
              currentUtteranceStart.value = event.payload.start;
              partialText.value = event.payload.text;

              // Запускаем анимацию для partial текста
              animatePartialText(event.payload.text);
            }
          }
        }
      );

      // Listen to final transcription events
      unlistenFinal = await listen<FinalTranscriptionPayload>(
        EVENT_TRANSCRIPTION_FINAL,
        async (event) => {
          // Детальное логирование для отладки
          console.log('✅ FINAL EVENT (speech_final=true):', {
            text: event.payload.text,
            confidence: event.payload.confidence,
            language: event.payload.language,
            timestamp: event.payload.timestamp,
            current_accumulated: accumulatedText.value,
            current_final: finalText.value,
            current_partial: partialText.value
          });

          // Deepgram отправляет финальный сегмент когда вся речь завершена (speech_final=true)
          //
          // БАГ-ФИКС (2025-10-30): Deepgram может разбивать речь на несколько utterances с разными start временами.
          // Если между SEGMENT FINAL и следующим Partial приходит другой FINAL - currentUtteranceStart
          // сбрасывается в -1, что ломает логику обнаружения смены utterance. Из-за этого accumulated текст
          // не сохраняется в finalText и теряется.
          //
          // Пример из логов:
          // 1. FINAL #1 (start=0.00s): "Да, должна происходить индексация." → currentUtteranceStart = -1
          // 2. SEGMENT FINAL (start=3.41s): "Когда в админке её запускаешь" → accumulated += текст
          // 3. Partial (start=6.73s): новый start, но currentUtteranceStart=-1 → код думает "та же utterance"
          // 4. FINAL #2 (start=6.73s): "для конкретного диалога?" → берет ТОЛЬКО это, accumulated теряется
          //
          // РЕШЕНИЕ: ВСЕГДА добавляем accumulated к FINAL тексту (если есть).
          // Дублирования не будет, т.к. accumulated очищается только при сохранении в finalText.
          if (event.payload.text || accumulatedText.value || partialText.value) {
            const currentUtteranceText = [accumulatedText.value, event.payload.text || partialText.value]
              .filter(Boolean)
              .join(' ')
              .trim();

            console.log('🔗 [SPEECH_FINAL] Combining utterance:', {
              accumulated: accumulatedText.value,
              partial: partialText.value,
              final_payload: event.payload.text,
              used_source: event.payload.text ? 'FINAL payload' : 'accumulated+partial',
              combined: currentUtteranceText
            });

            const oldFinalText = finalText.value;
            console.log('📋 [BEFORE ADD] finalText:', oldFinalText);
            console.log('📋 [BEFORE ADD] currentUtteranceText:', currentUtteranceText);

            console.log('🧹 [CLEANUP] Clearing all temporary data BEFORE updating finalText');
            console.log('🧹 [CLEANUP] Before: accumulated=', accumulatedText.value, 'partial=', partialText.value);

            // Очищаем промежуточные данные ПЕРЕД обновлением finalText
            // чтобы избежать дублирования в UI
            partialText.value = '';
            accumulatedText.value = '';
            lastFinalizedText.value = '';
            currentUtteranceStart.value = -1;

            // Очищаем анимированные тексты
            animatedPartialText.value = '';
            animatedAccumulatedText.value = '';

            console.log('🧹 [CLEANUP] After: all cleared, currentUtteranceStart reset to -1');

            // Останавливаем все анимации
            if (partialAnimationTimer) {
              clearInterval(partialAnimationTimer);
              partialAnimationTimer = null;
            }
            if (accumulatedAnimationTimer) {
              clearInterval(accumulatedAnimationTimer);
              accumulatedAnimationTimer = null;
            }

            // Добавляем к финальному тексту
            finalText.value = finalText.value
              ? `${finalText.value} ${currentUtteranceText}`
              : currentUtteranceText;

            console.log('📋 [AFTER ADD] finalText:', finalText.value);
            console.log('📋 Successfully added utterance to finalText');

            // Auto-paste финальной фразы (вся utterance целиком)
            if (autoPasteEnabled.value && currentUtteranceText.trim()) {
              // Защита от дубликатов: проверяем что мы еще не вставляли эту версию finalText
              if (finalText.value !== lastPastedFinalText.value) {
                try {
                  // Добавляем пробел перед фразой если это не первая фраза
                  const needsSpace = oldFinalText.length > 0;
                  const textToInsert = needsSpace ? ` ${currentUtteranceText}` : currentUtteranceText;
                  console.log('📝 Auto-pasting final utterance:', textToInsert);
                  await invoke('auto_paste_text', { text: textToInsert });
                  console.log('✅ Auto-pasted successfully');

                  // ВАЖНО: Обновляем флаг ПОСЛЕ успешной вставки
                  lastPastedFinalText.value = finalText.value;
                } catch (err) {
                  console.error('❌ Failed to auto-paste:', err);

                  // Fallback: копируем в clipboard
                  try {
                    await invoke('copy_to_clipboard_native', { text: currentUtteranceText });
                    console.log('📋 Fallback: copied to clipboard');
                  } catch (copyErr) {
                    console.error('❌ Failed to copy to clipboard:', copyErr);
                  }
                }
              } else {
                console.log('⏭️ Skipping auto-paste: already pasted this version of finalText');
              }
            }

            // Auto-copy to clipboard с накопленным текстом (если включено)
            if (autoCopyEnabled.value) {
              try {
                await invoke('copy_to_clipboard_native', { text: finalText.value });
                console.log('📋 Auto-copied to clipboard:', finalText.value);
              } catch (err) {
                console.error('Failed to copy to clipboard:', err);
              }
            } else {
              console.log('📋 Auto-copy disabled, skipping clipboard');
            }
          } else {
            console.warn('⚠️ [SPEECH_FINAL] event.payload.text is empty, skipping');
            console.log('⚠️ [SPEECH_FINAL] Event payload:', event.payload);
          }
        }
      );

      // Listen to recording status events
      unlistenStatus = await listen<RecordingStatusPayload>(
        EVENT_RECORDING_STATUS,
        async (event) => {
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
            currentUtteranceStart.value = -1;
            error.value = null;

            // Сбрасываем флаг auto-paste
            lastPastedFinalText.value = '';

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

          // Если статус стал Idle - обрабатываем текущий текст при ЛЮБОЙ остановке
          // (через hotkey ИЛИ через VAD timeout когда пользователь закончил говорить)
          //
          // Из логов [2025-11-03]: VAD timeout - это нормальный способ остановки после молчания >3 сек.
          // Пользователь закончил говорить → текст должен скопироваться и вставиться автоматически.
          // Проверка `stopped_via_hotkey` убрана, чтобы auto-paste работал в обоих случаях.
          if (event.payload.status === RecordingStatus.Idle) {
            console.log('🔄 Запись остановлена - обрабатываем текущий текст');

            // Собираем весь видимый текст (final + accumulated + partial)
            const currentText = [finalText.value, accumulatedText.value, partialText.value]
              .filter(Boolean)
              .join(' ')
              .trim();

            if (currentText) {
              console.log('📝 Текущий текст для обработки:', currentText);

              // Auto-copy: копируем ВЕСЬ текст в clipboard
              if (autoCopyEnabled.value) {
                try {
                  await invoke('copy_to_clipboard_native', { text: currentText });
                  console.log('📋 Весь текст скопирован в clipboard');
                } catch (err) {
                  console.error('❌ Ошибка копирования:', err);
                }
              }

              // Auto-paste: вставляем только НОВУЮ часть
              if (autoPasteEnabled.value) {
                // Определяем что нужно вставить (только новое)
                let textToInsert = currentText;

                if (lastPastedFinalText.value) {
                  // Если уже что-то вставляли, вставляем только новую часть
                  if (currentText.startsWith(lastPastedFinalText.value)) {
                    textToInsert = currentText.slice(lastPastedFinalText.value.length).trim();

                    // Добавляем пробел если нужно
                    if (textToInsert && lastPastedFinalText.value) {
                      textToInsert = ' ' + textToInsert;
                    }
                  }
                }

                if (textToInsert.trim()) {
                  try {
                    console.log('📝 Auto-paste: вставляем новую часть:', textToInsert);
                    await invoke('auto_paste_text', { text: textToInsert });
                    console.log('✅ Новая часть вставлена через auto-paste');

                    // Обновляем lastPastedFinalText
                    lastPastedFinalText.value = currentText;
                  } catch (err) {
                    console.error('❌ Ошибка auto-paste:', err);
                  }
                } else {
                  console.log('⏭️ Нечего вставлять - весь текст уже был вставлен');
                }
              }
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

      // Listen to connection quality events
      unlistenConnectionQuality = await listen<ConnectionQualityPayload>(
        EVENT_CONNECTION_QUALITY,
        (event) => {
          console.log('Connection quality changed:', event.payload.quality, event.payload.reason);
          connectionQuality.value = event.payload.quality;

          // Сбрасываем connection quality обратно в Good когда запись останавливается
          // (чтобы избежать показа старого статуса при следующей записи)
          if (status.value === RecordingStatus.Idle) {
            connectionQuality.value = ConnectionQuality.Good;
          }
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
      currentUtteranceStart.value = -1;
      status.value = RecordingStatus.Recording;

      // Сбрасываем флаг auto-paste
      lastPastedFinalText.value = '';

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
    if (unlistenConnectionQuality) {
      unlistenConnectionQuality();
      unlistenConnectionQuality = null;
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

  // Перезагрузка настроек auto-copy/paste из конфига
  // Вызывается после сохранения настроек в Settings
  async function reloadConfig() {
    try {
      const appConfig = await invoke<any>('get_app_config');
      autoCopyEnabled.value = appConfig.auto_copy_to_clipboard ?? true;
      autoPasteEnabled.value = appConfig.auto_paste_text ?? false;
      console.log('Config reloaded: autoCopy=', autoCopyEnabled.value, 'autoPaste=', autoPasteEnabled.value);
    } catch (err) {
      console.error('Failed to reload config:', err);
    }
  }

  return {
    // State
    status,
    partialText,
    accumulatedText,
    finalText,
    error,
    connectionQuality,

    // Computed
    isStarting,
    isRecording,
    isIdle,
    isProcessing,
    hasError,
    hasConnectionIssue,
    displayText,

    // Actions
    initialize,
    startRecording,
    stopRecording,
    toggleRecording,
    cleanup,
    reloadConfig,
  };
});
