/**
 * Auth Feature - Public API
 * Экспортирует только то, что нужно использовать снаружи фичи
 */

// Domain entities & types
export type { User } from './domain/entities/User';
export type { Session } from './domain/entities/Session';
export type { AuthStatus } from './domain/types';
export { AuthError, AuthErrorCode } from './domain/errors';

// Store
export { useAuthStore } from './store/authStore';

// Composables
export { useAuth } from './presentation/composables/useAuth';
export { useAuthState } from './presentation/composables/useAuthState';
export { useEmailVerification } from './presentation/composables/useEmailVerification';
export { usePasswordReset } from './presentation/composables/usePasswordReset';
export { useOAuth } from './presentation/composables/useOAuth';

// Components
export { default as AuthScreen } from './presentation/components/AuthScreen.vue';
export { default as LoginForm } from './presentation/components/LoginForm.vue';
export { default as VerifyEmailForm } from './presentation/components/VerifyEmailForm.vue';
export { default as PasswordResetForm } from './presentation/components/PasswordResetForm.vue';

// DI Container - для инициализации
export { getAuthContainer, resetAuthContainer } from './infrastructure/di/authContainer';

// API client с auto-refresh - для использования в других частях приложения
export { apiRequest, api } from './infrastructure/api/apiClient';
