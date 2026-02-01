import { configureRefreshFetch, type ResponseError } from 'refresh-fetch';
import { getTokenRepository } from '../repositories/TokenRepository';
import { createSession } from '../../domain/entities/Session';
import { createUser } from '../../domain/entities/User';
import { AuthError, AuthErrorCode } from '../../domain/errors';
import { runRefreshSingleFlight } from '../../application/services/refreshSingleFlight';
import { isTauriAvailable } from '../../../../utils/tauri';
import { useAuthStore } from '../../store/authStore';

const API_BASE = import.meta.env.VITE_API_URL || 'https://api.voicetotext.app';
const REQUEST_TIMEOUT_MS = 30000;

async function syncAuthWithTauriBackend(params: {
  authenticated: boolean;
  token: string | null;
}): Promise<void> {
  if (!isTauriAvailable()) return;

  try {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('set_authenticated', {
      authenticated: params.authenticated,
      token: params.token,
    });
  } catch (e) {
    console.warn('[Auth] Failed to sync token with Tauri backend:', e);
  }
}

function trySyncAuthStoreSession(session: ReturnType<typeof createSession> | null): void {
  // apiClient может вызываться до того, как Pinia активирована (например, в тестах).
  // В таком случае просто пропускаем sync.
  try {
    const store = useAuthStore();
    if (session) {
      store.setAuthenticated(session);
    } else {
      store.setSessionExpired();
    }
  } catch {}
}

/**
 * Обновление access токена через refresh endpoint
 * Вызывается автоматически при получении 401
 */
async function refreshToken(): Promise<void> {
  await runRefreshSingleFlight(async () => {
    const tokenRepo = getTokenRepository();
    const session = await tokenRepo.get();

    if (!session?.refreshToken) {
      throw new AuthError(AuthErrorCode.SessionExpired, 'No refresh token');
    }

    const usedRefreshToken = session.refreshToken;
    const deviceId = session.deviceId || tokenRepo.getDeviceId();
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

    let response: Response;
    try {
      response = await fetch(`${API_BASE}/api/v1/auth/refresh`, {
        method: 'POST',
        signal: controller.signal,
        headers: {
          'Content-Type': 'application/json',
          'X-Client-Type': 'native',
        },
        body: JSON.stringify({
          refresh_token: session.refreshToken,
          device_id: deviceId,
        }),
      });
    } catch (e) {
      // Важно: при проблемах сети/таймауте НЕ разлогиниваем пользователя.
      if (e instanceof DOMException && e.name === 'AbortError') {
        throw new AuthError(AuthErrorCode.NetworkError, 'Время ожидания истекло');
      }
      throw new AuthError(AuthErrorCode.NetworkError, 'Ошибка сети');
    } finally {
      clearTimeout(timeoutId);
    }

    if (!response.ok) {
      // Refresh может не удаться по разным причинам.
      // Разлогиниваем ТОЛЬКО когда сервер явно говорит "refresh токен невалиден" (401).
      // В остальных случаях (5xx/429 и т.д.) сохраняем токены и даём пользователю шанс повторить позже.
      const body = await response.json().catch(() => ({}));
      const errorData = body as { error?: { code?: string; message?: string; details?: any } };
      const message = errorData?.error?.message || 'Неизвестная ошибка';

      if (response.status === 401) {
        // Частый кейс в desktop multi-window:
        // другое окно успело сделать refresh (rotation), а мы ещё шлём старый refresh_token.
        // В этом случае НЕ очищаем storage, а просто подхватываем актуальную сессию.
        const currentSession = await tokenRepo.get();
        if (currentSession?.refreshToken && currentSession.refreshToken !== usedRefreshToken) {
          trySyncAuthStoreSession(currentSession);
          await syncAuthWithTauriBackend({
            authenticated: true,
            token: currentSession.accessToken,
          });
          return;
        }

        await tokenRepo.clear();
        trySyncAuthStoreSession(null);
        await syncAuthWithTauriBackend({ authenticated: false, token: null });
        throw new AuthError(AuthErrorCode.SessionExpired, message);
      }

      if (response.status === 429) {
        const retryAfterSeconds = Number(errorData?.error?.details?.retry_after_seconds);
        const retryAfterMs = Number.isFinite(retryAfterSeconds) ? retryAfterSeconds * 1000 : undefined;
        throw new AuthError(AuthErrorCode.RateLimitExceeded, message, retryAfterMs);
      }

      throw new AuthError(AuthErrorCode.NetworkError, message);
    }

    const json = await response.json();
    if (!json.data?.access_token) {
      throw new AuthError(AuthErrorCode.Unknown, 'Invalid refresh response');
    }

    // Парсим user из ответа или сохраняем из текущей сессии
    const userData = json.data.user;
    const user = userData
      ? createUser({
          id: userData.id,
          email: userData.email,
          emailVerified: userData.email_verified,
        })
      : session.user;

    // Сохраняем новую сессию с user
    const newSession = createSession({
      accessToken: json.data.access_token,
      refreshToken: json.data.refresh_token || session.refreshToken,
      accessExpiresAt: new Date(json.data.access_expires_at),
      refreshExpiresAt: json.data.refresh_expires_at
        ? new Date(json.data.refresh_expires_at)
        : session.refreshExpiresAt,
      deviceId: session.deviceId || deviceId,
      user,
    });
    await tokenRepo.save(newSession);
    trySyncAuthStoreSession(newSession);
    await syncAuthWithTauriBackend({
      authenticated: true,
      token: newSession.accessToken,
    });
  });
}

