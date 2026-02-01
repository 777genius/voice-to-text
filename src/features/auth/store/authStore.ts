import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import type { AuthStatus } from '../domain/types';
import type { Session } from '../domain/entities/Session';

/**
 * Store для UI state авторизации
 * Только reactive state, вся бизнес-логика в composables и use cases
 */
export const useAuthStore = defineStore('auth', () => {
  // State
  const status = ref<AuthStatus>('idle');
  const session = ref<Session | null>(null);
  const error = ref<string | null>(null);
  const pendingEmail = ref<string | null>(null);
  const userEmail = ref<string | null>(null); // Email текущего пользователя

  // Computed
  const isAuthenticated = computed(() => status.value === 'authenticated');
  const needsVerification = computed(() => status.value === 'needs_verification');
  const isLoading = computed(() => status.value === 'loading');
  const accessToken = computed(() => session.value?.accessToken);

  // Mutations - только для изменения состояния
  function setLoading(): void {
    status.value = 'loading';
    error.value = null;
  }

  function setAuthenticated(newSession: Session, email?: string): void {
    session.value = newSession;
    // Сохраняем email: из параметра, из session.user, или из pendingEmail
    userEmail.value = email ?? newSession.user?.email ?? pendingEmail.value;
    pendingEmail.value = null;
    error.value = null;
    status.value = 'authenticated';
  }

  function setUnauthenticated(): void {
    status.value = 'unauthenticated';
  }

  function setNeedsVerification(email: string): void {
    pendingEmail.value = email;
    status.value = 'needs_verification';
  }

  function setError(message: string): void {
    error.value = message;
  }

  function setStatusError(): void {
    status.value = 'error';
  }

  function setSessionExpired(): void {
    session.value = null;
    status.value = 'unauthenticated';
  }

  function clearError(): void {
    error.value = null;
    if (status.value === 'error') {
      status.value = 'unauthenticated';
    }
  }

  function clearPendingEmail(): void {
    pendingEmail.value = null;
  }

  function reset(): void {
    session.value = null;
    error.value = null;
    pendingEmail.value = null;
    userEmail.value = null;
    status.value = 'unauthenticated';
  }

  return {
    // State
    status,
    session,
    error,
    pendingEmail,
    userEmail,

    // Computed
    isAuthenticated,
    needsVerification,
    isLoading,
    accessToken,

    // Mutations
    setLoading,
    setAuthenticated,
    setUnauthenticated,
    setNeedsVerification,
    setError,
    setStatusError,
    setSessionExpired,
    clearError,
    clearPendingEmail,
    reset,
  };
});
