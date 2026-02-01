import type { Session } from '../../domain/entities/Session';

/**
 * Входные данные для верификации email
 */
export interface VerifyEmailRequest {
  email: string;
  code: string;
}

/**
 * Результат верификации - сессия с токенами
 */
export interface VerifyEmailResponse {
  session: Session;
}
