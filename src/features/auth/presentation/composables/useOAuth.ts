import { ref, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAuthStore } from '../../store/authStore';
import { getAuthContainer } from '../../infrastructure/di/authContainer';
import { AuthError, AuthErrorCode } from '../../domain/errors';
import { useAuthState } from './useAuthState';
import type { UnsubscribeFn } from '../../application/ports/IDeepLinkListener';

const OAUTH_TIMEOUT_MS = 120000;
const POLL_INTERVAL_MS = 2000;

/**
 * Composable для OAuth авторизации
 */
export function useOAuth() {
  const { t } = useI18n();
  const store = useAuthStore();
  const container = getAuthContainer();
  const state = useAuthState();

  let unsubscribeDeepLink: UnsubscribeFn | null = null;
  let subscribeDeepLinkPromise: Promise<UnsubscribeFn | null> | null = null;
  let startOAuthPromise: Promise<void> | null = null;
  let oauthTimeoutId: ReturnType<typeof setTimeout> | null = null;
  let pollIntervalId: ReturnType<typeof setInterval> | null = null;
  let oauthGeneration = 0;

  // Для защиты от двойной обработки
  const lastProcessedCode = ref<string | null>(null);
  // Флаг: OAuth уже завершён (deep link или polling)
  let oauthCompleted = false;

  function handleError(e: unknown): void {
    if (e instanceof AuthError) {
      switch (e.code) {
        case AuthErrorCode.OAuthError:
          store.setError(t('auth.errors.oauthError'));
          break;
        case AuthErrorCode.OAuthAccountLinked:
          store.setError(t('auth.errors.oauthAccountLinked'));
          break;
        case AuthErrorCode.NetworkError:
          store.setError(t('auth.errors.networkError'));
          break;
        default:
          store.setError(e.message);
      }
    } else {
      console.error('OAuth: необработанная ошибка', e);
      store.setError(t('auth.errors.generic'));
    }
    store.setStatusError();
  }

  function stopPolling(): void {
    if (pollIntervalId) {
      clearInterval(pollIntervalId);
      pollIntervalId = null;
    }
  }

  function clearOAuthTimeout(): void {
    if (oauthTimeoutId) {
      clearTimeout(oauthTimeoutId);
      oauthTimeoutId = null;
    }
  }

  function parseOAuthCallback(urlString: string): { exchangeCode?: string; error?: string } | null {
    let url: URL;
    try {
      url = new URL(urlString);
    } catch {
      return null;
    }

    const isOAuthCallback =
      url.host === 'oauth' ||
      url.pathname.startsWith('/oauth/callback') ||
      (url.host === 'oauth' && url.pathname === '/callback');

    if (!isOAuthCallback) {
      return null;
    }

    const params = new URLSearchParams(url.search);
    return {
      exchangeCode: params.get('exchange_code') || undefined,
      error: params.get('error') || undefined,
    };
  }

  function isCurrentOAuthGeneration(generation: number): boolean {
    return generation === oauthGeneration;
  }

  async function handleDeepLink(urlString: string): Promise<void> {
    const generation = oauthGeneration;
    const result = parseOAuthCallback(urlString);
    if (!result || oauthCompleted || !isCurrentOAuthGeneration(generation)) return;

    clearOAuthTimeout();
    stopPolling();

    if (result.error) {
      oauthCompleted = true;
      store.setError(t('auth.errors.googleError', { error: decodeURIComponent(result.error) }));
      store.setUnauthenticated({ preserveError: true });
      return;
    }

    if (result.exchangeCode) {
      // Защита от двойной обработки
      if (result.exchangeCode === lastProcessedCode.value || oauthCompleted) {
        return;
      }
      lastProcessedCode.value = result.exchangeCode;
      oauthCompleted = true;

      store.setLoading();

      try {
        const response = await container.exchangeOAuthCodeUseCase.execute({
          exchangeCode: result.exchangeCode,
        });
        if (!isCurrentOAuthGeneration(generation)) return;
        store.setAuthenticated(response.session);
      } catch (e) {
        if (isCurrentOAuthGeneration(generation)) {
          handleError(e);
        }
      }
    }
  }

  async function ensureDeepLinkSubscribed(generation: number): Promise<boolean> {
    if (unsubscribeDeepLink) return true;
    if (subscribeDeepLinkPromise) {
      return Boolean(await subscribeDeepLinkPromise);
    }

    const subscribePromise = container.deepLinkListener
      .subscribe(handleDeepLink)
      .then((unsubscribe) => {
        if (generation !== oauthGeneration) {
          unsubscribe();
          return null;
        }
        unsubscribeDeepLink = unsubscribe;
        return unsubscribe;
      })
      .finally(() => {
        if (subscribeDeepLinkPromise === subscribePromise) {
          subscribeDeepLinkPromise = null;
        }
      });
    subscribeDeepLinkPromise = subscribePromise;

    return Boolean(await subscribeDeepLinkPromise);
  }

  function startPolling(generation: number): void {
    stopPolling();

    const deviceId = container.tokenRepository.getDeviceId();

    pollIntervalId = setInterval(async () => {
      if (generation !== oauthGeneration) {
        stopPolling();
        return;
      }

      // Если OAuth уже завершён (через deep link), прекращаем
      if (oauthCompleted) {
        stopPolling();
        return;
      }

      try {
        const result = await container.authRepository.pollOAuth(deviceId);
        if (!isCurrentOAuthGeneration(generation) || oauthCompleted) return;

        if (result.status === 'completed' && result.session) {
          oauthCompleted = true;
          stopPolling();
          clearOAuthTimeout();

          // Сохраняем сессию
          await container.tokenRepository.save(result.session);
          if (!isCurrentOAuthGeneration(generation)) return;
          store.setAuthenticated(result.session);

          // Переключаем фокус на окно приложения
          try {
            await getCurrentWindow().setFocus();
          } catch {
            // В dev-режиме может не сработать, не критично
          }
        }
      } catch {
        // Ошибки polling не критичны — deep link может сработать
      }
    }, POLL_INTERVAL_MS);
  }

  async function startGoogleOAuth(): Promise<void> {
    if (startOAuthPromise) return startOAuthPromise;

    const generation = ++oauthGeneration;
    store.setLoading();
    oauthCompleted = false;

    const promise = startGoogleOAuthInternal(generation).finally(() => {
      if (startOAuthPromise === promise) {
        startOAuthPromise = null;
      }
    });
    startOAuthPromise = promise;
    return promise;
  }

  async function startGoogleOAuthInternal(generation: number): Promise<void> {
    try {
      // Подписываемся на deep link события
      const subscribed = await ensureDeepLinkSubscribed(generation);
      if (!subscribed || generation !== oauthGeneration) return;

      await container.startGoogleOAuthUseCase.execute();
      if (generation !== oauthGeneration) return;

      // Запускаем polling параллельно с deep link
      startPolling(generation);

      // Timeout на случай если ни deep link, ни polling не сработает
      clearOAuthTimeout();
      oauthTimeoutId = setTimeout(() => {
        if (generation !== oauthGeneration) return;
        stopPolling();
        if (store.isLoading) {
          store.setUnauthenticated();
        }
        oauthTimeoutId = null;
      }, OAUTH_TIMEOUT_MS);
    } catch (e) {
      if (generation === oauthGeneration) {
        handleError(e);
      }
    }
  }

  function cancelOAuth(): void {
    oauthGeneration++;
    startOAuthPromise = null;
    subscribeDeepLinkPromise = null;
    clearOAuthTimeout();
    stopPolling();
    oauthCompleted = true;
    if (store.isLoading) {
      store.setUnauthenticated();
    }
  }

  function cleanup(): void {
    oauthGeneration++;
    startOAuthPromise = null;
    subscribeDeepLinkPromise = null;
    clearOAuthTimeout();
    stopPolling();
    oauthCompleted = true;
    if (unsubscribeDeepLink) {
      unsubscribeDeepLink();
      unsubscribeDeepLink = null;
    }
  }

  onUnmounted(cleanup);

  return {
    ...state,
    startGoogleOAuth,
    cancelOAuth,
    cleanup,
  };
}
