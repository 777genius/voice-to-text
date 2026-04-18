import { defineStore } from 'pinia';
import { ref, computed, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isTauriAvailable } from '../utils/tauri';
import { i18n } from '../i18n';
import { api } from '../features/auth/infrastructure/api/apiClient';
import { useAuthStore } from '../features/auth/store/authStore';
import { useAppConfigStore } from './appConfig';
import { getTokenRepository } from '../features/auth/infrastructure/repositories/TokenRepository';
import { getAuthContainer } from '../features/auth/infrastructure/di/authContainer';
import { canRefreshSession, isAccessTokenExpired } from '../features/auth/domain/entities/Session';
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
  // Идентификатор текущей сессии записи (приходит из backend в событиях).
  // Нужен, чтобы никогда не "протекал" текст из прошлой сессии в новую.
  const sessionId = ref<number | null>(null);
  // Сессии с id <= closedSessionIdFloor считаются "закрытыми".
  // Любые отложенные/поздние события от них игнорируем, чтобы UI не возвращался в старое состояние.
  const closedSessionIdFloor = ref<number>(0);
  // Максимальный session_id, который мы видели в status событиях.
  // Нужен, чтобы уметь "закрывать" последнюю сессию даже если часть событий потерялась.
  const lastSeenSessionId = ref<number>(0);
  // Флаг "ждём старт новой сессии": пока он true — игнорируем любые статусы/события,
  // которые не относятся к запуску новой записи (защита от поздних событий старого сокета).
  const awaitingSessionStart = ref<boolean>(false);
  const partialText = ref<string>(''); // текущий промежуточный сегмент
  const accumulatedText = ref<string>(''); // накопленные финализированные сегменты
  const finalText = ref<string>(''); // полный финальный результат (для копирования)
  const error = ref<string | null>(null);
  const errorType = ref<TranscriptionErrorPayload['error_type'] | null>(null);
  const lastFinalizedText = ref<string>(''); // последний финализированный текст (для дедупликации)
  const connectionQuality = ref<ConnectionQuality>(ConnectionQuality.Good);

  // Retry логика подключения (когда запись ещё не стартанула и мы пытаемся подключиться к STT)
  const isConnecting = ref<boolean>(false);
  const connectAttempt = ref<number>(0);
  const connectMaxAttempts = ref<number>(0);
  const lastConnectFailure = ref<TranscriptionErrorPayload['error_type'] | null>(null);
  const lastConnectFailureRaw = ref<string>('');
  const lastConnectFailureDetails = ref<TranscriptionErrorPayload['error_details'] | null>(null);

  // STT auth ошибки чаще всего означают "access token протух" (TTL ~15 минут).
  // Это НЕ должно выкидывать пользователя из аккаунта — сначала пробуем тихо обновить токен.
  let suppressNextErrorStatus = false;

  // Счётчик auto-retry при 429 (rate limit). Не даём ретраить бесконечно — максимум 2 раза подряд.
  let rateLimitRetryCount = 0;
  const RATE_LIMIT_MAX_RETRIES = 2;
  let isForcingLogout = false;
  let refreshAuthForSttPromise: Promise<boolean> | null = null;

  // Config flags — берём из appConfig store (единый источник правды)
  const appConfig = useAppConfigStore();
  const autoCopyEnabled = computed(() => appConfig.autoCopyToClipboard);
  const autoPasteEnabled = computed(() => appConfig.autoPasteText);

  // Auth store — нужен, чтобы корректно сбрасывать ошибки записи после успешной авторизации,
  // если ошибка относилась к предыдущему пользователю/токену.
  const authStore = useAuthStore();

  function clearRecordingUiError(reason: string): void {
    // Не трогаем активную запись, чтобы не скрывать реальные проблемы во время стрима.
    if (
      status.value === RecordingStatus.Starting ||
      status.value === RecordingStatus.Recording ||
      status.value === RecordingStatus.Processing
    ) {
      return;
    }

    if (error.value || errorType.value || status.value === RecordingStatus.Error) {
      console.info('[STT] Clearing recording error after auth change:', reason);
    }

    // Возвращаем UI в "готов" состояние.
    if (status.value === RecordingStatus.Error) status.value = RecordingStatus.Idle;
    error.value = null;
    errorType.value = null;
    isDeviceNotFoundError.value = false;

    // И сбрасываем контекст последней неудачной попытки подключения,
    // чтобы новые попытки стартовали "с чистого листа".
    lastConnectFailure.value = null;
    lastConnectFailureRaw.value = '';
    lastConnectFailureDetails.value = null;
    suppressNextErrorStatus = false;
  }

  // После успешной авторизации (или смены пользователя) очищаем "залипшую" ошибку на экране записи.
  watch(
    () =>
      [
        authStore.isAuthenticated,
        authStore.session?.user?.id ?? null,
        authStore.accessToken ?? null,
      ] as const,
    ([isAuthed, userId, token], [prevAuthed, prevUserId, prevToken]) => {
      if (!isAuthed) return;

      const becameAuthed = !prevAuthed && isAuthed;
      const userChanged = !!userId && !!prevUserId && userId !== prevUserId;
      const tokenChanged = !!token && !!prevToken && token !== prevToken;

      // Если токен обновился (refresh) или пользователь сменился — старые ошибки
      // (401/429/прочие) могли относиться к прежней сессии.
      if (userChanged) {
        clearRecordingUiError('user_changed');
      } else if (becameAuthed) {
        clearRecordingUiError('authenticated');
      } else if (tokenChanged) {
        clearRecordingUiError('token_refreshed');
      }
    }
  );

  // Флаг для защиты от дублирования auto-paste
  // Хранит значение finalText на момент последней успешной вставки
  const lastPastedFinalText = ref<string>('');

  // Отслеживание utterances по start времени
  const currentUtteranceStart = ref<number>(-1); // start время текущей utterance (-1 = нет активной)

  // Анимированный текст для эффекта печати
  const animatedPartialText = ref<string>('');
  const animatedAccumulatedText = ref<string>('');

  // Таймеры для анимации
  let partialAnimationTimer: ReturnType<typeof setInterval> | null = null;
  let accumulatedAnimationTimer: ReturnType<typeof setInterval> | null = null;

  // Listeners
  type UnlistenFn = () => void;
  let unlistenPartial: UnlistenFn | null = null;
  let unlistenFinal: UnlistenFn | null = null;
  let unlistenStatus: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let unlistenConnectionQuality: UnlistenFn | null = null;

  function bumpLastSeenSessionId(next: number): void {
    if (next > lastSeenSessionId.value) {
      lastSeenSessionId.value = next;
    }
  }

  function markSessionsClosed(upToSessionId: number, reason: string): void {
    if (!upToSessionId || upToSessionId <= 0) return;

    const prev = closedSessionIdFloor.value;
    const next = Math.max(prev, upToSessionId);
    if (next !== prev) {
      closedSessionIdFloor.value = next;
      console.warn('[STT] Marked sessions closed up to', next, 'reason:', reason);
    }

    // Если текущая сессия попала под "закрытую" — принудительно сбрасываем её.
    if (sessionId.value !== null && sessionId.value <= closedSessionIdFloor.value) {
      sessionId.value = null;
    }
  }

  function ensureActiveSessionForIncomingEvent(payloadSessionId: number, source: string): boolean {
    bumpLastSeenSessionId(payloadSessionId);

    // Никогда не принимаем события от "закрытых" сессий.
    if (payloadSessionId <= closedSessionIdFloor.value) {
      return false;
    }

    // Если по какой-то причине пропустили recording:status (например, запись стартовала на Rust-стороне
    // сразу после show window, пока WebView ещё инициализируется), sessionId во фронте останется null,
    // и мы начнём игнорировать transcription:* навсегда, показывая "Подключение...".
    //
    // Чтобы UI самовосстанавливался, "усыновляем" session_id из первого пришедшего события,
    // но только когда ожидаем активный стрим (Starting/Recording/Processing или connect-retry).
    if (sessionId.value === null) {
      const canAdopt =
        awaitingSessionStart.value ||
        isConnecting.value ||
        status.value === RecordingStatus.Starting ||
        status.value === RecordingStatus.Recording ||
        status.value === RecordingStatus.Processing;

      if (!canAdopt) return false;

      console.warn('[STT] Adopted sessionId from event:', {
        source,
        payloadSessionId,
        status: status.value,
        awaitingSessionStart: awaitingSessionStart.value,
        isConnecting: isConnecting.value,
        closedFloor: closedSessionIdFloor.value,
      });

      sessionId.value = payloadSessionId;
      awaitingSessionStart.value = false;

      // Если мы "залипли" в Starting из-за пропущенного recording:status=Recording,
      // но уже видим события transcription:* — значит запись реально идёт.
      if (status.value === RecordingStatus.Starting) {
        status.value = RecordingStatus.Recording;
      }
    }

    return payloadSessionId === sessionId.value;
  }

  async function reconcileBackendStatus(reason: string): Promise<RecordingStatus | null> {
    if (!isTauriAvailable()) return null;

    try {
      const backendStatus = await invoke<RecordingStatus>('get_recording_status');
      if (backendStatus === RecordingStatus.Idle) {
        const uiIsStartingFlow =
          awaitingSessionStart.value ||
          isConnecting.value ||
          status.value === RecordingStatus.Starting;

        // ВАЖНО: иногда get_recording_status может на короткое время вернуть Idle
        // в момент старта записи (race: окно показано → reconcile успел спросить backend
        // до того как сервис обновил статус, но events уже летят/полетят).
        //
        // Если здесь "жёстко закрыть" сессию (markSessionsClosed) — мы можем случайно
        // пометить ТЕКУЩУЮ session_id как закрытую, и потом навсегда игнорировать
        // recording:status=Recording для этой же сессии → UI залипнет на "Подключение...".
        if (!uiIsStartingFlow) {
          // Backend говорит что мы точно не пишем — значит можно жёстко закрыть последнюю сессию,
          // чтобы никакие "поздние" события не вернули UI назад.
          markSessionsClosed(lastSeenSessionId.value, `backend_idle:${reason}`);
        } else {
          console.warn('[STT] Reconcile: backend reports Idle during start flow, skipping close floor update', {
            reason,
            backendStatus,
            uiStatus: status.value,
            awaitingSessionStart: awaitingSessionStart.value,
            isConnecting: isConnecting.value,
            lastSeenSessionId: lastSeenSessionId.value,
            closedFloor: closedSessionIdFloor.value,
          });
        }
      }

      if (backendStatus !== status.value) {
        // Не откатываем Starting → Idle: запись могла быть только что запрошена,
        // бэкенд ещё обрабатывает команду (race condition: window_shown эмитится
        // ДО start_recording в toggle_recording_with_window_internal).
        // Пропускаем только перезапись status, cleanup ниже выполняется всегда.
        if (status.value === RecordingStatus.Starting && backendStatus === RecordingStatus.Idle) {
          console.warn('[STT] Reconcile: keeping Starting (backend reports Idle, likely race with start_recording)');
        } else {
          console.warn('[STT] Reconcile status:', {
            reason,
            backendStatus,
            uiStatus: status.value,
            uiSessionId: sessionId.value,
            closedFloor: closedSessionIdFloor.value,
            lastSeenSessionId: lastSeenSessionId.value,
          });
          status.value = backendStatus;
        }
      }

      // Если backend idle, но UI почему-то держит активную сессию — сбрасываем.
      if (backendStatus === RecordingStatus.Idle) {
        // В start-flow не сбрасываем sessionId/awaitingSessionStart — иначе можем
        // "оторвать" UI от реальной записи при гонке статусов.
        const uiIsStartingFlow =
          awaitingSessionStart.value ||
          isConnecting.value ||
          status.value === RecordingStatus.Starting;
        if (!uiIsStartingFlow) {
          sessionId.value = null;
          awaitingSessionStart.value = false;
        }
      }

      return backendStatus;
    } catch (err) {
      console.warn('[STT] Failed to reconcile backend status:', reason, err);
      return null;
    }
  }

  // Computed
  const isStarting = computed(() => status.value === RecordingStatus.Starting);
  const isRecording = computed(() => status.value === RecordingStatus.Recording);
  const isIdle = computed(() => status.value === RecordingStatus.Idle);
  const isProcessing = computed(() => status.value === RecordingStatus.Processing);
  const hasError = computed(() => status.value === RecordingStatus.Error);
  const hasConnectionIssue = computed(() =>
    connectionQuality.value !== ConnectionQuality.Good
  );

  const canReconnect = computed(() => {
    // Показываем кнопку только когда реально упали в Error и причина похожа на сеть/таймаут
    if (status.value !== RecordingStatus.Error) return false;
    return errorType.value === 'connection' || errorType.value === 'timeout';
  });

  // Показываем кнопку "Активировать лицензию" при исчерпании лимита
  const canActivateLicense = computed(() => {
    if (status.value !== RecordingStatus.Error) return false;
    return errorType.value === 'limit_exceeded';
  });

  const isDeviceNotFoundError = ref(false);

  const canOpenSettingsForDevice = computed(
    () => status.value === RecordingStatus.Error && isDeviceNotFoundError.value
  );

  // Флаг: RecordingPopover подхватит и откроет ProfilePopover с секцией лицензии
  const wantsLicenseActivation = ref(false);

  function openLicenseActivation() {
    wantsLicenseActivation.value = true;
  }

  const visibleAccumulatedText = computed(() => {
    return animatedAccumulatedText.value || accumulatedText.value;
  });

  const visiblePartialText = computed(() => {
    return animatedPartialText.value || partialText.value;
  });

  const hasVisibleTranscriptionText = computed(() => {
    // В UI обычно показываем final + анимированный accumulated + анимированный partial.
    // Но на некоторых переходах (или если анимация временно выключена/сброшена) реальные данные могут быть в raw полях.
    // Поэтому считаем "есть текст" по обоим источникам — так UI-стили не зависят от анимационного слоя.
    const visible = `${finalText.value} ${visibleAccumulatedText.value} ${visiblePartialText.value}`.trim();
    return visible.length > 0;
  });

  const isListeningPlaceholder = computed(() => {
    return status.value === RecordingStatus.Recording && !hasVisibleTranscriptionText.value;
  });

  const isConnectingPlaceholder = computed(() => {
    return status.value === RecordingStatus.Starting && !hasVisibleTranscriptionText.value;
  });

  const displayText = computed(() => {
    const t = i18n.global.t;
    // Показываем: финальный текст + анимированный накопленный + анимированный промежуточный
    const final = finalText.value;
    const accumulated = visibleAccumulatedText.value;
    const partial = visiblePartialText.value;

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
      return t('main.idlePrompt');
    }

    // Во время Starting/Recording показываем пустую строку или "Listening..."
    if (status.value === RecordingStatus.Starting) {
      return t('main.connecting');
    }

    if (status.value === RecordingStatus.Recording) {
      return t('main.listening');
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

    if (!isTauriAvailable()) {
      const message = i18n.global.t('main.tauriUnavailable');
      console.warn(message);
      error.value = message;
      errorType.value = null;
      status.value = RecordingStatus.Error;
      return;
    }

    // Отписываемся от старых listeners перед регистрацией новых
    // Это предотвращает дублирование событий при повторной инициализации
    cleanup();

    try {
      // Listen to partial transcription events
      unlistenPartial = await listen<PartialTranscriptionPayload>(
        EVENT_TRANSCRIPTION_PARTIAL,
        async (event) => {
          if (!ensureActiveSessionForIncomingEvent(event.payload.session_id, 'transcription:partial')) {
            return;
          }

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
          if (!ensureActiveSessionForIncomingEvent(event.payload.session_id, 'transcription:final')) {
            return;
          }

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
          const nextStatus = event.payload.status;
          const payloadSessionId = event.payload.session_id;
          const isStartLike =
            nextStatus === RecordingStatus.Starting ||
            nextStatus === RecordingStatus.Recording;

          bumpLastSeenSessionId(payloadSessionId);

          // Если сессия уже помечена как "закрытая" — игнорируем любые её статусы,
          // иначе UI может "ожить" старым Recording спустя время (на скрытом окне).
          if (payloadSessionId <= closedSessionIdFloor.value) {
            console.warn('[STT] Ignoring status from closed session:', {
              payloadSessionId,
              closedFloor: closedSessionIdFloor.value,
              nextStatus,
            });
            return;
          }

          // Важно: статус Idle выставляем максимально рано, чтобы UI не мог "залипнуть" в Recording
          // из-за долгих await внутри обработчика (например copy_to_clipboard) перед автоскрытием окна.
          //
          // Для Error так делать нельзя — иначе сломаем suppression во время connect-retry.
          if (nextStatus === RecordingStatus.Idle) {
            status.value = RecordingStatus.Idle;
          }

          // Звук теперь воспроизводится раньше - в handleHotkeyToggle
          // Оставляем этот код закомментированным для справки
          // if (event.payload.status === RecordingStatus.Starting && status.value !== RecordingStatus.Starting) {
          //   console.log('Recording starting - playing show sound');
          //   playShowSound();
          // }

          // Пока мы ждём старт новой сессии — принимаем только Starting/Recording.
          // Любые Idle/Error от старой сессии здесь ломают UX (окно открыли → а UI внезапно "Idle").
          if (awaitingSessionStart.value) {
            if (!isStartLike) {
              return;
            }
            awaitingSessionStart.value = false;
          }

          // Если пришёл статус НЕ от текущей сессии — игнорируем (особенно важно для позднего Idle).
          // Исключение: Starting/Recording считаем началом новой сессии (например, старт инициирован Rust-стороной).
          if (!isStartLike && sessionId.value !== null && payloadSessionId !== sessionId.value) {
            console.warn('[STT] Ignoring status from stale session:', {
              payloadSessionId,
              activeSessionId: sessionId.value,
              nextStatus,
            });
            return;
          }

          // Начало новой сессии: фиксируем sessionId и чистим текст/ошибки.
          const prevSessionId = sessionId.value;
          if (isStartLike && payloadSessionId !== prevSessionId) {
            sessionId.value = payloadSessionId;
          }

          // Если статус стал Starting или Recording - очищаем весь текст
          // Это работает и для кнопки, и для hotkey (Ctrl+X)
          const isNewSession = isStartLike && payloadSessionId !== prevSessionId;
          if (isStartLike && (isNewSession
              || (status.value !== RecordingStatus.Starting && status.value !== RecordingStatus.Recording))) {
            console.log('Recording starting/started - clearing all text');
            partialText.value = '';
            accumulatedText.value = '';
            finalText.value = '';
            lastFinalizedText.value = '';
            currentUtteranceStart.value = -1;
            error.value = null;
            errorType.value = null;
            isDeviceNotFoundError.value = false;

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
          if (nextStatus === RecordingStatus.Idle) {
            console.log('🔄 Запись остановлена - обрабатываем текущий текст');

            // Собираем весь видимый текст (final + accumulated + partial)
            const currentText = [finalText.value, accumulatedText.value, partialText.value]
              .filter(Boolean)
              .join(' ')
              .trim();

            // Если остановка была через hotkey — для UX важнее "чистый лист" на следующем открытии,
            // поэтому закрываем сессию сразу (поздние partial/final не должны оживлять UI).
            if (event.payload.stopped_via_hotkey) {
              markSessionsClosed(payloadSessionId, 'stopped_via_hotkey:Idle');
              sessionId.value = null;
              awaitingSessionStart.value = false;
            }

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

            // UX: после остановки через hotkey окно сразу скрывается.
            // Следующее открытие должно начинаться с "чистого листа", без текста прошлой сессии.
            if (event.payload.stopped_via_hotkey) {
              resetTextStateBeforeStart();
            }
          }

          // Если прилетает Error после auth-ошибки, не показываем это пользователю.
          // В commands.rs сначала эмитится transcription:error, потом recording:status=Error.
          // Не меняем status — retry loop или auth handler сами определят следующее состояние.
          // (Раньше ставили Idle, что вызывало мигание "Подключение → Нажмите кнопку → Подключение".)
          if (nextStatus === RecordingStatus.Error && suppressNextErrorStatus) {
            suppressNextErrorStatus = false;
            return;
          }

          // Если сейчас идёт подключение с ретраями — не переключаем UI в Error мгновенно.
          // Решение о показе ошибки принимает retry-цикл, чтобы не мигала красная плашка.
          if (nextStatus === RecordingStatus.Error && isConnecting.value) {
            console.warn('[ConnectRetry] Got RecordingStatus.Error during connect attempt - waiting for retry decision');
            return;
          }

          // Фоновая ошибка после остановки записи (keep-alive/таймаут провайдера и т.п.)
          // Пользователь уже закончил запись — не надо переводить UI в Error.
          if (nextStatus === RecordingStatus.Error && !isConnecting.value) {
            const current = status.value;
            if (current === RecordingStatus.Idle || current === RecordingStatus.Processing) {
              console.warn('[STT] Ignoring background Error status while not recording:', event.payload);
              status.value = RecordingStatus.Idle;
              return;
            }
          }

          status.value = nextStatus;

          // Если упали в Error — закрываем сессию, чтобы поздние события не перетёрли UI.
          if (nextStatus === RecordingStatus.Error) {
            sessionId.value = null;
            awaitingSessionStart.value = false;
          }
        }
      );

      // Listen to transcription error events
      unlistenError = await listen<TranscriptionErrorPayload>(
        EVENT_TRANSCRIPTION_ERROR,
        async (event) => {
          if (!ensureActiveSessionForIncomingEvent(event.payload.session_id, 'transcription:error')) {
            return;
          }

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

          // Auth ошибка: чаще всего это 401 от нашего backend WS из-за протухшего access token.
          // Сначала даём retry-циклу шанс обновить токен и переподключиться.
          const detectedFromRaw = detectErrorTypeFromRaw(event.payload.error);
          if (event.payload.error_type === 'authentication' || detectedFromRaw === 'authentication') {
            errorType.value = 'authentication';
            suppressNextErrorStatus = true;

            lastConnectFailure.value = 'authentication';
            lastConnectFailureRaw.value = event.payload.error;

            // Если мы не в цикле подключения (например, ошибка пришла "фоном"),
            // попробуем тихо обновить токен. Если не получилось — тогда уже разлогиниваем.
            if (!isConnecting.value) {
              const wasStarting = status.value === RecordingStatus.Starting;
              const ok = await tryRefreshAuthForStt();
              if (!ok) {
                void forceLogoutFromSttAuthError();
              } else if (wasStarting) {
                // Запись была инициирована (хоткей/кнопка), но токен протух.
                // Токен обновлён — перезапускаем автоматически.
                void startRecording();
              } else {
                status.value = RecordingStatus.Idle;
              }
            }
            return;
          }

          // 429: слишком много сессий или rate limit от сервера.
          // Если запись только стартовала — пробуем один auto-retry с задержкой.
          const isRateLimited = event.payload.error_details?.category === 'rate_limited'
            || event.payload.error_details?.httpStatus === 429;

          // Лимит подписки исчерпан — показываем сразу, без retry
          const isLimitExceeded = event.payload.error_type === 'limit_exceeded'
            || event.payload.error_details?.category === 'limit_exceeded';

          if (isLimitExceeded) {
            // Уже обработали — не дёргаем API повторно
            if (errorType.value === 'limit_exceeded') return;
            // Пробуем получить детальную информацию об использовании для наглядного сообщения
            let usageMessage = mapErrorMessage('limit_exceeded', event.payload.error, event.payload.error_details);
            try {
              const data = await api.get<{ licenses: Array<{ status: string; plan: string; seconds_used: number; seconds_limit: number }> }>('/api/v1/account/licenses');
              const lic = data.licenses.find(l => l.status === 'active') ?? data.licenses[0];
              if (lic) {
                const usedMin = Math.round(lic.seconds_used / 60);
                const totalMin = Math.round(lic.seconds_limit / 60);
                const planKey = `profile.plans.${lic.plan}`;
                const planName = i18n.global.t(planKey) !== planKey
                  ? i18n.global.t(planKey)
                  : lic.plan;
                usageMessage = i18n.global.t('errors.limitExceededDetailed', { plan: planName, used: usedMin, total: totalMin });
              }
            } catch {}
            error.value = usageMessage;
            errorType.value = 'limit_exceeded';
            status.value = RecordingStatus.Error;
            return;
          }

          if (isRateLimited && !isConnecting.value) {
            const wasStarting = status.value === RecordingStatus.Starting;
            const serverCode = event.payload.error_details?.serverCode;

            if (wasStarting && rateLimitRetryCount < RATE_LIMIT_MAX_RETRIES) {
              rateLimitRetryCount++;
              const delaySec = serverCode === 'TOO_MANY_SESSIONS' ? 2 : 5;
              console.warn(`[STT] 429 (${serverCode ?? 'unknown'}), auto-retry #${rateLimitRetryCount} через ${delaySec}с`);
              suppressNextErrorStatus = true;
              status.value = RecordingStatus.Starting;
              setTimeout(() => {
                if (status.value === RecordingStatus.Starting) {
                  // Важно: вызываем напрямую startRecordingWithRetry, а не startRecording,
                  // чтобы не сбросить rateLimitRetryCount (он нужен для защиты от бесконечных ретраев).
                  void startRecordingWithRetry(3);
                }
              }, delaySec * 1000);
            } else {
              // Исчерпали лимит ретраев или не в Starting — показываем ошибку
              if (rateLimitRetryCount >= RATE_LIMIT_MAX_RETRIES) {
                console.warn(`[STT] 429 retry limit reached (${RATE_LIMIT_MAX_RETRIES}), showing error`);
              }
              rateLimitRetryCount = 0;
              error.value = mapErrorMessage('connection', event.payload.error, event.payload.error_details);
              errorType.value = 'connection';
              status.value = RecordingStatus.Error;
            }
            return;
          }

          // Фоновая ошибка после остановки записи (keep-alive, таймаут провайдера, и т.п.)
          // Если пользователь сейчас не записывает и не подключается — игнорируем, чтобы не "залипать" в Error.
          if (!isConnecting.value) {
            const current = status.value;
            if (current === RecordingStatus.Idle || current === RecordingStatus.Processing) {
              console.warn('[STT] Ignoring background error while not recording:', event.payload);
              return;
            }
          }

          // Во время подключения подавляем показ ошибки и даём retry-циклу принять решение.
          // Это убирает "Проблема с подключением" на первой же неудачной попытке.
          if (isConnecting.value) {
            // Лимит подписки — нет смысла ретраить, прерываем цикл и показываем ошибку сразу
            const connectIsLimitExceeded = event.payload.error_type === 'limit_exceeded'
              || event.payload.error_details?.category === 'limit_exceeded';
            if (connectIsLimitExceeded) {
              // Уже обработали — не дёргаем API повторно
              if (errorType.value === 'limit_exceeded') return;
              isConnecting.value = false;
              let usageMessage = mapErrorMessage('limit_exceeded', event.payload.error, event.payload.error_details);
              try {
                const data = await api.get<{ licenses: Array<{ status: string; plan: string; seconds_used: number; seconds_limit: number }> }>('/api/v1/account/licenses');
                const lic = data.licenses.find(l => l.status === 'active') ?? data.licenses[0];
                if (lic) {
                  const usedMin = Math.round(lic.seconds_used / 60);
                  const totalMin = Math.round(lic.seconds_limit / 60);
                  const planKey = `profile.plans.${lic.plan}`;
                  const planName = i18n.global.t(planKey) !== planKey
                    ? i18n.global.t(planKey)
                    : lic.plan;
                  usageMessage = i18n.global.t('errors.limitExceededDetailed', { plan: planName, used: usedMin, total: totalMin });
                }
              } catch {}
              error.value = usageMessage;
              errorType.value = 'limit_exceeded';
              status.value = RecordingStatus.Error;
              return;
            }

            // error_type может быть любым (backend иногда присылает PROVIDER_ERROR и т.п.)
            // Нормализуем к нашим типам, иначе retry-цикл может не понять, что произошло.
            lastConnectFailure.value =
              asKnownErrorType(event.payload.error_type) ??
              detectErrorTypeFromRaw(event.payload.error) ??
              'connection';
            lastConnectFailureRaw.value = event.payload.error;
            lastConnectFailureDetails.value = event.payload.error_details ?? null;
            console.warn('[ConnectRetry] Suppressed error during connect:', event.payload);
            return;
          }

          // Остальные ошибки показываем пользователю
          const normalizedType =
            asKnownErrorType(event.payload.error_type) ??
            detectErrorTypeFromRaw(event.payload.error) ??
            'connection';

          // Защита от даунгрейда: если уже показали limit_exceeded, не перезаписываем
          // менее конкретной ошибкой (send_audio() шлёт "connection" пока audio loop
          // добивает retry-цикл после закрытия WS сервером).
          if (errorType.value === 'limit_exceeded' && normalizedType !== 'limit_exceeded') {
            console.warn('[STT] Skipping error downgrade from limit_exceeded to', normalizedType);
            return;
          }

          error.value = mapErrorMessage(normalizedType, event.payload.error, event.payload.error_details);
          errorType.value = normalizedType;
          isDeviceNotFoundError.value =
            normalizedType === 'configuration' && isDeviceNotFoundInRaw(event.payload.error);
          status.value = RecordingStatus.Error;
        }
      );

      // Listen to connection quality events
      unlistenConnectionQuality = await listen<ConnectionQualityPayload>(
        EVENT_CONNECTION_QUALITY,
        (event) => {
          if (!ensureActiveSessionForIncomingEvent(event.payload.session_id, 'connection:quality')) {
            return;
          }

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

  function isDeviceNotFoundInRaw(raw: string): boolean {
    const lower = String(raw ?? '').toLowerCase();
    return (
      lower.includes('device not found') ||
      lower.includes("device '") ||
      lower.includes('не удалось инициализировать устройство') ||
      lower.includes('failed to create audio capture with device')
    );
  }

  function isLicenseInactiveFromRaw(raw: string): boolean {
    const lower = String(raw ?? '').toLowerCase();
    return (
      lower.includes('license_inactive') ||
      lower.includes('license inactive') ||
      lower.includes('лицензия не активна')
    );
  }

  function detectErrorTypeFromRaw(raw: string): TranscriptionErrorPayload['error_type'] | null {
    const lower = raw.toLowerCase();
    // Ошибки захвата аудио/микрофона часто прилетают как raw строка из invoke('start_recording'),
    // в т.ч. с суффиксом "(type: processing)".
    if (
      lower.includes('(type: processing)') ||
      lower.includes('type: processing') ||
      lower.includes('failed to start audio capture') ||
      lower.includes('capture error')
    ) {
      return 'processing';
    }
    // Устройство отключено/отсоединено во время записи
    if (isAudioDeviceUnavailableFromRaw(raw)) {
      return 'processing';
    }
    // Выбранное устройство недоступно (не найдено в списке) — нужно сменить в настройках
    if (isDeviceNotFoundInRaw(raw)) {
      return 'configuration';
    }
    if (isLicenseInactiveFromRaw(raw)) {
      return 'limit_exceeded';
    }
    if (
      lower.includes('authentication error') ||
      lower.includes('401') ||
      lower.includes('unauthorized') ||
      (lower.includes('token') && lower.includes('auth'))
    ) {
      return 'authentication';
    }
    if (lower.includes('timeout') || lower.includes('timed out')) return 'timeout';
    if (lower.includes('limit_exceeded') || lower.includes('limit exceeded') || lower.includes('usage limit')) return 'limit_exceeded';
    if (lower.includes('connection error') || lower.includes('websocket')) return 'connection';
    if (lower.includes('configuration error')) return 'configuration';
    if (lower.includes('processing error')) return 'processing';
    return null;
  }

  function formatUnknownError(err: unknown): string {
    if (typeof err === 'string') return err;
    if (err && typeof err === 'object') {
      const anyErr = err as { message?: unknown; error?: unknown; details?: unknown; toString?: unknown };
      const message = typeof anyErr.message === 'string' ? anyErr.message : null;
      const error = typeof anyErr.error === 'string' ? anyErr.error : null;
      const details = typeof anyErr.details === 'string' ? anyErr.details : null;
      const merged = [message, error, details].filter(Boolean).join(' | ');
      if (merged) return merged;
      try {
        return JSON.stringify(err);
      } catch {
        // Fallback: хотя бы не теряем тип
        return Object.prototype.toString.call(err);
      }
    }
    return String(err ?? '');
  }

  function asKnownErrorType(value: unknown): TranscriptionErrorPayload['error_type'] | null {
    if (value === 'timeout') return 'timeout';
    if (value === 'connection') return 'connection';
    if (value === 'configuration') return 'configuration';
    if (value === 'processing') return 'processing';
    if (value === 'authentication') return 'authentication';
    if (value === 'limit_exceeded') return 'limit_exceeded';
    return null;
  }

  function isOffline(): boolean {
    try {
      // navigator.onLine в Tauri работает, но иногда даёт false positives,
      // поэтому используем это только как "точный" сигнал офлайна.
      if (typeof navigator === 'undefined') return false;
      if (typeof navigator.onLine !== 'boolean') return false;
      return navigator.onLine === false;
    } catch {
      return false;
    }
  }

  function extractHttpStatusFromRaw(raw: string): number | null {
    // Примеры raw:
    // - "WS connection failed: HTTP error: 503 Service Unavailable"
    // - "WS connection failed: HTTP error: 502"
    const match = String(raw ?? '').match(/\bHTTP error:\s*(\d{3})\b/i);
    if (!match) return null;
    const status = Number(match[1]);
    return Number.isFinite(status) ? status : null;
  }

  function mapConnectionErrorMessage(
    raw: string,
    details: TranscriptionErrorPayload['error_details'] | null | undefined
  ): string {
    const category = details?.category;
    if (category) {
      if (category === 'offline') return i18n.global.t('errors.connectionOffline');
      if (category === 'dns') return i18n.global.t('errors.connectionDns');
      if (category === 'tls') return i18n.global.t('errors.connectionTls');
      if (category === 'timeout') return i18n.global.t('errors.timeout');
      if (category === 'limit_exceeded') return i18n.global.t('errors.limitExceeded');
      if (category === 'rate_limited') return i18n.global.t('errors.rateLimited');
      if (category === 'http') {
        return details?.httpStatus
          ? i18n.global.t('errors.connectionHttp', { status: details.httpStatus })
          : i18n.global.t('errors.connection');
      }
      if (
        category === 'server_unavailable' ||
        category === 'refused' ||
        category === 'reset' ||
        category === 'closed'
      ) {
        return i18n.global.t('errors.connectionServerUnavailable');
      }
    }

    const text = String(raw ?? '');
    const lower = text.toLowerCase();

    if (isOffline()) return i18n.global.t('errors.connectionOffline');

    // Иногда timeout прилетает в connection типе — лучше показать явный timeout текст.
    if (lower.includes('timeout') || lower.includes('timed out')) {
      return i18n.global.t('errors.timeout');
    }

    const httpStatus = extractHttpStatusFromRaw(text);
    if (httpStatus) {
      // 502/503/504 часто выглядят как "сервер перезапускается/обновляется" (в т.ч. hot reload).
      if (httpStatus === 502 || httpStatus === 503 || httpStatus === 504) {
        return i18n.global.t('errors.connectionServerUnavailable');
      }
      return i18n.global.t('errors.connectionHttp', { status: httpStatus });
    }

    // DNS/резолвинг (часто VPN/прокси/нет интернета)
    if (
      lower.includes('dns') ||
      lower.includes('enotfound') ||
      lower.includes('failed to lookup') ||
      lower.includes('name or service not known') ||
      lower.includes('nodename nor servname provided') ||
      lower.includes('could not resolve')
    ) {
      return i18n.global.t('errors.connectionDns');
    }

    // TLS/сертификаты/SSL
    if (
      lower.includes('tls') ||
      lower.includes('ssl') ||
      lower.includes('certificate') ||
      lower.includes('invalid peer certificate') ||
      lower.includes('unknown issuer')
    ) {
      return i18n.global.t('errors.connectionTls');
    }

    // Похоже на рестарт/обрыв сокета: connection refused/reset/broken pipe и т.п.
    if (
      lower.includes('connection refused') ||
      lower.includes('econnrefused') ||
      lower.includes('os error 61') || // macOS: connection refused
      lower.includes('os error 111') || // linux: connection refused
      lower.includes('connection reset') ||
      lower.includes('reset by peer') ||
      lower.includes('broken pipe') ||
      lower.includes('connection closed') ||
      lower.includes('unexpected eof') ||
      lower.includes('handshake') ||
      lower.includes('websocket')
    ) {
      return i18n.global.t('errors.connectionServerUnavailable');
    }

    return i18n.global.t('errors.connection');
  }

  function mapErrorMessage(
    type: TranscriptionErrorPayload['error_type'] | null,
    raw: string,
    details?: TranscriptionErrorPayload['error_details'] | null
  ): string {
    switch (type) {
      case 'timeout':
        return i18n.global.t('errors.timeout');
      case 'connection':
        return mapConnectionErrorMessage(raw, details);
      case 'limit_exceeded':
        return i18n.global.t('errors.limitExceeded');
      case 'processing':
        return mapProcessingErrorMessage(raw);
      case 'authentication':
        // По идее мы сюда не попадаем (auth ошибка приводит к auto-logout),
        // но оставляем адекватный текст на всякий случай.
        return i18n.global.t('errors.authentication');
      case 'configuration':
        if (
          raw.toLowerCase().includes('device not found') ||
          raw.toLowerCase().includes('не удалось инициализировать устройство') ||
          raw.toLowerCase().includes('failed to create audio capture with device')
        ) {
          return i18n.global.t('errors.audioDeviceNotFound');
        }
        return i18n.global.t('errors.generic', { error: raw });
      default:
        return i18n.global.t('errors.generic', { error: raw });
    }
  }

  function isAudioDeviceUnavailableFromRaw(raw: string): boolean {
    const lower = String(raw ?? '').toLowerCase();
    return (
      // Пример (cpal/rodio): "The requested device is no longer available. For example, it has been unplugged."
      lower.includes('device is no longer available') ||
      lower.includes('requested device is no longer available') ||
      lower.includes('has been unplugged') ||
      // Частые обёртки вокруг этой причины
      (lower.includes('failed to build audio stream') && (lower.includes('device') || lower.includes('stream'))) ||
      (lower.includes('failed to start audio capture') && lower.includes('device'))
    );
  }

  function mapProcessingErrorMessage(raw: string): string {
    if (isAudioDeviceUnavailableFromRaw(raw)) {
      return i18n.global.t('errors.audioDeviceUnavailable');
    }
    return i18n.global.t('errors.processing');
  }

  function sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  function calcBackoffMs(attemptIndex: number): number {
    // attemptIndex: 1..N
    // Плавный backoff: 600ms, 1200ms, 2000ms, 3000ms...
    const base = [600, 1200, 2000, 3000, 4000][attemptIndex - 1] ?? 5000;
    const jitter = Math.floor(Math.random() * 250);
    return base + jitter;
  }

  async function waitForConnectOutcome(timeoutMs: number): Promise<void> {
    return new Promise((resolve, reject) => {
      let finished = false;
      let stop: (() => void) | null = null;
      let timer: ReturnType<typeof setTimeout> | null = null;

      const finishOk = () => {
        if (finished) return;
        finished = true;
        if (timer) clearTimeout(timer);
        if (stop) stop();
        resolve();
      };

      const finishErr = (type: TranscriptionErrorPayload['error_type']) => {
        if (finished) return;
        finished = true;
        if (timer) clearTimeout(timer);
        if (stop) stop();
        reject(type);
      };

      // Мгновенные проверки перед подпиской, чтобы избежать гонок с immediate-watch
      if (status.value === RecordingStatus.Recording) {
        finishOk();
        return;
      }
      if (lastConnectFailure.value) {
        finishErr(lastConnectFailure.value);
        return;
      }

      stop = watch(
        [status, lastConnectFailure],
        ([nextStatus, failure]) => {
          if (finished) return;
          if (nextStatus === RecordingStatus.Recording) {
            finishOk();
            return;
          }
          if (failure) {
            finishErr(failure);
          }
        }
      );

      timer = setTimeout(() => {
        if (finished) return;
        finishErr('timeout');
      }, timeoutMs);
    });
  }

  async function forceLogoutFromSttAuthError(): Promise<void> {
    if (isForcingLogout) return;
    isForcingLogout = true;

    try {
      // 1) Чистим локальную сессию
      try {
        await getTokenRepository().clear();
      } catch {}

      // 2) Сбрасываем auth store (это переключит окно на auth через watcher в App.vue)
      try {
        const authStore = useAuthStore();
        authStore.reset();
      } catch {}

      // 3) И гарантируем, что auth окно показано (fallback)
      try {
        await invoke('show_auth_window');
      } catch {}
    } finally {
      // Важно: не оставляем UI в error состоянии.
      status.value = RecordingStatus.Idle;
      error.value = null;
      errorType.value = null;
      isDeviceNotFoundError.value = false;
      isForcingLogout = false;
    }
  }

  async function tryRefreshAuthForStt(): Promise<boolean> {
    if (refreshAuthForSttPromise) return refreshAuthForSttPromise;

    refreshAuthForSttPromise = (async () => {
      try {
        const tokenRepo = getTokenRepository();
        const session = await tokenRepo.get();
        if (!session) return false;

        // Если refresh невозможен — смысла пытаться нет.
        if (!canRefreshSession(session)) return false;

        const container = getAuthContainer();
        const refreshed = await container.refreshTokensUseCase.execute();
        if (!refreshed) return false;

        // Обновляем UI состояние (isAuthenticated остаётся true, но токен меняется)
        try {
          const authStore = useAuthStore();
          authStore.setAuthenticated(refreshed);
        } catch {}

        return true;
      } catch (err) {
        console.warn('[STT] Failed to refresh auth for STT:', err);
        return false;
      }
    })();

    try {
      return await refreshAuthForSttPromise;
    } finally {
      refreshAuthForSttPromise = null;
    }
  }

  function resetTextStateBeforeStart(): void {
    // Очищаем весь предыдущий текст перед новой записью
    error.value = null;
    errorType.value = null;
    isDeviceNotFoundError.value = false;
    partialText.value = '';
    accumulatedText.value = '';
    finalText.value = '';
    lastFinalizedText.value = '';
    currentUtteranceStart.value = -1;

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

  async function startRecordingOnce(): Promise<void> {
    // Начинаем новую сессию "с чистого листа": пока не получим Starting/Recording с новым session_id,
    // игнорируем любые поздние события от прошлых запусков.
    awaitingSessionStart.value = true;
    sessionId.value = null;

    resetTextStateBeforeStart();
    status.value = RecordingStatus.Starting;

    // На каждый запуск сбрасываем маркеры исхода подключения
    lastConnectFailure.value = null;
    lastConnectFailureRaw.value = '';

    console.log('[ConnectRetry] Starting recording (single attempt)');
    await invoke<string>('start_recording');
  }

  async function startRecordingWithRetry(maxAttempts = 3): Promise<void> {
    // Не запускаем два подключения одновременно
    if (isConnecting.value) {
      console.log('[ConnectRetry] Skipped - connect already in progress');
      return;
    }

    isConnecting.value = true;
    connectAttempt.value = 0;
    connectMaxAttempts.value = Math.max(1, maxAttempts);
    let authRefreshUsed = false;

    try {
      for (let attempt = 1; attempt <= connectMaxAttempts.value; attempt++) {
        connectAttempt.value = attempt;
        lastConnectFailure.value = null;
        lastConnectFailureRaw.value = '';

        try {
          // Перед первой попыткой гарантируем, что access token свежий.
          // Иначе backend WS легко вернёт 401 (access TTL ~15 минут), и UI начнёт "разлогинивать" пользователя.
          if (attempt === 1) {
            const tokenRepo = getTokenRepository();
            const session = await tokenRepo.get();
            if (session && isAccessTokenExpired(session)) {
              await tryRefreshAuthForStt();
            }
          }

          // Перед ретраем аккуратно пробуем остановить возможный "полузапущенный" поток.
          // Если он не стартанул — просто игнорируем ошибку.
          if (attempt > 1) {
            try {
              await invoke('stop_recording');
            } catch {}
          }

          await startRecordingOnce();

          // Ждём пока backend реально переведёт нас в Recording или пришлёт ошибку
          await waitForConnectOutcome(12_000);

          console.log('[ConnectRetry] Connected successfully');
          rateLimitRetryCount = 0;
          return;
    } catch (err) {
          // ВАЖНО: err может быть либо "типом" (timeout/connection/...) из waitForConnectOutcome,
          // либо сырой строкой ошибки из invoke('start_recording').
          // Нельзя интерпретировать любую строку как error_type.
          const failureType = asKnownErrorType(err);

          // Если ошибка пришла не через events, пробуем классифицировать по raw строке
          const raw = lastConnectFailureRaw.value || formatUnknownError(err);
          const details = lastConnectFailureDetails.value;
          const detected = failureType || detectErrorTypeFromRaw(raw) || 'connection';

          const httpStatus = details?.httpStatus ?? extractHttpStatusFromRaw(raw);
          const serverCode = details?.serverCode;
          const isLimitExceeded =
            details?.category === 'limit_exceeded' ||
            serverCode === 'LIMIT_EXCEEDED' ||
            serverCode === 'LICENSE_INACTIVE' ||
            isLicenseInactiveFromRaw(raw);

          // Auth ошибка: обычно это протухший access token.
          // Пробуем один раз обновить сессию и продолжить retry-цикл.
          if (detected === 'authentication') {
            // Если уже успешно обновляли токены, но всё равно получаем 401 — значит токен не подходит
            // (или backend всё ещё использует старый). Не оставляем UI в "Подключение..." бесконечно.
            if (authRefreshUsed) {
              errorType.value = 'authentication';
              suppressNextErrorStatus = true;
              await forceLogoutFromSttAuthError();
              return;
            }

            const ok = await tryRefreshAuthForStt();
            if (ok) {
              authRefreshUsed = true;
              console.warn('[ConnectRetry] Auth refreshed, retrying connection');
              // Refresh не должен "съедать" попытку подключения — иначе можно выйти из цикла
              // без финального результата и залипнуть в Starting.
              attempt = Math.max(0, attempt - 1);
              continue;
            }
            errorType.value = 'authentication';
            suppressNextErrorStatus = true;
            await forceLogoutFromSttAuthError();
            return;
          }

          if (isLimitExceeded) {
            errorType.value = 'limit_exceeded';
            error.value = mapErrorMessage('limit_exceeded', raw, details);
            status.value = RecordingStatus.Error;
            return;
          }

          const isRetriable = detected === 'connection' || detected === 'timeout';
          const isLastAttempt = attempt >= connectMaxAttempts.value;
          const isRateLimited =
            details?.category === 'rate_limited' ||
            // Fallback: иногда category не проставляется, но сервер код есть.
            serverCode === 'RATE_LIMIT_EXCEEDED' ||
            serverCode === 'TOO_MANY_SESSIONS' ||
            // Если кода нет, но это 429 — бэкоффим как rate limit (кроме limit_exceeded, см. выше).
            httpStatus === 429;

          console.warn('[ConnectRetry] Connect attempt failed:', {
            attempt,
            detected,
            isRetriable,
            isLastAttempt,
            raw,
          });

          if (!isRetriable || isLastAttempt) {
            errorType.value = detected;
            error.value = mapErrorMessage(detected, raw, details);
            isDeviceNotFoundError.value = detected === 'configuration' && isDeviceNotFoundInRaw(raw);
            status.value = RecordingStatus.Error;
            return;
          }

          // Короткая пауза перед следующей попыткой
          let backoffMs = calcBackoffMs(attempt);
          if (isRateLimited) {
            const jitter = Math.floor(Math.random() * 250);
            // 429 нельзя ретраить "быстро": иначе сами усугубляем лимит.
            // При TOO_MANY_SESSIONS обычно достаточно пары секунд (сервер успевает закрыть старую сессию).
            if (serverCode === 'TOO_MANY_SESSIONS') {
              backoffMs = 2000 + jitter;
            } else if (serverCode === 'RATE_LIMIT_EXCEEDED') {
              backoffMs = 5000 + jitter;
            } else {
              backoffMs = 4000 + jitter;
            }
          }
          await sleep(backoffMs);
        }
      }

      // Защита: на всякий случай не оставляем UI в Starting без исхода.
      // Это может случиться, если все попытки завершились continue (например, серия auth refresh),
      // или если события от backend потерялись/были отфильтрованы.
      const fallbackType = lastConnectFailure.value ?? 'connection';
      const fallbackRaw = lastConnectFailureRaw.value || 'Unknown connection error';
      errorType.value = fallbackType;
      error.value = mapErrorMessage(fallbackType, fallbackRaw, lastConnectFailureDetails.value);
      isDeviceNotFoundError.value =
        fallbackType === 'configuration' && isDeviceNotFoundInRaw(fallbackRaw);
      status.value = RecordingStatus.Error;
    } finally {
      isConnecting.value = false;
      connectAttempt.value = 0;
      connectMaxAttempts.value = 0;
      lastConnectFailure.value = null;
      lastConnectFailureRaw.value = '';
      lastConnectFailureDetails.value = null;
    }
  }

  async function startRecording(): Promise<void> {
    // Пользовательский вызов — сбрасываем счётчик rate limit,
    // чтобы прошлые авто-ретраи не блокировали новый запуск.
    rateLimitRetryCount = 0;
    await startRecordingWithRetry(3);
  }

  async function reconnect(): Promise<void> {
    rateLimitRetryCount = 0;
    await startRecordingWithRetry(3);
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

  function clearText() {
    // Сбрасываем "текущую" сессию, чтобы любые поздние события от предыдущего запуска
    // не смогли снова заполнить UI текстом после очистки.
    sessionId.value = null;
    awaitingSessionStart.value = false;

    resetTextStateBeforeStart();
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

  return {
    // State
    status,
    partialText,
    accumulatedText,
    finalText,
    error,
    errorType,
    connectionQuality,

    // Computed
    isStarting,
    isRecording,
    isIdle,
    isProcessing,
    hasError,
    hasConnectionIssue,
    canReconnect,
    canActivateLicense,
    canOpenSettingsForDevice,
    wantsLicenseActivation,
    isConnecting,
    connectAttempt,
    connectMaxAttempts,
    hasVisibleTranscriptionText,
    isListeningPlaceholder,
    isConnectingPlaceholder,
    displayText,

    // Actions
    initialize,
    startRecording,
    reconnect,
    openLicenseActivation,
    stopRecording,
    clearText,
    toggleRecording,
    reconcileBackendStatus,
    cleanup,
  };
});
