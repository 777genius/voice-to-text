/**
 * Типы для API запросов/ответов
 * Отражают формат бэкенда (snake_case)
 */

export interface ApiLoginRequest {
  email: string;
  password: string;
  device_id: string;
}

export interface ApiUserResponse {
  id: string;
  email: string;
  email_verified: boolean;
}

export interface ApiLoginResponse {
  needs_verification: boolean;
  access_token?: string;
  refresh_token?: string;
  access_expires_at?: string;
  refresh_expires_at?: string;
  user?: ApiUserResponse;
}

export interface ApiRegisterRequest {
  email: string;
  password: string;
  device_id: string;
}

export interface ApiRegisterResponse {
  needs_verification: boolean;
}

export interface ApiVerifyEmailRequest {
  email: string;
  code: string;
  device_id: string;
}

export interface ApiOAuthStartRequest {
  device_id: string;
  app_redirect_uri: string;
}

export interface ApiOAuthStartResponse {
  auth_url: string;
}

export interface ApiOAuthExchangeRequest {
  device_id: string;
  exchange_code: string;
}

export interface ApiOAuthPollRequest {
  device_id: string;
}

export interface ApiOAuthPollResponse {
  status: 'pending' | 'completed';
  access_token?: string;
  refresh_token?: string;
  access_expires_at?: string;
  refresh_expires_at?: string;
}

export interface ApiRefreshRequest {
  refresh_token: string;
  device_id: string;
}

export interface ApiLogoutRequest {
  refresh_token: string;
}

export interface ApiPasswordResetStartRequest {
  email: string;
}

export interface ApiPasswordResetConfirmRequest {
  email: string;
  code: string;
  new_password: string;
  device_id: string;
}

export interface ApiResendVerificationRequest {
  email: string;
}

/**
 * Обёртка успешного ответа API
 */
export interface ApiResponse<T> {
  data: T;
}

/**
 * Формат ошибки API
 */
export interface ApiErrorResponse {
  error?: {
    code?: string;
    message?: string;
    retry_after_ms?: number;
  };
}
