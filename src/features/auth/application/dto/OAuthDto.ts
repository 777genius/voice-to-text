import type { Session } from '../../domain/entities/Session';

/**
 * Результат старта OAuth flow
 */
export interface StartOAuthResponse {
  authUrl: string;
}

/**
 * Входные данные для обмена OAuth кода
 */
export interface ExchangeOAuthCodeRequest {
  exchangeCode: string;
}

/**
 * Результат обмена OAuth кода - сессия с токенами
 */
export interface ExchangeOAuthCodeResponse {
  session: Session;
}
