import { AuthError, AuthErrorCode } from '../../domain/errors';
import type {
  ApiLoginRequest,
  ApiLoginResponse,
  ApiRegisterRequest,
  ApiRegisterResponse,
  ApiVerifyEmailRequest,
  ApiOAuthStartRequest,
  ApiOAuthStartResponse,
  ApiOAuthExchangeRequest,
  ApiOAuthPollRequest,
  ApiOAuthPollResponse,
  ApiRefreshRequest,
  ApiLogoutRequest,
  ApiPasswordResetStartRequest,
  ApiPasswordResetConfirmRequest,
  ApiResendVerificationRequest,
  ApiUserResponse,
  ApiErrorResponse,
} from './apiTypes';

const API_BASE = import.meta.env.VITE_API_URL || 'https://api.voicetotext.app';
const REQUEST_TIMEOUT_MS = 30000;

/**
 * Чистый HTTP клиент для auth API
 * Без бизнес-логики, только транспорт
 */
export class AuthApiClient {
  private async request<T>(
    path: string,
    options: RequestInit = {}
  ): Promise<T> {
    const url = `${API_BASE}/api/v1/auth${path}`;
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

    let response: Response;
    try {
      response = await fetch(url, {
        ...options,
        signal: controller.signal,
        headers: {
          'Content-Type': 'application/json',
          'X-Client-Type': 'native',
          ...options.headers,
        },
      });
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        throw new AuthError(AuthErrorCode.NetworkError, 'Время ожидания истекло');
      }
      throw new AuthError(AuthErrorCode.NetworkError, 'Ошибка сети');
    } finally {
      clearTimeout(timeoutId);
    }

    if (!response.ok) {
      const error = await response.json().catch(() => ({}));
      throw this.mapError(response.status, error as ApiErrorResponse);
    }

    const json = await response.json().catch(() => {
      throw new AuthError(AuthErrorCode.Unknown, 'Неверный формат ответа сервера');
    });

    if (json.data === undefined) {
      throw new AuthError(AuthErrorCode.Unknown, 'Неверный формат ответа сервера');
    }

    return json.data;
  }

  private mapError(status: number, error: ApiErrorResponse): AuthError {
    const errorData = error.error;
    const code = errorData?.code;
    const message = errorData?.message || 'Неизвестная ошибка';

    switch (code) {
      case 'INVALID_CREDENTIALS':
        return new AuthError(AuthErrorCode.InvalidCredentials, message);
      case 'EMAIL_NOT_VERIFIED':
        return new AuthError(AuthErrorCode.EmailNotVerified, message);
      case 'USER_NOT_FOUND':
        return new AuthError(AuthErrorCode.UserNotFound, message);
      case 'CODE_INVALID':
        return new AuthError(AuthErrorCode.CodeInvalid, message);
      case 'CODE_EXPIRED':
        return new AuthError(AuthErrorCode.CodeExpired, message);
      case 'RATE_LIMIT_EXCEEDED':
        return new AuthError(
          AuthErrorCode.RateLimitExceeded,
          message,
          errorData?.retry_after_ms
        );
      case 'PASSWORD_WEAK':
        return new AuthError(AuthErrorCode.PasswordWeak, message);
      case 'OAUTH_ERROR':
        return new AuthError(AuthErrorCode.OAuthError, message);
      case 'OAUTH_ACCOUNT_ALREADY_LINKED':
        return new AuthError(AuthErrorCode.OAuthAccountLinked, message);
      default:
        if (status === 401) {
          return new AuthError(AuthErrorCode.SessionExpired, message);
        }
        if (status === 429) {
          return new AuthError(
            AuthErrorCode.RateLimitExceeded,
            message,
            errorData?.retry_after_ms
          );
        }
        return new AuthError(AuthErrorCode.Unknown, message);
    }
  }

  async login(req: ApiLoginRequest): Promise<ApiLoginResponse> {
    return this.request('/login', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async register(req: ApiRegisterRequest): Promise<ApiRegisterResponse> {
    return this.request('/register', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async verifyEmail(req: ApiVerifyEmailRequest): Promise<ApiLoginResponse> {
    return this.request('/email/verify', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async resendVerification(req: ApiResendVerificationRequest): Promise<void> {
    return this.request('/email/resend', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async startOAuth(req: ApiOAuthStartRequest): Promise<ApiOAuthStartResponse> {
    return this.request('/oauth/google/start', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async exchangeOAuth(req: ApiOAuthExchangeRequest): Promise<ApiLoginResponse> {
    return this.request('/oauth/exchange', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async pollOAuth(req: ApiOAuthPollRequest): Promise<ApiOAuthPollResponse> {
    return this.request('/oauth/poll', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async refresh(req: ApiRefreshRequest): Promise<ApiLoginResponse> {
    return this.request('/refresh', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async logout(req: ApiLogoutRequest): Promise<void> {
    return this.request('/logout', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async passwordResetStart(req: ApiPasswordResetStartRequest): Promise<void> {
    return this.request('/password/reset/start', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async passwordResetConfirm(req: ApiPasswordResetConfirmRequest): Promise<ApiLoginResponse> {
    return this.request('/password/reset/confirm', {
      method: 'POST',
      body: JSON.stringify(req),
    });
  }

  async getCurrentUser(accessToken: string): Promise<ApiUserResponse> {
    return this.request('/me', {
      method: 'GET',
      headers: {
        'Authorization': `Bearer ${accessToken}`,
      },
    });
  }
}
