import { useI18n } from 'vue-i18n';
import { useAuthStore } from '../../store/authStore';
import { getAuthContainer } from '../../infrastructure/di/authContainer';
import { AuthError, AuthErrorCode } from '../../domain/errors';
import { useAuthState } from './useAuthState';

/**
 * Основной composable для авторизации
 * Связывает use cases с UI через store
 */
export function useAuth() {
  const { t } = useI18n();
  const store = useAuthStore();
  const container = getAuthContainer();
  const state = useAuthState();

  type InitializeOptions = {
    /**
     * Не переводит store в статус loading.
     * Удобно для синхронизации между окнами, чтобы UI не "прыгал" на auth экран.
     */
    silent?: boolean;
  };

  function handleError(e: unknown): void {
    if (e instanceof AuthError) {
      switch (e.code) {
        case AuthErrorCode.InvalidCredentials:
          store.setError(t('auth.errors.invalidCredentials'));
          break;
        case AuthErrorCode.EmailNotVerified:
          store.setError(t('auth.errors.emailNotVerified'));
          break;
        case AuthErrorCode.CodeInvalid:
          store.setError(t('auth.errors.codeInvalid'));
          break;
        case AuthErrorCode.CodeExpired:
          store.setError(t('auth.errors.codeExpired'));
          break;
        case AuthErrorCode.RateLimited:
        case AuthErrorCode.RateLimitExceeded: {
          const seconds = Math.ceil((e.retryAfterMs || 60000) / 1000);
          store.setError(t('auth.errors.rateLimited', { seconds }));
          break;
        }
        case AuthErrorCode.UserNotFound:
          store.setError(t('auth.errors.userNotFound'));
          break;
        case AuthErrorCode.NetworkError:
          store.setError(
            e.message.includes('истекло') || e.message.includes('timed out')
              ? t('auth.errors.timeout')
              : t('auth.errors.networkError')
          );
          break;
        case AuthErrorCode.PasswordWeak:
          store.setError(t('auth.errors.passwordWeak'));
          break;
        case AuthErrorCode.OAuthError:
          store.setError(t('auth.errors.oauthError'));
          break;
        case AuthErrorCode.OAuthAccountLinked:
          store.setError(t('auth.errors.oauthAccountLinked'));
          break;
        case AuthErrorCode.ValidationError:
          store.setError(e.message);
          break;
        case AuthErrorCode.SessionExpired:
          store.setError(t('auth.errors.sessionExpired'));
          store.setSessionExpired();
          return;
        default:
          store.setError(e.message);
      }
    } else {
      store.setError(t('auth.errors.generic'));
    }
    store.setStatusError();
  }

  async function initialize(options: InitializeOptions = {}): Promise<void> {
    if (!options.silent) {
      store.setLoading();
    }

    try {
      const result = await container.initializeAuthUseCase.execute();

      if (result.isAuthenticated && result.session) {
        store.setAuthenticated(result.session);
      } else if (result.sessionExpired) {
        store.setError(t('auth.errors.sessionExpired'));
        store.setUnauthenticated();
      } else {
        store.setUnauthenticated();
      }
    } catch (e) {
      if (e instanceof AuthError && e.code === AuthErrorCode.SessionExpired) {
        store.setError(t('auth.errors.sessionExpired'));
      }
      store.setUnauthenticated();
    }
  }

  async function login(email: string, password: string): Promise<void> {
    store.setLoading();

    try {
      const result = await container.loginUseCase.execute({ email, password });

      if (result.needsVerification) {
        store.setNeedsVerification(email);
      } else if (result.session) {
        store.setAuthenticated(result.session, email);
      }
    } catch (e) {
      handleError(e);
    }
  }

  async function register(email: string, password: string): Promise<void> {
    store.setLoading();

    try {
      await container.registerUseCase.execute({ email, password });
      store.setNeedsVerification(email);
    } catch (e) {
      handleError(e);
    }
  }

  async function logout(): Promise<void> {
    await container.logoutUseCase.execute();
    store.reset();
  }

  function clearError(): void {
    store.clearError();
  }

  return {
    ...state,
    initialize,
    login,
    register,
    logout,
    clearError,
  };
}
