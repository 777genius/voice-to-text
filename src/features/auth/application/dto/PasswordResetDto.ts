import type { Session } from '../../domain/entities/Session';

/**
 * Входные данные для запроса сброса пароля
 */
export interface StartPasswordResetRequest {
  email: string;
}

/**
 * Входные данные для подтверждения сброса пароля
 */
export interface ConfirmPasswordResetRequest {
  email: string;
  code: string;
  newPassword: string;
}

/**
 * Результат подтверждения сброса - автоматический логин
 */
export interface ConfirmPasswordResetResponse {
  session: Session;
}
