import { computed } from 'vue';
import { useAuthStore } from '../../store/authStore';

/**
 * Composable для read-only доступа к состоянию авторизации
 * Используется компонентами для отображения UI
 */
export function useAuthState() {
  const store = useAuthStore();

  return {
    // State
    status: computed(() => store.status),
    error: computed(() => store.error),
    pendingEmail: computed(() => store.pendingEmail),
    accessToken: computed(() => store.accessToken),

    // Computed flags
    isAuthenticated: computed(() => store.isAuthenticated),
    isLoading: computed(() => store.isLoading),
    needsVerification: computed(() => store.needsVerification),
    hasError: computed(() => store.status === 'error'),
  };
}
