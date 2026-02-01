import { useI18n } from 'vue-i18n';
import { useAuthStore } from '../../store/authStore';
import { getAuthContainer } from '../../infrastructure/di/authContainer';
import { AuthError, AuthErrorCode } from '../../domain/errors';
import { useAuthState } from './useAuthState';

/**
 * Composable для верификации email
 */
export function useEmailVerification() {
  const { t } = useI18n();
  const store = useAuthStore();
  const container = getAuthContainer();
  const state = useAuthState();

  function handleError(e: unknown): void {
    if (e instanceof AuthError) {
      switch (e.code) {
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
          store.setError(t('auth.errors.networkError'));
          break;
        default:
          store.setError(e.message);
      }
    } else {
      store.setError(t('auth.errors.generic'));
    }
    store.setStatusError();
  }

  async function verify(code: string): Promise<void> {
    const email = store.pendingEmail;
    if (!email) {
      store.setError(t('auth.errors.emailNotSet'));
      return;
    }

    store.setLoading();

    try {
      const result = await container.verifyEmailUseCase.execute({ email, code });
      store.setAuthenticated(result.session, email);
    } catch (e) {
      handleError(e);
    }
  }

  async function resend(): Promise<void> {
    const email = store.pendingEmail;
    if (!email) return;

    try {
      await container.resendVerificationUseCase.execute(email);
    } catch (e) {
      handleError(e);
      throw e;
    }
  }

  function goBack(): void {
    store.clearPendingEmail();
    store.setUnauthenticated();
  }

  function clearError(): void {
    store.clearError();
  }

  return {
    ...state,
    verify,
    resend,
    goBack,
    clearError,
  };
}
