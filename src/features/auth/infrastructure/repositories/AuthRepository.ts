import type { IAuthRepository, LoginResult } from '../../domain/repositories/IAuthRepository';
import type { Session } from '../../domain/entities/Session';
import type { User } from '../../domain/entities/User';
import { createSession } from '../../domain/entities/Session';
import { createUser } from '../../domain/entities/User';
import { AuthApiClient } from '../api/authApiClient';
import type { ApiLoginResponse } from '../api/apiTypes';
import { AuthError, AuthErrorCode } from '../../domain/errors';

/**
 * Реализация репозитория авторизации через HTTP API
 */
export class AuthRepository implements IAuthRepository {
  constructor(private readonly apiClient: AuthApiClient) {}

  async login(email: string, password: string, deviceId: string): Promise<LoginResult> {
    const response = await this.apiClient.login({
      email,
      password,
      device_id: deviceId,
    });

    if (response.needs_verification) {
      return { needsVerification: true };
    }

    return {
      needsVerification: false,
      session: this.parseSession(response, deviceId),
    };
  }

  async register(email: string, password: string, deviceId: string): Promise<void> {
    await this.apiClient.register({
      email,
      password,
      device_id: deviceId,
    });
  }

  async verifyEmail(email: string, code: string, deviceId: string): Promise<Session> {
    const response = await this.apiClient.verifyEmail({
      email,
      code,
      device_id: deviceId,
    });
    return this.parseSession(response, deviceId);
  }

  async resendVerificationCode(email: string): Promise<void> {
    await this.apiClient.resendVerification({ email });
  }

  async startOAuth(deviceId: string, redirectUri: string): Promise<string> {
    const response = await this.apiClient.startOAuth({
      device_id: deviceId,
      app_redirect_uri: redirectUri,
    });
    return response.auth_url;
  }

  async pollOAuth(deviceId: string): Promise<{ status: string; session?: Session }> {
    const response = await this.apiClient.pollOAuth({ device_id: deviceId });

    if (response.status === 'completed' && response.access_token && response.access_expires_at) {
      const session = createSession({
        accessToken: response.access_token,
        refreshToken: response.refresh_token,
        accessExpiresAt: new Date(response.access_expires_at),
        refreshExpiresAt: response.refresh_expires_at
          ? new Date(response.refresh_expires_at)
          : undefined,
        deviceId,
      });
      return { status: 'completed', session };
    }

    return { status: 'pending' };
  }

  async exchangeOAuthCode(exchangeCode: string, deviceId: string): Promise<Session> {
    const response = await this.apiClient.exchangeOAuth({
      device_id: deviceId,
      exchange_code: exchangeCode,
    });
    return this.parseSession(response, deviceId);
  }

  async refreshTokens(refreshToken: string, deviceId: string): Promise<Session> {
    const response = await this.apiClient.refresh({
      refresh_token: refreshToken,
      device_id: deviceId,
    });
    return this.parseSession(response, deviceId);
  }

  async logout(refreshToken: string): Promise<void> {
    await this.apiClient.logout({ refresh_token: refreshToken });
  }

  async requestPasswordReset(email: string): Promise<void> {
    await this.apiClient.passwordResetStart({ email });
  }

  async confirmPasswordReset(
    email: string,
    code: string,
    newPassword: string,
    deviceId: string
  ): Promise<Session> {
    const response = await this.apiClient.passwordResetConfirm({
      email,
      code,
      new_password: newPassword,
      device_id: deviceId,
    });
    return this.parseSession(response, deviceId);
  }

  async getCurrentUser(accessToken: string): Promise<User> {
    const response = await this.apiClient.getCurrentUser(accessToken);
    return createUser({
      id: response.id,
      email: response.email,
      emailVerified: response.email_verified,
    });
  }

  private parseSession(response: ApiLoginResponse, deviceId?: string): Session {
    if (!response.access_token || !response.access_expires_at) {
      throw new AuthError(AuthErrorCode.Unknown, 'Сервер не вернул токены');
    }

    // Парсим user если бэкенд его вернул
    const user = response.user
      ? createUser({
          id: response.user.id,
          email: response.user.email,
          emailVerified: response.user.email_verified,
        })
      : undefined;

    return createSession({
      accessToken: response.access_token,
      refreshToken: response.refresh_token,
      accessExpiresAt: new Date(response.access_expires_at),
      refreshExpiresAt: response.refresh_expires_at
        ? new Date(response.refresh_expires_at)
        : undefined,
      deviceId,
      user,
    });
  }
}
