/**
 * Входные данные для регистрации
 */
export interface RegisterRequest {
  email: string;
  password: string;
}

/**
 * Результат регистрации - всегда требуется верификация
 */
export interface RegisterResponse {
  needsVerification: true;
}
