import { ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useAuthStore } from '../../store/authStore';
import { getAuthContainer } from '../../infrastructure/di/authContainer';
import { AuthError, AuthErrorCode } from '../../domain/errors';
import { useAuthState } from './useAuthState';

export type PasswordResetStep = 'email' | 'code' | 'password';

/**
 * Composable для сброса пароля
 */
export function usePasswordReset() {
  const { t } = useI18n();
  const store = useAuthStore();
  const container = getAuthContainer();
  const state = useAuthState();

  const step = ref<PasswordResetStep>('email');
  const resetEmail = ref('');

  function handleError(e: unknown): void {
    if (e instanceof AuthError) {
      switch (e.code) {
        case AuthErrorCode.CodeInvalid:
          store.setError(t('auth.errors.codeInvalid'));
          break;
        case AuthErrorCode.CodeExpired:
          store.setError(t('auth.errors.codeExpired'));
          break;
        case AuthErrorCode.PasswordWeak:
          store.setError(t('auth.errors.passwordWeak'));
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

  async function startReset(email: string): Promise<boolean> {
    store.setLoading();

    try {
      await container.startPasswordResetUseCase.execute({ email });
      resetEmail.value = email;
      step.value = 'code';
      store.setUnauthenticated();
      return true;
    } catch (e) {
      handleError(e);
      return false;
    }
  }

  function submitCode(): void {
    step.value = 'password';
  }

  async function confirmReset(code: string, newPassword: string): Promise<void> {
    if (!resetEmail.value) {
      store.setError(t('auth.errors.emailNotSet'));
      return;
    }

    store.setLoading();

    try {
      const email = resetEmail.value;
      const result = await container.confirmPasswordResetUseCase.execute({
        email,
        code,
        newPassword,
      });
      resetEmail.value = '';
      step.value = 'email';
      store.setAuthenticated(result.session, email);
    } catch (e) {
      handleError(e);
    }
  }

  function goBack(): void {
    if (step.value === 'email') {
      return;
    } else if (step.value === 'code') {
      step.value = 'email';
    } else {
      step.value = 'code';
    }
  }

  function reset(): void {
    step.value = 'email';
    resetEmail.value = '';
    store.clearError();
  }

  function clearError(): void {
    store.clearError();
  }

  return {
    ...state,
    step,
    resetEmail,
    startReset,
    submitCode,
    confirmReset,
    goBack,
    reset,
    clearError,
  };
}
