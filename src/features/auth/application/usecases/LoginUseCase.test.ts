import { describe, it, expect, vi, beforeEach } from 'vitest';
import { LoginUseCase } from './LoginUseCase';
import type { IAuthRepository, LoginResult } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import { AuthError, AuthErrorCode } from '../../domain/errors';

describe('LoginUseCase', () => {
  let loginUseCase: LoginUseCase;
  let mockAuthRepository: IAuthRepository;
  let mockTokenRepository: ITokenRepository;

  const mockSession: Session = {
    accessToken: 'access-token',
    refreshToken: 'refresh-token',
    accessExpiresAt: new Date(Date.now() + 3600000),
    refreshExpiresAt: new Date(Date.now() + 86400000),
  };

  beforeEach(() => {
    mockAuthRepository = {
      login: vi.fn(),
      register: vi.fn(),
      verifyEmail: vi.fn(),
      resendVerificationCode: vi.fn(),
      startOAuth: vi.fn(),
      exchangeOAuthCode: vi.fn(),
      refreshTokens: vi.fn(),
      logout: vi.fn(),
      requestPasswordReset: vi.fn(),
      confirmPasswordReset: vi.fn(),
      pollOAuth: vi.fn(),
      getCurrentUser: vi.fn(),
    };

    mockTokenRepository = {
      save: vi.fn(),
      get: vi.fn(),
      clear: vi.fn(),
      getDeviceId: vi.fn().mockReturnValue('device-123'),
    };

    loginUseCase = new LoginUseCase(mockAuthRepository, mockTokenRepository);
  });

  it('успешно авторизует пользователя', async () => {
    const loginResult: LoginResult = {
      needsVerification: false,
      session: mockSession,
    };
    vi.mocked(mockAuthRepository.login).mockResolvedValue(loginResult);

    const result = await loginUseCase.execute({
      email: 'test@example.com',
      password: 'validpassword',
    });

    expect(result.needsVerification).toBe(false);
    expect(result.session).toEqual(mockSession);
    expect(mockTokenRepository.save).toHaveBeenCalledWith(mockSession);
    expect(mockAuthRepository.login).toHaveBeenCalledWith(
      'test@example.com',
      'validpassword',
      'device-123'
    );
  });

  it('возвращает needsVerification если требуется подтверждение', async () => {
    const loginResult: LoginResult = { needsVerification: true };
    vi.mocked(mockAuthRepository.login).mockResolvedValue(loginResult);

    const result = await loginUseCase.execute({
      email: 'test@example.com',
      password: 'password12345',
    });

    expect(result.needsVerification).toBe(true);
    expect(result.session).toBeUndefined();
    expect(mockTokenRepository.save).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для некорректного email', async () => {
    await expect(
      loginUseCase.execute({
        email: 'invalid-email',
        password: 'password12345',
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.login).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для пустого email', async () => {
    try {
      await loginUseCase.execute({
        email: '',
        password: 'password12345',
      });
    } catch (e) {
      expect((e as AuthError).code).toBe(AuthErrorCode.ValidationError);
    }
  });

  it('пробрасывает ошибки от репозитория', async () => {
    vi.mocked(mockAuthRepository.login).mockRejectedValue(
      new AuthError(AuthErrorCode.InvalidCredentials, 'Invalid credentials')
    );

    await expect(
      loginUseCase.execute({
        email: 'test@example.com',
        password: 'wrongpassword',
      })
    ).rejects.toThrow(AuthError);
  });
});
