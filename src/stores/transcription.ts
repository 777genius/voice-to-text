import { defineStore } from 'pinia';
import { ref, computed, watch } from 'vue';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isTauriAvailable } from '../utils/tauri';
import { i18n } from '../i18n';
import { appendTranscriptText, mergeTranscriptText } from '../utils/transcriptionText';

const MAX_STREAMING_TRANSLATION_TEXT_CHARS = 32_000;

function keepRecentStreamingText(value: string): string {
  if (value.length <= MAX_STREAMING_TRANSLATION_TEXT_CHARS) return value;

  const tail = value.slice(-MAX_STREAMING_TRANSLATION_TEXT_CHARS);
  const firstWhitespace = tail.search(/\s/);
  return firstWhitespace >= 0 && firstWhitespace < 256
    ? tail.slice(firstWhitespace + 1)
    : tail;
}

function appendStreamingTranscriptText(current: string, next: string): string {
  return keepRecentStreamingText(appendTranscriptText(current, next));
}
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
  TranslationDeltaPayload,
  TranslationErrorPayload,
  IncomingTranslationStatusPayload,
  IncomingTranslationTextPayload,
  IncomingTranslationErrorPayload,
  LiveTranslationHealthCheck,
  EVENT_TRANSCRIPTION_PARTIAL,
  EVENT_TRANSCRIPTION_FINAL,
  EVENT_RECORDING_STATUS,
  EVENT_TRANSCRIPTION_ERROR,
  EVENT_CONNECTION_QUALITY,
  EVENT_TRANSLATION_DELTA,
  EVENT_TRANSLATION_ERROR,
  EVENT_INCOMING_TRANSLATION_STATUS,
  EVENT_INCOMING_TRANSLATION_SOURCE_FINAL,
  EVENT_INCOMING_TRANSLATION_DELTA,
  EVENT_INCOMING_TRANSLATION_ERROR,
} from '../types';
import type { RecordingMode } from '../types';