/**
 * Определяет нужно ли обновлять токен по ошибке
 */
function shouldRefreshToken(error: ResponseError): boolean {
  return error.status === 401;
}

/**
 * Базовая fetch функция с timeout и headers
 */
async function baseFetch(
  url: RequestInfo | URL,
  options: RequestInit = {}
): Promise<Response> {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  // Получаем текущий access token
  const tokenRepo = getTokenRepository();
  const session = await tokenRepo.get();

  const fetchOptions: RequestInit = {
    ...options,
    signal: controller.signal,
    headers: {
      'Content-Type': 'application/json',
      'X-Client-Type': 'native',
      ...(session?.accessToken && { Authorization: `Bearer ${session.accessToken}` }),
      ...options.headers,
    },
  };

  let response: Response;
  try {
    response = await fetch(url, fetchOptions);
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') {
      throw new AuthError(AuthErrorCode.NetworkError, 'Время ожидания истекло');
    }
    throw new AuthError(AuthErrorCode.NetworkError, 'Ошибка сети');
  } finally {
    clearTimeout(timeoutId);
  }

  // Превращаем не-ok ответы в ошибки для refresh-fetch
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    const error = new Error('Response error') as ResponseError;
    error.name = 'ResponseError';
    error.status = response.status;
    error.response = response;
    error.body = body;
    throw error;
  }

  return response;
}

// Сконфигурированный fetch с авто-обновлением токена
const refreshFetch = configureRefreshFetch({
  refreshToken,
  shouldRefreshToken,
  fetch: baseFetch,
});

/**
 * API клиент для защищённых эндпоинтов
 * Автоматически обновляет токены при 401 и ставит запросы в очередь
 */
export async function apiRequest<T>(
  path: string,
  options: RequestInit = {}
): Promise<T> {
  const url = `${API_BASE}${path}`;

  try {
    const response = await refreshFetch(url, options);
    const json = await response.json();

    // Стандартный формат ответа API: { data: T }
    if (json.data === undefined) {
      throw new AuthError(AuthErrorCode.Unknown, 'Неверный формат ответа сервера');
    }

    return json.data;
  } catch (e) {
    // Если это уже AuthError - пробрасываем
    if (e instanceof AuthError) {
      throw e;
    }

    // Преобразуем ResponseError в AuthError
    const err = e as ResponseError;
    if (err.status) {
      const errorData = err.body as { error?: { code?: string; message?: string } };
      const code = errorData?.error?.code;
      const message = errorData?.error?.message || 'Неизвестная ошибка';

      switch (code) {
        case 'SESSION_EXPIRED':
        case 'TOKEN_EXPIRED':
          return Promise.reject(new AuthError(AuthErrorCode.SessionExpired, message));
        case 'RATE_LIMIT_EXCEEDED':
          return Promise.reject(new AuthError(AuthErrorCode.RateLimitExceeded, message));
        default:
          if (err.status === 401) {
            return Promise.reject(new AuthError(AuthErrorCode.SessionExpired, message));
          }
          return Promise.reject(new AuthError(AuthErrorCode.Unknown, message));
      }
    }

    throw e;
  }
}

// Удобные методы
export const api = {
  get: <T>(path: string) => apiRequest<T>(path, { method: 'GET' }),

  post: <T>(path: string, body?: unknown) =>
    apiRequest<T>(path, {
      method: 'POST',
      body: body ? JSON.stringify(body) : undefined,
    }),

  put: <T>(path: string, body?: unknown) =>
    apiRequest<T>(path, {
      method: 'PUT',
      body: body ? JSON.stringify(body) : undefined,
    }),

  delete: <T>(path: string) => apiRequest<T>(path, { method: 'DELETE' }),
};
