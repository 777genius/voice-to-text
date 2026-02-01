import type { Session } from '../../domain/entities/Session';

/**
 * Входные данные для логина
 */
export interface LoginRequest {
  email: string;
  password: string;
}

/**
 * Результат логина
 */
export interface LoginResponse {
  needsVerification: boolean;
  session?: Session;
}