export const useTranscriptionStore = defineStore('transcription', () => {
  // State
  const status = ref<RecordingStatus>(RecordingStatus.Idle);
  // Идентификатор текущей сессии записи (приходит из backend в событиях).
  // Нужен, чтобы никогда не "протекал" текст из прошлой сессии в новую.
  const sessionId = ref<number | null>(null);
  // Сессии с id <= closedSessionIdFloor считаются "закрытыми".
  // Любые отложенные/поздние события от них игнорируем, чтобы UI не возвращался в старое состояние.
  const closedSessionIdFloor = ref<number>(0);
  // Отдельные session_id, закрытые точечно. Нужно для failed-start ошибок с большим id:
  // они не должны закрывать восстановленную более старую живую сессию.
  const closedSessionIds = ref<Set<number>>(new Set());
  // Максимальный session_id, который мы видели в status событиях.
  // Нужен, чтобы уметь "закрывать" последнюю сессию даже если часть событий потерялась.
  const lastSeenSessionId = ref<number>(0);
  // Флаг "ждём старт новой сессии": пока он true — игнорируем любые статусы/события,
  // которые не относятся к запуску новой записи (защита от поздних событий старого сокета).
  const awaitingSessionStart = ref<boolean>(false);
  const partialText = ref<string>(''); // текущий промежуточный сегмент
  const accumulatedText = ref<string>(''); // накопленные финализированные сегменты
  const finalText = ref<string>(''); // полный финальный результат (для копирования)
  const previousTranscriptionDisplaySuppressed = ref<boolean>(false);
  const suppressedPreviousSessionId = ref<number | null>(null);
  const error = ref<string | null>(null);
  const errorType = ref<TranscriptionErrorPayload['error_type'] | null>(null);
  const errorRaw = ref<string | null>(null);
  const errorDetails = ref<TranscriptionErrorPayload['error_details'] | null>(null);
  const isDeviceNotFoundError = ref(false);
  const lastFinalizedSegmentKey = ref<string>(''); // последний finalized range (для дедупликации)
  const lastSpeechFinalRangeKey = ref<string>(''); // последний speech_final range (для дедупликации)
  const connectionQuality = ref<ConnectionQuality>(ConnectionQuality.Good);

  // Live translation: режим активной сессии + накопленный перевод.
  // Обновляется из payload.mode у recording:status; live_translation сессии не идут через STT auto-paste.
  const activeRecordingMode = ref<RecordingMode>('dictation');
  const translationText = ref<string>('');

  // Incoming subtitles: system audio -> STT -> text translation. Separate lifecycle.
  const incomingTranslationStatus = ref<RecordingStatus>(RecordingStatus.Idle);
  const incomingTranslationSessionId = ref<number | null>(null);
  let incomingTranslationStatusEventVersion = 0;
  let incomingTerminalSessionId: number | null = null;
  const incomingClosedSessionIds = ref<Set<number>>(new Set());
  const incomingSourceText = ref<string>('');
  const incomingTranslationText = ref<string>('');
  const incomingTranslationError = ref<string | null>(null);
  const incomingTranslationCommandInFlight = ref(false);
  const liveTranslationHealthCheck = ref<LiveTranslationHealthCheck | null>(null);
  const liveTranslationHealthCheckLoading = ref<boolean>(false);
  const liveTranslationHealthCheckError = ref<string | null>(null);

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
  let rateLimitRetryTimer: ReturnType<typeof setTimeout> | null = null;
  let rateLimitRetryGeneration = 0;
  const RATE_LIMIT_MAX_RETRIES = 2;
  let isForcingLogout = false;
  let refreshAuthForSttPromise: Promise<boolean> | null = null;

  // Config flags - appConfig store is the single source of truth for auto actions.
  const appConfig = useAppConfigStore();
  // В live_translation auto-copy/paste/history намеренно отключены — перевод не должен попадать
  // в clipboard или активное окно. См. плана MVP, секция "Auto actions disabled".
  const autoCopyEnabled = computed(
    () => activeRecordingMode.value !== 'live_translation' && appConfig.autoCopyToClipboard
  );
  const autoPasteEnabled = computed(
    () => activeRecordingMode.value !== 'live_translation' && appConfig.autoPasteText
  );

  // Auth store — нужен, чтобы корректно сбрасывать ошибки записи после успешной авторизации,
  // если ошибка относилась к предыдущему пользователю/токену.
  const authStore = useAuthStore();

  function clearRecordingErrorState(): void {
    error.value = null;
    errorType.value = null;
    errorRaw.value = null;
    errorDetails.value = null;
    isDeviceNotFoundError.value = false;
  }

  function setRecordingError(
    type: TranscriptionErrorPayload['error_type'] | null,
    raw: string,
    details?: TranscriptionErrorPayload['error_details'] | null,
    displayMessage?: string,
  ): void {
    errorType.value = type;
    errorRaw.value = raw;
    errorDetails.value = details ?? null;
    error.value = displayMessage ?? mapErrorMessage(type, raw, details);
    isDeviceNotFoundError.value =
      type === 'configuration' && isDeviceNotFoundInRaw(raw);
  }

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
    clearRecordingErrorState();

    // И сбрасываем контекст последней неудачной попытки подключения,
    // чтобы новые попытки стартовали "с чистого листа".
    lastConnectFailure.value = null;
    lastConnectFailureRaw.value = '';
    lastConnectFailureDetails.value = null;
    suppressNextErrorStatus = false;
  }

  function clearRateLimitRetryTimer(): void {
    rateLimitRetryGeneration++;
    if (rateLimitRetryTimer) {
      clearTimeout(rateLimitRetryTimer);
      rateLimitRetryTimer = null;
    }
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

  // Отслеживание utterances по start времени
  const currentUtteranceStart = ref<number>(-1); // start время текущей utterance (-1 = нет активной)
  const lastPastedFinalText = ref<string>('');

  function normalizeForDedup(v: string): string {
    return String(v ?? '')
      .toLowerCase()
      .replace(/[\u200B-\u200D\uFEFF]/g, '')
      .replace(/[.,!?;:"'`(){}\[\]<>\-–—_\\/|@#$%^&*+=~]/g, ' ')
      .replace(/\s+/g, ' ')
      .trim();
  }

  function hasEnoughContextForVisibleOverlap(norm: string): boolean {
    if (!norm) return false;
    const words = norm.split(' ').filter(Boolean);
    return words.length >= 3 || norm.length >= 18;
  }

  function combineVisibleTranscriptParts(parts: string[]): string {
    let combined = '';

    for (const part of parts) {
      const next = String(part ?? '').trim();
      if (!next) continue;
      if (!combined) {
        combined = next;
        continue;
      }

      const combinedNorm = normalizeForDedup(combined);
      const nextNorm = normalizeForDedup(next);
      const canUseVisibleOverlap =
        hasEnoughContextForVisibleOverlap(combinedNorm) ||
        hasEnoughContextForVisibleOverlap(nextNorm);
      if (!canUseVisibleOverlap) {
        combined = `${combined} ${next}`.trim();
        continue;
      }

      if (hasEnoughContextForVisibleOverlap(nextNorm) && combinedNorm.includes(nextNorm)) {
        continue;
      }
      if (hasEnoughContextForVisibleOverlap(combinedNorm) && nextNorm.includes(combinedNorm)) {
        combined = next;
        continue;
      }

      // Across visible finalized/current chunks, do not remove suffix-prefix overlap.
      // Deepgram can split one long utterance into several finalized ranges where
      // repeated words at the boundary are real speech ("two two" + "two two three").
      combined = `${combined} ${next}`.trim();
    }

    return combined.trim();
  }

  function finalizedRangeKey(start?: number, duration?: number): string {
    const s = Number(start);
    const d = Number(duration);
    if (!Number.isFinite(s) || !Number.isFinite(d) || d <= 0) return '';
    return `${s.toFixed(3)}|${d.toFixed(3)}`;
  }

  // Анимированный текст для эффекта печати
  const animatedPartialText = ref<string>('');
  const animatedAccumulatedText = ref<string>('');

  // Таймеры для анимации
  let partialAnimationTimer: ReturnType<typeof setInterval> | null = null;
  let accumulatedAnimationTimer: ReturnType<typeof setInterval> | null = null;
  let autoPasteQueue: Promise<void> = Promise.resolve();
  let autoPasteGeneration = 0;
  let hotkeyStopFinalizeTimer: ReturnType<typeof setTimeout> | null = null;

  const HOTKEY_STOP_LATE_FINAL_GRACE_MS = 1_500;
  const IDLE_STOP_LATE_FINAL_GRACE_MS = 500;

  function clientLog(
    event: string,
    data: Record<string, unknown> = {},
    level: 'debug' | 'info' | 'warn' | 'error' = 'info'
  ): void {
    if (!isTauriAvailable()) return;

    try {
      const result = invoke('log_client_event', { level, event, data });
      if (result && typeof (result as Promise<unknown>).catch === 'function') {
        void (result as Promise<unknown>).catch(() => undefined);
      }
    } catch {}
  }

  // Listeners
  type UnlistenFn = () => void;
  let unlistenPartial: UnlistenFn | null = null;
  let unlistenFinal: UnlistenFn | null = null;
  let unlistenStatus: UnlistenFn | null = null;
  let unlistenError: UnlistenFn | null = null;
  let unlistenConnectionQuality: UnlistenFn | null = null;
  let unlistenTranslationDelta: UnlistenFn | null = null;
  let unlistenTranslationError: UnlistenFn | null = null;
  let unlistenIncomingStatus: UnlistenFn | null = null;
  let unlistenIncomingSourceFinal: UnlistenFn | null = null;
  let unlistenIncomingDelta: UnlistenFn | null = null;
  let unlistenIncomingError: UnlistenFn | null = null;
  let listenerGeneration = 0;

  async function registerStoreListener<T>(
    generation: number,
    eventName: string,
    handler: Parameters<typeof listen<T>>[1],
  ): Promise<UnlistenFn | null> {
    const unlisten = await listen<T>(eventName, handler);
    if (generation !== listenerGeneration) {
      unlisten();
      return null;
    }
    return unlisten;
  }

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
      if (closedSessionIds.value.size > 0) {
        closedSessionIds.value = new Set(
          [...closedSessionIds.value].filter((session) => session > next)
        );
      }
      console.warn('[STT] Marked sessions closed up to', next, 'reason:', reason);
    }

    // Если текущая сессия попала под "закрытую" — принудительно сбрасываем её.
    if (sessionId.value !== null && sessionId.value <= closedSessionIdFloor.value) {
      sessionId.value = null;
    }
  }

  function isRecordingSessionClosed(payloadSessionId: number): boolean {
    return (
      payloadSessionId > 0 &&
      (payloadSessionId <= closedSessionIdFloor.value ||
        closedSessionIds.value.has(payloadSessionId))
    );
  }

  function markRecordingSessionClosed(payloadSessionId: number, reason: string): void {
    if (!payloadSessionId || payloadSessionId <= 0) return;
    if (payloadSessionId <= closedSessionIdFloor.value) return;

    if (!closedSessionIds.value.has(payloadSessionId)) {
      const next = new Set(closedSessionIds.value);
      next.add(payloadSessionId);
      while (next.size > 128) {
        const oldest = next.values().next().value;
        if (typeof oldest !== 'number') break;
        next.delete(oldest);
      }
      closedSessionIds.value = next;
      console.warn('[STT] Marked session closed', payloadSessionId, 'reason:', reason);
    }

    if (sessionId.value === payloadSessionId) {
      sessionId.value = null;
    }
  }

  function setTerminalRecordingErrorStatus(payloadSessionId: number, reason: string): void {
    status.value = RecordingStatus.Error;
    markRecordingSessionClosed(payloadSessionId, reason);
    awaitingSessionStart.value = false;
  }

  function closeCurrentRecordingSessionForNewStart(reason: string): void {
    if (sessionId.value !== null) {
      markRecordingSessionClosed(sessionId.value, reason);
    }
  }

  function isValidIncomingTranslationSessionId(payloadSessionId: number): boolean {
    return Number.isSafeInteger(payloadSessionId) && payloadSessionId > 0;
  }

  function isStaleIncomingTranslationSession(payloadSessionId: number): boolean {
    return (
      incomingTranslationSessionId.value !== null &&
      payloadSessionId < incomingTranslationSessionId.value
    );
  }

  function isIncomingTranslationSessionClosed(payloadSessionId: number): boolean {
    return (
      isValidIncomingTranslationSessionId(payloadSessionId) &&
      incomingClosedSessionIds.value.has(payloadSessionId)
    );
  }

  function markIncomingTranslationSessionClosed(sessionIdToClose: number, reason: string): void {
    if (!isValidIncomingTranslationSessionId(sessionIdToClose)) return;

    if (!incomingClosedSessionIds.value.has(sessionIdToClose)) {
      const next = new Set(incomingClosedSessionIds.value);
      next.add(sessionIdToClose);
      while (next.size > 128) {
        const oldest = next.values().next().value;
        if (typeof oldest !== 'number') break;
        next.delete(oldest);
      }
      incomingClosedSessionIds.value = next;
      console.warn('[IncomingTranslation] Marked session closed', sessionIdToClose, 'reason:', reason);
    }

    if (
      incomingTranslationSessionId.value !== null &&
      incomingTranslationSessionId.value === sessionIdToClose
    ) {
      incomingTranslationSessionId.value = null;
    }
  }

  function applyIncomingTranslationStatus(payload: IncomingTranslationStatusPayload): void {
    const payloadSessionId = payload.session_id;
    const nextStatus = payload.status;
    if (!isValidIncomingTranslationSessionId(payloadSessionId)) return;
    if (isStaleIncomingTranslationSession(payloadSessionId)) return;
    if (isIncomingTranslationSessionClosed(payloadSessionId)) return;

    if (
      incomingTranslationStatus.value === RecordingStatus.Error &&
      incomingTranslationSessionId.value === payloadSessionId &&
      nextStatus !== RecordingStatus.Idle &&
      nextStatus !== RecordingStatus.Error
    ) {
      return;
    }

    const isNewStart =
      nextStatus === RecordingStatus.Starting || nextStatus === RecordingStatus.Recording;

    if (isNewStart && payloadSessionId !== incomingTranslationSessionId.value) {
      incomingTerminalSessionId = null;
      incomingTranslationSessionId.value = payloadSessionId;
      incomingSourceText.value = '';
      incomingTranslationText.value = '';
      incomingTranslationError.value = null;
    }

    if (
      incomingTranslationSessionId.value !== null &&
      payloadSessionId !== incomingTranslationSessionId.value
    ) {
      return;
    }

    incomingTranslationStatus.value = nextStatus;
    if (nextStatus === RecordingStatus.Error) {
      incomingTerminalSessionId = payloadSessionId;
      if (!incomingTranslationError.value) {
        incomingTranslationError.value = String(i18n.global.t('main.errorGeneric'));
      }
      markIncomingTranslationSessionClosed(payloadSessionId, 'status:error');
    }
    if (nextStatus === RecordingStatus.Idle) {
      if (incomingTerminalSessionId === payloadSessionId) {
        incomingTerminalSessionId = null;
      }
      markIncomingTranslationSessionClosed(payloadSessionId, 'status:idle');
    }
  }

  function applyIncomingTranslationSnapshot(payload: IncomingTranslationStatusPayload): void {
    if (isValidIncomingTranslationSessionId(payload.session_id)) {
      applyIncomingTranslationStatus(payload);
      return;
    }
    if (payload.session_id !== 0 || payload.status !== RecordingStatus.Idle) return;

    if (incomingTranslationSessionId.value !== null) {
      markIncomingTranslationSessionClosed(
        incomingTranslationSessionId.value,
        'backend_snapshot:idle'
      );
    }
    incomingTranslationSessionId.value = null;
    incomingTerminalSessionId = null;
    incomingTranslationStatus.value = RecordingStatus.Idle;
    incomingTranslationError.value = null;
  }

  async function reconcileIncomingTranslationState(
    reason: string,
    expectedGeneration?: number
  ): Promise<void> {
    const statusEventVersionBeforeSnapshot = incomingTranslationStatusEventVersion;
    try {
      const snapshot = await invoke<IncomingTranslationStatusPayload>(
        'get_incoming_translation_state'
      );
      if (
        (expectedGeneration === undefined || expectedGeneration === listenerGeneration) &&
        statusEventVersionBeforeSnapshot === incomingTranslationStatusEventVersion &&
        snapshot &&
        typeof snapshot === 'object'
      ) {
        applyIncomingTranslationSnapshot(snapshot);
      }
    } catch (err) {
      console.warn('[IncomingTranslation] Failed to reconcile backend state:', reason, err);
    }
  }

  function isStartOrActiveFlow(): boolean {
    return (
      awaitingSessionStart.value ||
      isConnecting.value ||
      status.value === RecordingStatus.Starting ||
      status.value === RecordingStatus.Recording ||
      status.value === RecordingStatus.Processing
    );
  }

  function shouldPreserveActiveFlowOnIdleReconcile(reason: string): boolean {
    return reason === 'window_shown' || reason === 'start_requested';
  }

  function ensureActiveSessionForIncomingEvent(payloadSessionId: number, source: string): boolean {
    bumpLastSeenSessionId(payloadSessionId);
    const isTranslationEvent = source.startsWith('translation:');

    if (payloadSessionId <= 0) {
      return false;
    }

    // Никогда не принимаем события от "закрытых" сессий.
    if (isRecordingSessionClosed(payloadSessionId)) {
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

      if (
        previousTranscriptionDisplaySuppressed.value &&
        payloadSessionId !== suppressedPreviousSessionId.value
      ) {
        console.warn('[STT] Clearing hidden previous transcript on first event from new session:', {
          source,
          payloadSessionId,
          suppressedPreviousSessionId: suppressedPreviousSessionId.value,
        });
        resetTranscriptionBuffersForNewSession();
      }

      sessionId.value = payloadSessionId;
      awaitingSessionStart.value = false;

      // Если мы "залипли" в Starting из-за пропущенного recording:status=Recording,
      // но уже видим события transcription:* — значит запись реально идёт.
      if (status.value === RecordingStatus.Starting) {
        status.value = RecordingStatus.Recording;
      }
    }

    const isActiveSessionEvent = payloadSessionId === sessionId.value;
    if (isTranslationEvent && isActiveSessionEvent) {
      activeRecordingMode.value = 'live_translation';
    }

    return isActiveSessionEvent;
  }

  async function reconcileBackendStatus(reason: string): Promise<RecordingStatus | null> {
    if (!isTauriAvailable()) return null;

    try {
      const backendStatus = await invoke<RecordingStatus>('get_recording_status');
      const uiLooksLikeStartRace =
        status.value === RecordingStatus.Starting &&
        (awaitingSessionStart.value || isConnecting.value || sessionId.value !== null);
      const preserveActiveFlowOnIdle =
        backendStatus === RecordingStatus.Idle &&
        (uiLooksLikeStartRace ||
          (shouldPreserveActiveFlowOnIdleReconcile(reason) && isStartOrActiveFlow()));

      if (backendStatus === RecordingStatus.Idle) {
        // ВАЖНО: иногда get_recording_status может на короткое время вернуть Idle
        // в момент старта записи (race: окно показано → reconcile успел спросить backend
        // до того как сервис обновил статус, но events уже летят/полетят).
        //
        // Если здесь "жёстко закрыть" сессию (markSessionsClosed) — мы можем случайно
        // пометить ТЕКУЩУЮ session_id как закрытую, и потом навсегда игнорировать
        // recording:status=Recording для этой же сессии → UI залипнет на "Подключение...".
        if (!preserveActiveFlowOnIdle) {
          // Backend говорит что мы точно не пишем — значит можно жёстко закрыть последнюю сессию,
          // чтобы никакие "поздние" события не вернули UI назад.
          markSessionsClosed(lastSeenSessionId.value, `backend_idle:${reason}`);
        } else {
          console.warn('[STT] Reconcile: backend reports Idle while UI is in active/start flow, skipping close floor update', {
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
        if (preserveActiveFlowOnIdle) {
          console.warn('[STT] Reconcile: keeping active UI status (backend reports Idle, likely race with Rust-side start)', {
            reason,
            backendStatus,
            uiStatus: status.value,
            uiSessionId: sessionId.value,
            closedFloor: closedSessionIdFloor.value,
            lastSeenSessionId: lastSeenSessionId.value,
          });
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
        if (!preserveActiveFlowOnIdle) {
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

  const canOpenSettingsForDevice = computed(
    () => status.value === RecordingStatus.Error && isDeviceNotFoundError.value
  );

  const errorSummary = computed(() => error.value ?? i18n.global.t('main.errorGeneric'));

  const errorFullText = computed(() => {
    if (!error.value && status.value !== RecordingStatus.Error) return '';

    const parts: string[] = [];
    if (error.value) parts.push(error.value);
    if (errorType.value) parts.push(`Type: ${errorType.value}`);
    if (errorRaw.value && errorRaw.value !== error.value) {
      parts.push(`Raw error: ${errorRaw.value}`);
    }
    if (errorDetails.value) {
      try {
        parts.push(`Details:\n${JSON.stringify(errorDetails.value, null, 2)}`);
      } catch {
        parts.push(`Details: ${String(errorDetails.value)}`);
      }
    }

    return parts.join('\n\n') || errorSummary.value;
  });

  // Флаг: RecordingPopover подхватит и откроет ProfilePopover с секцией лицензии
  const wantsLicenseActivation = ref(false);

  function openLicenseActivation() {
    wantsLicenseActivation.value = true;
  }

  const visibleAccumulatedText = computed(() => {
    if (previousTranscriptionDisplaySuppressed.value) return '';
    return animatedAccumulatedText.value || accumulatedText.value;
  });

  const visiblePartialText = computed(() => {
    if (previousTranscriptionDisplaySuppressed.value) return '';
    return animatedPartialText.value || partialText.value;
  });

  const visibleFinalText = computed(() => {
    if (previousTranscriptionDisplaySuppressed.value) return '';
    return finalText.value;
  });

  const hasVisibleTranscriptionText = computed(() => {
    // В UI обычно показываем final + анимированный accumulated + анимированный partial.
    // При hotkey restart старые raw buffers могут ещё понадобиться для stop/finalize,
    // поэтому UI считает только visible-поля.
    const visible = `${visibleFinalText.value} ${visibleAccumulatedText.value} ${visiblePartialText.value}`.trim();
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

    // В режиме live_translation показываем накопленный перевод вместо STT-буферов.
    if (activeRecordingMode.value === 'live_translation') {
      if (translationText.value) {
        return translationText.value;
      }
      if (status.value === RecordingStatus.Idle) {
        return t('main.idlePrompt');
      }
      // Starting/Recording без текста — пусть placeholder отрабатывает как обычно (через isConnectingPlaceholder/isListeningPlaceholder).
      return '';
    }

    // Показываем: финальный текст + анимированный накопленный + анимированный промежуточный
    const final = visibleFinalText.value;
    const accumulated = visibleAccumulatedText.value;
    const partial = visiblePartialText.value;

    // Собираем все части которые есть
    const parts = [];
    if (final) parts.push(final);
    if (accumulated) parts.push(accumulated);
    if (partial) parts.push(partial);

    if (parts.length > 0) {
      return combineVisibleTranscriptParts(parts);
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

  function buildCurrentTranscriptionText(): string {
    return [finalText.value, accumulatedText.value, partialText.value]
      .filter(Boolean)
      .join(' ')
      .trim();
  }

  function getAutoPasteDelta(currentText: string): string {
    const normalizedCurrent = currentText.trim();
    const alreadyPasted = lastPastedFinalText.value.trim();

    if (!normalizedCurrent) return '';
    if (!alreadyPasted) return normalizedCurrent;
    if (normalizedCurrent === alreadyPasted) return '';

    if (normalizedCurrent.startsWith(alreadyPasted)) {
      const nextText = normalizedCurrent.slice(alreadyPasted.length).trim();
      return nextText ? ` ${nextText}` : '';
    }

    console.warn('[AutoPaste] Skipping paste because text baseline changed:', {
      alreadyPasted,
      currentText: normalizedCurrent,
    });
    return '';
  }

  async function runAutoPasteCurrentText(
    reason: string,
    currentText: string,
    generation: number
  ): Promise<boolean> {
    const normalizedCurrent = currentText.trim();
    const textToInsert = getAutoPasteDelta(normalizedCurrent);

    if (!textToInsert.trim()) {
      console.log('[AutoPaste] Nothing new to paste:', {
        reason,
        currentText: normalizedCurrent,
        alreadyPasted: lastPastedFinalText.value,
      });
      clientLog('auto_paste_skipped', {
        reason,
        currentLength: normalizedCurrent.length,
        alreadyPastedLength: lastPastedFinalText.value.length,
      }, 'debug');
      return true;
    }

    try {
      console.log('[AutoPaste] Pasting new text:', { reason, textToInsert });
      clientLog('auto_paste_attempt', {
        reason,
        textLength: textToInsert.length,
        currentLength: normalizedCurrent.length,
        alreadyPastedLength: lastPastedFinalText.value.length,
      }, 'info');
      await invoke('auto_paste_text', { text: textToInsert });
      if (generation !== autoPasteGeneration) {
        console.log('[AutoPaste] Paste completed after reset, keeping current session baseline:', { reason });
        clientLog('auto_paste_completed_after_reset', {
          reason,
          textLength: textToInsert.length,
        }, 'warn');
        return true;
      }
      lastPastedFinalText.value = normalizedCurrent;
      console.log('✅ Auto-pasted successfully');
      clientLog('auto_paste_success', {
        reason,
        textLength: textToInsert.length,
        baselineLength: normalizedCurrent.length,
      }, 'info');
      return true;
    } catch (err) {
      console.error('❌ Failed to auto-paste:', err);
      clientLog('auto_paste_failed', {
        reason,
        textLength: textToInsert.length,
        error: String(err),
      }, 'error');
      return false;
    }
  }

  function autoPasteCurrentText(reason: string, currentText = buildCurrentTranscriptionText()): Promise<boolean> {
    const textSnapshot = currentText.trim();
    const generation = autoPasteGeneration;
    const task = autoPasteQueue
      .catch(() => undefined)
      .then(() => {
        if (generation !== autoPasteGeneration) {
          console.log('[AutoPaste] Skipping stale paste task:', { reason });
          return true;
        }
        return runAutoPasteCurrentText(reason, textSnapshot, generation);
      });

    autoPasteQueue = task.then(
      () => undefined,
      () => undefined
    );

    return task;
  }

  function resetAutoPasteProgress(): void {
    lastPastedFinalText.value = '';
    autoPasteGeneration++;
  }

  function clearHotkeyStopFinalizeTimer(): void {
    if (hotkeyStopFinalizeTimer) {
      clearTimeout(hotkeyStopFinalizeTimer);
      hotkeyStopFinalizeTimer = null;
    }
  }

  function clearTranscriptionAnimationTimers(): void {
    if (partialAnimationTimer) {
      clearInterval(partialAnimationTimer);
      partialAnimationTimer = null;
    }
    if (accumulatedAnimationTimer) {
      clearInterval(accumulatedAnimationTimer);
      accumulatedAnimationTimer = null;
    }
  }

  function resetTranscriptionBuffersForNewSession(): void {
    partialText.value = '';
    accumulatedText.value = '';
    finalText.value = '';
    lastFinalizedSegmentKey.value = '';
    lastSpeechFinalRangeKey.value = '';
    currentUtteranceStart.value = -1;
    resetAutoPasteProgress();
    previousTranscriptionDisplaySuppressed.value = false;
    suppressedPreviousSessionId.value = null;
    animatedPartialText.value = '';
    animatedAccumulatedText.value = '';
    clearTranscriptionAnimationTimers();
    translationText.value = '';
  }

  function suppressPreviousTranscriptionDisplay(reason = 'window_hide'): void {
    if (!buildCurrentTranscriptionText() && !hasVisibleTranscriptionText.value) return;

    previousTranscriptionDisplaySuppressed.value = true;
    suppressedPreviousSessionId.value = sessionId.value;
    animatedPartialText.value = '';
    animatedAccumulatedText.value = '';
    clearTranscriptionAnimationTimers();
    clientLog('previous_transcript_display_suppressed', {
      reason,
      sessionId: sessionId.value,
      status: status.value,
      textLength: buildCurrentTranscriptionText().length,
    }, 'debug');
  }

  async function processCurrentTextAfterStop(reason: string): Promise<boolean> {
    const currentText = buildCurrentTranscriptionText();
    if (!currentText) {
      console.log('[STT] No transcription text to process after stop:', { reason });
      clientLog('recording_stop_text_empty', {
        reason,
        accumulatedLength: accumulatedText.value.length,
        partialLength: partialText.value.length,
        finalLength: finalText.value.length,
        sessionId: sessionId.value,
      }, 'warn');
      return false;
    }

    console.log('📝 Текущий текст для обработки:', currentText);

    if (autoCopyEnabled.value) {
      try {
        await invoke('copy_to_clipboard_native', { text: currentText });
        console.log('📋 Auto-copied full transcription to clipboard');
      } catch (err) {
        console.error('❌ Failed to auto-copy transcription:', err);
      }
    }

    if (autoPasteEnabled.value) {
      await autoPasteCurrentText(reason, currentText);
    }

    return true;
  }

  /**
   * Досылает незапечатанный хвост прошлой сессии, если grace-таймер hotkey-стопа
   * ещё не успел сработать, а новая сессия уже стартует.
   *
   * Без этого clearHotkeyStopFinalizeTimer() при быстром рестарте (< grace-окна)
   * молча выбрасывал поздние финалы: текст оставался в буферах и терялся при reset.
   * Дельту считаем сразу (baseline вот-вот сбросится), а в очередь ставим задачу
   * без generation-проверки — этот paste принадлежит уже завершившейся сессии.
   */
  function flushPendingHotkeyStopTailBeforeReset(reason: string): void {
    if (!hotkeyStopFinalizeTimer) return;
    clearHotkeyStopFinalizeTimer();

    const currentText = buildCurrentTranscriptionText();
    if (!currentText) return;

    clientLog('hotkey_stop_tail_flush', {
      reason,
      textLength: currentText.length,
      sessionId: sessionId.value,
    }, 'info');

    if (autoCopyEnabled.value) {
      invoke('copy_to_clipboard_native', { text: currentText }).catch((err) => {
        console.error('❌ Failed to auto-copy pending tail before reset:', err);
      });
    }

    if (!autoPasteEnabled.value) return;

    const textToInsert = getAutoPasteDelta(currentText);
    if (!textToInsert.trim()) return;

    const task = autoPasteQueue
      .catch(() => undefined)
      .then(async () => {
        try {
          console.log('[AutoPaste] Pasting pending tail from previous session:', { reason, textToInsert });
          await invoke('auto_paste_text', { text: textToInsert });
        } catch (err) {
          console.error('❌ Failed to paste pending tail before reset:', err);
        }
      });

    autoPasteQueue = task.then(
      () => undefined,
      () => undefined
    );
  }

  function scheduleStopFinalize(
    payloadSessionId: number,
    reason: string,
    delayMs: number,
    closeReason: string
  ): void {
    clearHotkeyStopFinalizeTimer();

    hotkeyStopFinalizeTimer = setTimeout(() => {
      hotkeyStopFinalizeTimer = null;

      if (sessionId.value !== payloadSessionId) {
        return;
      }

      void (async () => {
        try {
          clientLog('hotkey_stop_grace_elapsed', {
            reason,
            payloadSessionId,
            textLength: buildCurrentTranscriptionText().length,
            accumulatedLength: accumulatedText.value.length,
            partialLength: partialText.value.length,
            finalLength: finalText.value.length,
          }, 'info');
          await processCurrentTextAfterStop(reason);
        } catch (err) {
          console.error('[STT] Failed to process text after stop:', err);
          clientLog('recording_stop_text_processing_failed', {
            reason,
            payloadSessionId,
            error: String(err),
          }, 'error');
        } finally {
          markSessionsClosed(payloadSessionId, closeReason);
          sessionId.value = null;
          awaitingSessionStart.value = false;
          resetTextStateBeforeStart();
        }
      })();
    }, delayMs);
  }

  function scheduleHotkeyStopFinalize(payloadSessionId: number): void {
    scheduleStopFinalize(
      payloadSessionId,
      'hotkey_stop_grace',
      HOTKEY_STOP_LATE_FINAL_GRACE_MS,
      'stopped_via_hotkey:grace'
    );
  }

  function scheduleIdleStopFinalize(payloadSessionId: number): void {
    scheduleStopFinalize(
      payloadSessionId,
      'idle_grace',
      IDLE_STOP_LATE_FINAL_GRACE_MS,
      'stopped:idle_grace'
    );
  }

  // Actions
  async function initialize() {
    console.log('Initializing transcription store');

    if (!isTauriAvailable()) {
      const message = i18n.global.t('main.tauriUnavailable');
      console.warn(message);
      setRecordingError(null, message, null, message);
      status.value = RecordingStatus.Error;
      return;
    }

    // Отписываемся от старых listeners перед регистрацией новых
    // Это предотвращает дублирование событий при повторной инициализации
    cleanup();
    const generation = listenerGeneration;

    try {
      // Listen to partial transcription events
      const partialUnlisten = await registerStoreListener<PartialTranscriptionPayload>(
        generation,
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
            last_finalized: lastFinalizedSegmentKey.value
          });
          clientLog('transcription_partial_event_received', {
            textLength: event.payload.text.length,
            isSegmentFinal: event.payload.is_segment_final,
            start: event.payload.start,
            duration: event.payload.duration,
            accumulatedLength: accumulatedText.value.length,
            partialLength: partialText.value.length,
            finalLength: finalText.value.length,
          }, 'debug');

          // Если сегмент финализирован (is_final=true, но не speech_final)
          if (event.payload.is_segment_final) {
            const newText = event.payload.text;
            const segKey = finalizedRangeKey(event.payload.start, event.payload.duration);

            // Проверка на точный дубликат (защита от повторной отправки того же сегмента)
            if (segKey && segKey === lastFinalizedSegmentKey.value) {
              console.log('⚠️ Exact duplicate segment detected, skipping:', newText);
              return;
            }

            // Финализировали utterance - добавляем к накопленному тексту
            const oldAccumulated = accumulatedText.value;
            console.log('🔒 [BEFORE ACCUMULATE] accumulated:', oldAccumulated);
            console.log('🔒 [BEFORE ACCUMULATE] newText:', newText);

            // Deepgram can send segment finals before the utterance ends.
            // Paste finalized ranges as live deltas; native hotkey suppression prevents self-toggle.
            accumulatedText.value = appendTranscriptText(accumulatedText.value, newText);

            lastFinalizedSegmentKey.value = segKey;

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

            if (autoPasteEnabled.value && newText.trim()) {
              await autoPasteCurrentText('segment_final');
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

              // Не переносим старый interim в stable buffer: `is_final=false`
              // может быть исправлен Deepgram следующим segment-final.
              // Старый текст был только live-гипотезой и не должен попадать в paste baseline.
              if (partialText.value) {
                console.info('[STT] Dropping unfinalized partial after start change:', {
                  old_start: currentUtteranceStart.value,
                  new_start: event.payload.start,
                  dropped_partial: partialText.value,
                });
              }

              // Начинаем новый segment
              currentUtteranceStart.value = event.payload.start;
              partialText.value = event.payload.text;

              // Запускаем анимацию для partial текста
              animatePartialText(event.payload.text);
            }
          }
        }
      );
      if (!partialUnlisten) return;
      unlistenPartial = partialUnlisten;

      // Listen to final transcription events
      const finalUnlisten = await registerStoreListener<FinalTranscriptionPayload>(
        generation,
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
          clientLog('transcription_final_event_received', {
            textLength: event.payload.text.length,
            start: event.payload.start,
            duration: event.payload.duration,
            accumulatedLength: accumulatedText.value.length,
            partialLength: partialText.value.length,
            finalLength: finalText.value.length,
          }, 'debug');

          // `speech_final=true` закрывает текущий utterance/диапазон транскрипта, но не запись.
          // Жизненным циклом записи владеет Rust/VAD или явная команда пользователя.
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
            const finalKey = finalizedRangeKey(event.payload.start, event.payload.duration);
            if (finalKey && finalKey === lastSpeechFinalRangeKey.value) {
              console.log('⚠️ Duplicate speech_final range detected, skipping:', finalKey);
              return;
            }
            const isDuplicateFinalRange =
              !!event.payload.text &&
              !!finalKey &&
              finalKey === lastFinalizedSegmentKey.value &&
              !!accumulatedText.value.trim();
            const currentUtteranceText = isDuplicateFinalRange
              ? accumulatedText.value.trim()
              : event.payload.text
                ? appendTranscriptText(accumulatedText.value, event.payload.text).trim()
                : mergeTranscriptText(accumulatedText.value, partialText.value).trim();
            console.log('🔗 [SPEECH_FINAL] Combining utterance:', {
              accumulated: accumulatedText.value,
              partial: partialText.value,
              final_payload: event.payload.text,
              used_source: isDuplicateFinalRange
                ? 'deduped finalized range'
                : event.payload.text ? 'FINAL payload' : 'accumulated+partial',
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
            lastFinalizedSegmentKey.value = '';
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
            if (finalKey) {
              lastSpeechFinalRangeKey.value = finalKey;
            }

            console.log('📋 [AFTER ADD] finalText:', finalText.value);
            console.log('📋 Successfully added utterance to finalText');

            if (autoPasteEnabled.value && currentUtteranceText.trim()) {
              const pasted = await autoPasteCurrentText('speech_final');
              if (!pasted && autoCopyEnabled.value) {
                try {
                  await invoke('copy_to_clipboard_native', { text: currentUtteranceText });
                  console.log('📋 Auto-paste fallback copied current utterance to clipboard');
                } catch (copyErr) {
                  console.error('❌ Failed to copy auto-paste fallback:', copyErr);
                }
              }
            }

          } else {
            console.warn('⚠️ [SPEECH_FINAL] event.payload.text is empty, skipping');
            console.log('⚠️ [SPEECH_FINAL] Event payload:', event.payload);
          }
        }
      );
      if (!finalUnlisten) return;
      unlistenFinal = finalUnlisten;

      // Listen to recording status events
      const statusUnlisten = await registerStoreListener<RecordingStatusPayload>(
        generation,
        EVENT_RECORDING_STATUS,
        async (event) => {
          console.log('Recording status changed:', event.payload);
          const nextStatus = event.payload.status;
          const payloadSessionId = event.payload.session_id;
          const previousStatus = status.value;
          const isStartLike =
            nextStatus === RecordingStatus.Starting ||
            nextStatus === RecordingStatus.Recording;

          clientLog('recording_status_event_received', {
            previousStatus,
            nextStatus,
            payloadSessionId,
            activeSessionId: sessionId.value,
            stoppedViaHotkey: event.payload.stopped_via_hotkey,
            awaitingSessionStart: awaitingSessionStart.value,
            isConnecting: isConnecting.value,
            connectAttempt: connectAttempt.value,
            connectMaxAttempts: connectMaxAttempts.value,
            closedSessionIdFloor: closedSessionIdFloor.value,
          }, 'info');

          bumpLastSeenSessionId(payloadSessionId);

          // Если сессия уже помечена как "закрытая" — игнорируем любые её статусы,
          // иначе UI может "ожить" старым Recording спустя время (на скрытом окне).
          if (isRecordingSessionClosed(payloadSessionId)) {
            console.warn('[STT] Ignoring status from closed session:', {
              payloadSessionId,
              closedFloor: closedSessionIdFloor.value,
              nextStatus,
            });
            clientLog('recording_status_event_ignored', {
              reason: 'closed_session',
              nextStatus,
              payloadSessionId,
              closedSessionIdFloor: closedSessionIdFloor.value,
            }, 'warn');
            return;
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
              clientLog('recording_status_event_ignored', {
                reason: 'awaiting_session_start_non_start_status',
                nextStatus,
                payloadSessionId,
                activeSessionId: sessionId.value,
              }, 'warn');
              return;
            }
            awaitingSessionStart.value = false;
            clientLog('recording_session_start_accepted', {
              nextStatus,
              payloadSessionId,
            }, 'info');
          }

          // Если пришёл статус НЕ от текущей сессии — игнорируем (особенно важно для позднего Idle).
          // Исключение: Starting/Recording считаем началом новой сессии (например, старт инициирован Rust-стороной).
          if (!isStartLike && sessionId.value !== null && payloadSessionId !== sessionId.value) {
            console.warn('[STT] Ignoring status from stale session:', {
              payloadSessionId,
              activeSessionId: sessionId.value,
              nextStatus,
            });
            clientLog('recording_status_event_ignored', {
              reason: 'stale_session',
              nextStatus,
              payloadSessionId,
              activeSessionId: sessionId.value,
            }, 'warn');
            return;
          }

          // Важно: статус Idle выставляем рано, но только после session guards.
          // Иначе поздний Idle от старой сессии может перевести UI в Idle уже после нового Recording.
          //
          // Для Error так делать нельзя — иначе сломаем suppression во время connect-retry.
          if (nextStatus === RecordingStatus.Idle) {
            status.value = RecordingStatus.Idle;
          }

          // Начало новой сессии: фиксируем sessionId и чистим текст/ошибки.
          const prevSessionId = sessionId.value;
          if (isStartLike && payloadSessionId !== prevSessionId) {
            sessionId.value = payloadSessionId;
          }

          const modeBeforeStatus = activeRecordingMode.value;

          // Активный режим — обновляем при start. Если backend не прислал mode, считаем dictation.
          if (isStartLike) {
            activeRecordingMode.value = event.payload.mode ?? 'dictation';
          }
          // На Idle после live_translation оставляем mode, чтобы translated text не исчезал мгновенно
          // и auto-copy/paste оставались отключены до cleanup/следующего старта.
          const idleFromLiveTranslation =
            nextStatus === RecordingStatus.Idle &&
            (modeBeforeStatus === 'live_translation' || event.payload.mode === 'live_translation');
          if (nextStatus === RecordingStatus.Idle && !idleFromLiveTranslation) {
            activeRecordingMode.value = 'dictation';
          }

          // Если статус стал Starting или Recording - очищаем весь текст
          // Это работает и для кнопки, и для hotkey (Ctrl+X)
          const isNewSession = isStartLike && payloadSessionId !== prevSessionId;
          if (
            isStartLike &&
            (isNewSession ||
              (status.value !== RecordingStatus.Starting && status.value !== RecordingStatus.Recording))
          ) {
            console.log('Recording starting/started - clearing all text');
            // Если grace-таймер hotkey-стопа ещё не сработал, досылаем хвост прошлой
            // сессии до очистки буферов — иначе он молча потеряется.
            flushPendingHotkeyStopTailBeforeReset('new_session_status');
            clearRecordingErrorState();
            resetTranscriptionBuffersForNewSession();
          }

          // Если статус стал Idle - обрабатываем текущий текст при ЛЮБОЙ остановке
          // (через hotkey ИЛИ через VAD timeout когда пользователь закончил говорить)
          //
          // Из логов [2025-11-03]: VAD timeout - это нормальный способ остановки после молчания >3 сек.
          // Пользователь закончил говорить → текст должен скопироваться и вставиться автоматически.
          // Проверка `stopped_via_hotkey` убрана, чтобы auto-paste работал в обоих случаях.
          if (nextStatus === RecordingStatus.Idle) {
            console.log('🔄 Запись остановлена - обрабатываем текущий текст');
            clientLog('recording_idle_processing_started', {
              payloadSessionId,
              stoppedViaHotkey: event.payload.stopped_via_hotkey,
              textLength: buildCurrentTranscriptionText().length,
              previousStatus,
            }, 'info');

            if (event.payload.stopped_via_hotkey) {
              // Final/partial события могут прийти чуть позже Idle из другой async-задачи.
              // Не обрабатываем partial сразу, иначе можем вставить черновик, а поздний final
              // исправит только UI. Grace-таймер обработает самый свежий текст один раз.
              scheduleHotkeyStopFinalize(payloadSessionId);
            } else {
              // VAD/non-hotkey stop проходит тот же async event pipeline: final может
              // доехать сразу после Idle. Короткое окно сохраняет прежнюю отзывчивость,
              // но не вставляет черновой partial перед чистовым speech_final.
              scheduleIdleStopFinalize(payloadSessionId);
            }
          }

          // Если прилетает Error после auth-ошибки, не показываем это пользователю.
          // В commands.rs сначала эмитится transcription:error, потом recording:status=Error.
          // Не меняем status — retry loop или auth handler сами определят следующее состояние.
          // (Раньше ставили Idle, что вызывало мигание "Подключение → Нажмите кнопку → Подключение".)
          if (nextStatus === RecordingStatus.Error && suppressNextErrorStatus) {
            suppressNextErrorStatus = false;
            clientLog('recording_error_status_suppressed', {
              reason: 'auth_error_suppression',
              payloadSessionId,
              previousStatus,
            }, 'warn');
            return;
          }

          // Если сейчас идёт подключение с ретраями — не переключаем UI в Error мгновенно.
          // Решение о показе ошибки принимает retry-цикл, чтобы не мигала красная плашка.
          if (nextStatus === RecordingStatus.Error && isConnecting.value) {
            console.warn('[ConnectRetry] Got RecordingStatus.Error during connect attempt - waiting for retry decision');
            clientLog('recording_error_status_suppressed', {
              reason: 'connect_retry_active',
              payloadSessionId,
              previousStatus,
              connectAttempt: connectAttempt.value,
              connectMaxAttempts: connectMaxAttempts.value,
            }, 'warn');
            return;
          }

          // Фоновая ошибка после остановки записи (keep-alive/таймаут провайдера и т.п.)
          // Пользователь уже закончил запись — не надо переводить UI в Error.
          if (nextStatus === RecordingStatus.Error && !isConnecting.value) {
            const current = status.value;
            if (current === RecordingStatus.Idle || current === RecordingStatus.Processing) {
              console.warn('[STT] Ignoring background Error status while not recording:', event.payload);
              status.value = RecordingStatus.Idle;
              markSessionsClosed(payloadSessionId, 'status:error_background_after_stop');
              awaitingSessionStart.value = false;
              clientLog('recording_error_status_suppressed', {
                reason: 'background_error_after_stop',
                payloadSessionId,
                previousStatus,
                currentStatus: current,
              }, 'warn');
              return;
            }
          }

          status.value = nextStatus;
          clientLog('recording_status_applied', {
            previousStatus,
            nextStatus,
            payloadSessionId,
            activeSessionId: sessionId.value,
            isConnecting: isConnecting.value,
          }, 'info');
          // Если упали в Error — закрываем сессию, чтобы поздние события не перетёрли UI.
          if (nextStatus === RecordingStatus.Error) {
            markSessionsClosed(payloadSessionId, 'status:error');
            awaitingSessionStart.value = false;
          }
        }
      );
      if (!statusUnlisten) return;
      unlistenStatus = statusUnlisten;

      // Listen to transcription error events
      const errorUnlisten = await registerStoreListener<TranscriptionErrorPayload>(
        generation,
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
            errorRaw.value = event.payload.error;
            errorDetails.value = event.payload.error_details ?? null;
            suppressNextErrorStatus = true;
            markRecordingSessionClosed(event.payload.session_id, 'transcription:error:authentication');

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

          const isProviderQuotaExceeded =
            event.payload.error_type === 'provider_quota_exceeded' ||
            event.payload.error_details?.category === 'provider_quota_exceeded' ||
            event.payload.error_details?.serverCode === 'PROVIDER_QUOTA_EXCEEDED';

          if (isProviderQuotaExceeded) {
            setRecordingError(
              'provider_quota_exceeded',
              event.payload.error,
              event.payload.error_details
            );
            setTerminalRecordingErrorStatus(
              event.payload.session_id,
              'transcription:error:provider_quota_exceeded'
            );
            return;
          }

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
            setRecordingError(
              'limit_exceeded',
              event.payload.error,
              event.payload.error_details,
              usageMessage
            );
            setTerminalRecordingErrorStatus(
              event.payload.session_id,
              'transcription:error:limit_exceeded'
            );
            return;
          }

          if (isRateLimited && !isConnecting.value) {
            const wasStarting = status.value === RecordingStatus.Starting;
            const serverCode = event.payload.error_details?.serverCode;

            if (wasStarting && rateLimitRetryCount < RATE_LIMIT_MAX_RETRIES) {
              rateLimitRetryCount++;
              const delaySec = serverCode === 'TOO_MANY_SESSIONS' ? 2 : 5;
              const retrySessionId = event.payload.session_id;
              console.warn(`[STT] 429 (${serverCode ?? 'unknown'}), auto-retry #${rateLimitRetryCount} через ${delaySec}с`);
              suppressNextErrorStatus = true;
              markRecordingSessionClosed(retrySessionId, 'transcription:error:rate_limited_retry');
              status.value = RecordingStatus.Starting;
              clearRateLimitRetryTimer();
              const retryGeneration = ++rateLimitRetryGeneration;
              rateLimitRetryTimer = setTimeout(() => {
                rateLimitRetryTimer = null;
                if (
                  retryGeneration === rateLimitRetryGeneration &&
                  status.value === RecordingStatus.Starting &&
                  (retrySessionId <= 0 || sessionId.value === null || sessionId.value === retrySessionId)
                ) {
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
              setRecordingError('connection', event.payload.error, event.payload.error_details);
              setTerminalRecordingErrorStatus(
                event.payload.session_id,
                'transcription:error:rate_limited'
              );
            }
            return;
          }

          // Фоновая ошибка после остановки записи (keep-alive, таймаут провайдера, и т.п.)
          // Если пользователь сейчас не записывает и не подключается — игнорируем, чтобы не "залипать" в Error.
          if (!isConnecting.value) {
            const current = status.value;
            if (current === RecordingStatus.Idle || current === RecordingStatus.Processing) {
              console.warn('[STT] Ignoring background error while not recording:', event.payload);
              status.value = RecordingStatus.Idle;
              markSessionsClosed(event.payload.session_id, 'transcription:error_background_after_stop');
              awaitingSessionStart.value = false;
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
              setRecordingError(
                'limit_exceeded',
                event.payload.error,
                event.payload.error_details,
                usageMessage
              );
              setTerminalRecordingErrorStatus(
                event.payload.session_id,
                'transcription:error:connect_limit_exceeded'
              );
              return;
            }

            const connectIsProviderQuotaExceeded =
              event.payload.error_type === 'provider_quota_exceeded' ||
              event.payload.error_details?.category === 'provider_quota_exceeded' ||
              event.payload.error_details?.serverCode === 'PROVIDER_QUOTA_EXCEEDED';
            if (connectIsProviderQuotaExceeded) {
              isConnecting.value = false;
              setRecordingError(
                'provider_quota_exceeded',
                event.payload.error,
                event.payload.error_details
              );
              setTerminalRecordingErrorStatus(
                event.payload.session_id,
                'transcription:error:connect_provider_quota_exceeded'
              );
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
            markRecordingSessionClosed(event.payload.session_id, 'transcription:error:connect_failure');
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
          if (
            errorType.value === 'provider_quota_exceeded' &&
            normalizedType !== 'provider_quota_exceeded'
          ) {
            console.warn('[STT] Skipping error downgrade from provider_quota_exceeded to', normalizedType);
            return;
          }

          setRecordingError(normalizedType, event.payload.error, event.payload.error_details);
          setTerminalRecordingErrorStatus(event.payload.session_id, 'transcription:error');
        }
      );
      if (!errorUnlisten) return;
      unlistenError = errorUnlisten;

      // Listen to connection quality events
      const connectionQualityUnlisten = await registerStoreListener<ConnectionQualityPayload>(
        generation,
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
      if (!connectionQualityUnlisten) return;
      unlistenConnectionQuality = connectionQualityUnlisten;

      // Live translation events. Идут параллельно STT — не пересекаются с auto-paste/copy/history.
      const translationDeltaUnlisten = await registerStoreListener<TranslationDeltaPayload>(
        generation,
        EVENT_TRANSLATION_DELTA,
        (event) => {
          if (!ensureActiveSessionForIncomingEvent(event.payload.session_id, 'translation:delta')) {
            return;
          }
          if (event.payload.text) {
            translationText.value = keepRecentStreamingText(
              translationText.value + event.payload.text
            );
          }
        }
      );
      if (!translationDeltaUnlisten) return;
      unlistenTranslationDelta = translationDeltaUnlisten;

      const translationErrorUnlisten = await registerStoreListener<TranslationErrorPayload>(
        generation,
        EVENT_TRANSLATION_ERROR,
        (event) => {
          if (!ensureActiveSessionForIncomingEvent(event.payload.session_id, 'translation:error')) {
            return;
          }
          // Translation errors не должны триггерить STT auth/logout flow.
          // Но для live_translation это terminal error, поэтому connect-loop должен
          // завершиться сразу, а не ждать общий timeout.
          const normalizedType =
            asKnownErrorType(event.payload.error_type) ??
            detectErrorTypeFromRaw(event.payload.error) ??
            'connection';
          setRecordingError(normalizedType, event.payload.error, null, event.payload.error);
          if (isConnecting.value || awaitingSessionStart.value) {
            lastConnectFailure.value = normalizedType;
            lastConnectFailureRaw.value = event.payload.error;
            lastConnectFailureDetails.value = null;
          }
          status.value = RecordingStatus.Error;
          awaitingSessionStart.value = false;
          markRecordingSessionClosed(event.payload.session_id, 'translation:error');
          console.error('[translation:error]', event.payload);
        }
      );
      if (!translationErrorUnlisten) return;
      unlistenTranslationError = translationErrorUnlisten;

      const incomingStatusUnlisten = await registerStoreListener<IncomingTranslationStatusPayload>(
        generation,
        EVENT_INCOMING_TRANSLATION_STATUS,
        (event) => {
          incomingTranslationStatusEventVersion += 1;
          applyIncomingTranslationStatus(event.payload);
        }
      );
      if (!incomingStatusUnlisten) return;
      unlistenIncomingStatus = incomingStatusUnlisten;

      const incomingSourceFinalUnlisten = await registerStoreListener<IncomingTranslationTextPayload>(
        generation,
        EVENT_INCOMING_TRANSLATION_SOURCE_FINAL,
        (event) => {
          const payloadSessionId = event.payload.session_id;
          if (!isValidIncomingTranslationSessionId(payloadSessionId)) return;
          if (isStaleIncomingTranslationSession(payloadSessionId)) return;
          if (isIncomingTranslationSessionClosed(payloadSessionId)) return;
          if (payloadSessionId !== incomingTranslationSessionId.value) return;
          if (incomingTranslationStatus.value === RecordingStatus.Error) return;
          if (event.payload.text) {
            incomingSourceText.value = appendStreamingTranscriptText(
              incomingSourceText.value,
              event.payload.text
            );
          }
        }
      );
      if (!incomingSourceFinalUnlisten) return;
      unlistenIncomingSourceFinal = incomingSourceFinalUnlisten;

      const incomingDeltaUnlisten = await registerStoreListener<IncomingTranslationTextPayload>(
        generation,
        EVENT_INCOMING_TRANSLATION_DELTA,
        (event) => {
          const payloadSessionId = event.payload.session_id;
          if (!isValidIncomingTranslationSessionId(payloadSessionId)) return;
          if (isIncomingTranslationSessionClosed(payloadSessionId)) return;
          if (payloadSessionId !== incomingTranslationSessionId.value) return;
          if (incomingTranslationStatus.value === RecordingStatus.Error) return;
          if (event.payload.text) {
            incomingTranslationError.value = null;
            incomingTranslationText.value = appendStreamingTranscriptText(
              incomingTranslationText.value,
              event.payload.text
            );
          }
        }
      );
      if (!incomingDeltaUnlisten) return;
      unlistenIncomingDelta = incomingDeltaUnlisten;

      const incomingErrorUnlisten = await registerStoreListener<IncomingTranslationErrorPayload>(
        generation,
        EVENT_INCOMING_TRANSLATION_ERROR,
        (event) => {
          const payloadSessionId = event.payload.session_id;
          if (!isValidIncomingTranslationSessionId(payloadSessionId)) return;
          const canRefineTerminalError =
            isIncomingTranslationSessionClosed(payloadSessionId) &&
            incomingTerminalSessionId === payloadSessionId &&
            incomingTranslationStatus.value === RecordingStatus.Error;
          if (isIncomingTranslationSessionClosed(payloadSessionId) && !canRefineTerminalError) {
            return;
          }
          if (
            !canRefineTerminalError &&
            incomingTranslationSessionId.value !== null &&
            payloadSessionId !== incomingTranslationSessionId.value
          ) {
            return;
          }
          if (!canRefineTerminalError) {
            incomingTranslationSessionId.value = payloadSessionId;
          }
          // IncomingTranslationService emits on_error only after it has stopped the
          // session. The error event must therefore be sufficient even if the
          // separate status event is delayed or lost.
          incomingTerminalSessionId = payloadSessionId;
          incomingTranslationError.value = event.payload.error;
          incomingTranslationStatus.value = RecordingStatus.Error;
          markIncomingTranslationSessionClosed(payloadSessionId, 'terminal_error');
        }
      );
      if (!incomingErrorUnlisten) return;
      unlistenIncomingError = incomingErrorUnlisten;

      await reconcileIncomingTranslationState('initialize', generation);

      console.log('Event listeners initialized successfully');
    } catch (err) {
      console.error('Failed to initialize event listeners:', err);
      cleanup();
      const raw = formatUnknownError(err);
      setRecordingError(null, raw, null, `Failed to initialize: ${raw}`);
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
    if (lower.startsWith('configuration:') || lower.includes('(type: configuration)')) {
      return 'configuration';
    }
    if (lower.startsWith('processing:')) {
      return 'processing';
    }
    if (lower.startsWith('connection:')) {
      return 'connection';
    }
    if (lower.startsWith('timeout:')) {
      return 'timeout';
    }
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
      lower.includes('live translation health check failed') ||
      lower.includes('health check failed')
    ) {
      return 'configuration';
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
    if (
      lower.includes('rate_limited') ||
      lower.includes('rate limited') ||
      lower.includes('too many requests') ||
      lower.includes('429')
    ) {
      return 'rate_limited';
    }
    if (lower.includes('limit_exceeded') || lower.includes('limit exceeded') || lower.includes('usage limit')) return 'limit_exceeded';
    if (lower.includes('provider_quota_exceeded') || lower.includes('quota_exceeded')) return 'provider_quota_exceeded';
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
    if (value === 'rate_limited') return 'rate_limited';
    if (value === 'limit_exceeded') return 'limit_exceeded';
    if (value === 'provider_quota_exceeded') return 'provider_quota_exceeded';
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
    if (
      details?.serverCode === 'PROVIDER_QUOTA_EXCEEDED' ||
      category === 'provider_quota_exceeded'
    ) {
      return i18n.global.t('errors.providerQuotaExceeded');
    }
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
      case 'provider_quota_exceeded':
        return i18n.global.t('errors.providerQuotaExceeded');
      case 'processing':
        return mapProcessingErrorMessage(raw);
      case 'authentication':
        // По идее мы сюда не попадаем (auth ошибка приводит к auto-logout),
        // но оставляем адекватный текст на всякий случай.
        return i18n.global.t('errors.authentication');
      case 'rate_limited':
        return i18n.global.t('errors.rateLimited');
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

  function currentConnectFailureDetails(): TranscriptionErrorPayload['error_details'] | null {
    return lastConnectFailureDetails.value ?? null;
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
      clearRecordingErrorState();
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
    clearHotkeyStopFinalizeTimer();

    // Очищаем весь предыдущий текст перед новой записью
    clearRecordingErrorState();
    resetTranscriptionBuffersForNewSession();
  }

  async function startRecordingOnce(): Promise<void> {
    // Начинаем новую сессию "с чистого листа": пока не получим Starting/Recording с новым session_id,
    // игнорируем любые поздние события от прошлых запусков.
    awaitingSessionStart.value = true;
    closeCurrentRecordingSessionForNewStart('start_recording_once');

    resetTextStateBeforeStart();
    status.value = RecordingStatus.Starting;

    // На каждый запуск сбрасываем маркеры исхода подключения
    lastConnectFailure.value = null;
    lastConnectFailureRaw.value = '';
    lastConnectFailureDetails.value = null;

    console.log('[ConnectRetry] Starting recording (single attempt)');
    clientLog('recording_start_requested', {
      connectAttempt: connectAttempt.value,
      connectMaxAttempts: connectMaxAttempts.value,
      awaitingSessionStart: awaitingSessionStart.value,
    }, 'info');
    await invoke<string>('start_recording');
  }

  async function startRecordingWithRetry(maxAttempts = 3): Promise<void> {
    // Не запускаем два подключения одновременно
    if (isConnecting.value) {
      console.log('[ConnectRetry] Skipped - connect already in progress');
      return;
    }

    const requestedRecordingMode = appConfig.recordingMode ?? 'dictation';
    const isLiveTranslationAttempt = requestedRecordingMode === 'live_translation';

    isConnecting.value = true;
    connectAttempt.value = 0;
    connectMaxAttempts.value = Math.max(1, isLiveTranslationAttempt ? 1 : maxAttempts);
    let authRefreshUsed = false;

    try {
      for (let attempt = 1; attempt <= connectMaxAttempts.value; attempt++) {
        connectAttempt.value = attempt;
        lastConnectFailure.value = null;
        lastConnectFailureRaw.value = '';
        lastConnectFailureDetails.value = null;

        try {
          // Перед первой попыткой гарантируем, что access token свежий.
          // Иначе backend WS легко вернёт 401 (access TTL ~15 минут), и UI начнёт "разлогинивать" пользователя.
          // Для live_translation это OpenAI key, не STT backend auth, поэтому refresh/logout тут запрещён.
          if (attempt === 1 && !isLiveTranslationAttempt) {
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
          clientLog('recording_connect_success', {
            attempt,
            maxAttempts: connectMaxAttempts.value,
            sessionId: sessionId.value,
            status: status.value,
          }, 'info');
          rateLimitRetryCount = 0;
          return;
    } catch (err) {
          // ВАЖНО: err может быть либо "типом" (timeout/connection/...) из waitForConnectOutcome,
          // либо сырой строкой ошибки из invoke('start_recording').
          // Нельзя интерпретировать любую строку как error_type.
          const failureType = asKnownErrorType(err);

          // Если ошибка пришла не через events, пробуем классифицировать по raw строке
          const raw = lastConnectFailureRaw.value || formatUnknownError(err);
          const details = currentConnectFailureDetails();
          const detected = failureType || detectErrorTypeFromRaw(raw) || 'connection';

          const httpStatus = details?.httpStatus ?? extractHttpStatusFromRaw(raw);
          const serverCode = details?.serverCode;
          const isLimitExceeded =
            details?.category === 'limit_exceeded' ||
            serverCode === 'LIMIT_EXCEEDED' ||
            serverCode === 'LICENSE_INACTIVE' ||
            isLicenseInactiveFromRaw(raw);

          if (isLiveTranslationAttempt) {
            setRecordingError(detected, raw, details);
            status.value = RecordingStatus.Error;
            clientLog('recording_connect_attempt_failed', {
              attempt,
              maxAttempts: connectMaxAttempts.value,
              detected,
              liveTranslation: true,
              raw,
            }, 'error');
            return;
          }

          // Auth ошибка: обычно это протухший access token.
          // Пробуем один раз обновить сессию и продолжить retry-цикл.
          if (detected === 'authentication') {
            // Если уже успешно обновляли токены, но всё равно получаем 401 — значит токен не подходит
            // (или backend всё ещё использует старый). Не оставляем UI в "Подключение..." бесконечно.
            if (authRefreshUsed) {
              errorType.value = 'authentication';
              errorRaw.value = raw;
              errorDetails.value = details;
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
            errorRaw.value = raw;
            errorDetails.value = details;
            suppressNextErrorStatus = true;
            await forceLogoutFromSttAuthError();
            return;
          }

          if (isLimitExceeded) {
            setRecordingError('limit_exceeded', raw, details);
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
          clientLog('recording_connect_attempt_failed', {
            attempt,
            maxAttempts: connectMaxAttempts.value,
            detected,
            isRetriable,
            isLastAttempt,
            isRateLimited,
            httpStatus,
            serverCode,
            raw,
          }, isLastAttempt || !isRetriable ? 'error' : 'warn');

          if (!isRetriable || isLastAttempt) {
            setRecordingError(detected, raw, details);
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
      setRecordingError(fallbackType, fallbackRaw, lastConnectFailureDetails.value);
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
    clearRateLimitRetryTimer();
    rateLimitRetryCount = 0;
    await startRecordingWithRetry(3);
  }

  function prepareForRustHotkeyStart(warmStartExpected = false): void {
    clearRateLimitRetryTimer();
    flushPendingHotkeyStopTailBeforeReset('rust_hotkey_start');

    suppressPreviousTranscriptionDisplay('rust_hotkey_start');
    closeCurrentRecordingSessionForNewStart('rust_hotkey_start');
    activeRecordingMode.value = appConfig.recordingMode ?? 'dictation';
    awaitingSessionStart.value = true;
    status.value = warmStartExpected ? RecordingStatus.Recording : RecordingStatus.Starting;
    clearRecordingErrorState();
    lastConnectFailure.value = null;
    lastConnectFailureRaw.value = '';
    lastConnectFailureDetails.value = null;
  }

  async function reconnect(): Promise<void> {
    clearRateLimitRetryTimer();
    rateLimitRetryCount = 0;
    await startRecordingWithRetry(3);
  }

  async function stopRecording(reason = 'manual') {
    clearRateLimitRetryTimer();
    try {
      clientLog('recording_stop_requested', {
        reason,
        status: status.value,
        sessionId: sessionId.value,
        textLength: buildCurrentTranscriptionText().length,
        isConnecting: isConnecting.value,
      }, 'warn');
      status.value = RecordingStatus.Processing;
      const result = await invoke<string>('stop_recording');
      console.log('Recording stopped:', result);
      const backendStatus = await reconcileBackendStatus('stop_recording_success');
      clientLog('recording_stop_completed', {
        reason,
        result,
        backendStatus,
        status: status.value,
        sessionId: sessionId.value,
      }, 'info');
      if (backendStatus === RecordingStatus.Idle) {
        clearRecordingErrorState();
      }
    } catch (err) {
      console.error('Failed to stop recording:', err);
      const backendStatus = await reconcileBackendStatus('stop_recording_error');
      clientLog('recording_stop_failed', {
        reason,
        error: String(err),
        backendStatus,
        status: status.value,
        sessionId: sessionId.value,
      }, 'error');
      if (backendStatus === RecordingStatus.Idle) {
        console.warn('[STT] Stop command failed, but backend is already Idle:', err);
        clearRecordingErrorState();
        return;
      }
      const raw = formatUnknownError(err);
      setRecordingError(null, raw, null, raw);
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
      await stopRecording('manual_toggle');
    } else {
      await startRecording();
    }
  }

  async function toggleIncomingTranslation(): Promise<void> {
    if (!isTauriAvailable()) return;
    if (incomingTranslationStatus.value === RecordingStatus.Processing) return;
    if (incomingTranslationCommandInFlight.value) return;

    const shouldStop =
      incomingTranslationStatus.value === RecordingStatus.Starting ||
      incomingTranslationStatus.value === RecordingStatus.Recording;
    const command = shouldStop ? 'stop_incoming_translation' : 'start_incoming_translation';
    const statusBeforeCommand = incomingTranslationStatus.value;
    const sessionBeforeCommand = incomingTranslationSessionId.value;

    incomingTranslationError.value = null;
    if (!shouldStop) {
      incomingTerminalSessionId = null;
    }
    incomingTranslationCommandInFlight.value = true;
    try {
      await invoke<string>(command);
      if (shouldStop) {
        incomingTranslationStatus.value = RecordingStatus.Idle;
        if (incomingTerminalSessionId === sessionBeforeCommand) {
          incomingTerminalSessionId = null;
        }
        if (sessionBeforeCommand !== null) {
          markIncomingTranslationSessionClosed(sessionBeforeCommand, 'stop_command_success');
        }
      } else {
        await reconcileIncomingTranslationState('start_command_success');
      }
    } catch (err) {
      incomingTranslationError.value = String(err);
      incomingTranslationStatus.value = shouldStop ? statusBeforeCommand : RecordingStatus.Error;
    } finally {
      incomingTranslationCommandInFlight.value = false;
    }
  }

  async function runLiveTranslationHealthCheck(): Promise<void> {
    if (!isTauriAvailable() || liveTranslationHealthCheckLoading.value) return;
    const recordingBusy =
      status.value === RecordingStatus.Starting ||
      status.value === RecordingStatus.Recording ||
      status.value === RecordingStatus.Processing;
    const incomingBusy =
      incomingTranslationStatus.value === RecordingStatus.Starting ||
      incomingTranslationStatus.value === RecordingStatus.Recording ||
      incomingTranslationStatus.value === RecordingStatus.Processing;
    if (recordingBusy || incomingBusy) return;
    await executeLiveTranslationHealthCheck();
  }

  async function executeLiveTranslationHealthCheck(): Promise<LiveTranslationHealthCheck | null> {
    if (!isTauriAvailable() || liveTranslationHealthCheckLoading.value) return null;
    liveTranslationHealthCheckLoading.value = true;
    liveTranslationHealthCheckError.value = null;
    try {
      liveTranslationHealthCheck.value = await invoke<LiveTranslationHealthCheck>(
        'run_live_translation_health_check',
      );
      return liveTranslationHealthCheck.value;
    } catch (err) {
      liveTranslationHealthCheckError.value = formatUnknownError(err);
      liveTranslationHealthCheck.value = null;
      return null;
    } finally {
      liveTranslationHealthCheckLoading.value = false;
    }
  }

  function cleanup() {
    listenerGeneration++;
    clearRateLimitRetryTimer();

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
    if (unlistenTranslationDelta) {
      unlistenTranslationDelta();
      unlistenTranslationDelta = null;
    }
    if (unlistenTranslationError) {
      unlistenTranslationError();
      unlistenTranslationError = null;
    }
    if (unlistenIncomingStatus) {
      unlistenIncomingStatus();
      unlistenIncomingStatus = null;
    }
    if (unlistenIncomingSourceFinal) {
      unlistenIncomingSourceFinal();
      unlistenIncomingSourceFinal = null;
    }
    if (unlistenIncomingDelta) {
      unlistenIncomingDelta();
      unlistenIncomingDelta = null;
    }
    if (unlistenIncomingError) {
      unlistenIncomingError();
      unlistenIncomingError = null;
    }

    clearHotkeyStopFinalizeTimer();

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
    sessionId,
    closedSessionIdFloor,
    partialText,
    accumulatedText,
    finalText,
    error,
    errorType,
    errorSummary,
    errorFullText,
    connectionQuality,
    activeRecordingMode,
    translationText,
    incomingTranslationStatus,
    incomingTranslationSessionId,
    incomingSourceText,
    incomingTranslationText,
    incomingTranslationError,
    liveTranslationHealthCheck,
    liveTranslationHealthCheckLoading,
    liveTranslationHealthCheckError,

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
    visibleFinalText,
    visibleAccumulatedText,
    visiblePartialText,
    hasVisibleTranscriptionText,
    isListeningPlaceholder,
    isConnectingPlaceholder,
    displayText,
    isIncomingTranslationActive: computed(
      () =>
        incomingTranslationStatus.value === RecordingStatus.Starting ||
        incomingTranslationStatus.value === RecordingStatus.Recording ||
        incomingTranslationStatus.value === RecordingStatus.Processing
    ),
    hasIncomingTranslationText: computed(() => incomingTranslationText.value.trim().length > 0),
    liveTranslationHealthCheckSummary: computed(() => {
      if (liveTranslationHealthCheckLoading.value) return i18n.global.t('main.healthCheckRunning');
      if (liveTranslationHealthCheckError.value) return liveTranslationHealthCheckError.value;
      if (!liveTranslationHealthCheck.value) return '';
      return liveTranslationHealthCheck.value.ok
        ? i18n.global.t('main.healthCheckReady')
        : i18n.global.t('main.healthCheckNeedsAttention');
    }),

    // Actions
    initialize,
    startRecording,
    reconnect,
    prepareForRustHotkeyStart,
    suppressPreviousTranscriptionDisplay,
    openLicenseActivation,
    stopRecording,
    clearText,
    toggleRecording,
    toggleIncomingTranslation,
    runLiveTranslationHealthCheck,
    reconcileBackendStatus,
    cleanup,
  };
});
