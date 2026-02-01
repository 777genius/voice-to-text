import { describe, it, expect, vi, beforeEach } from 'vitest';
import { ConfirmPasswordResetUseCase } from './ConfirmPasswordResetUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import { AuthError, AuthErrorCode } from '../../domain/errors';

describe('ConfirmPasswordResetUseCase', () => {
  let confirmPasswordResetUseCase: ConfirmPasswordResetUseCase;
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

    confirmPasswordResetUseCase = new ConfirmPasswordResetUseCase(
      mockAuthRepository,
      mockTokenRepository
    );
  });

  it('успешно сбрасывает пароль и сохраняет сессию', async () => {
    vi.mocked(mockAuthRepository.confirmPasswordReset).mockResolvedValue(mockSession);

    const result = await confirmPasswordResetUseCase.execute({
      email: 'test@example.com',
      code: '123456',
      newPassword: 'NewPassword123!',
    });

    expect(result.session).toEqual(mockSession);
    expect(mockTokenRepository.save).toHaveBeenCalledWith(mockSession);
    expect(mockAuthRepository.confirmPasswordReset).toHaveBeenCalledWith(
      'test@example.com',
      '123456',
      'NewPassword123!',
      'device-123'
    );
  });

  it('выбрасывает ошибку для некорректного email', async () => {
    await expect(
      confirmPasswordResetUseCase.execute({
        email: 'invalid',
        code: '123456',
        newPassword: 'NewPassword123!',
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.confirmPasswordReset).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для некорректного кода', async () => {
    await expect(
      confirmPasswordResetUseCase.execute({
        email: 'test@example.com',
        code: '12345', // 5 цифр
        newPassword: 'NewPassword123!',
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.confirmPasswordReset).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для слабого пароля', async () => {
    await expect(
      confirmPasswordResetUseCase.execute({
        email: 'test@example.com',
        code: '123456',
        newPassword: '123', // слишком короткий
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.confirmPasswordReset).not.toHaveBeenCalled();
  });

  it('пробрасывает ошибку невалидного кода от сервера', async () => {
    vi.mocked(mockAuthRepository.confirmPasswordReset).mockRejectedValue(
      new AuthError(AuthErrorCode.CodeInvalid, 'Invalid code')
    );

    await expect(
      confirmPasswordResetUseCase.execute({
        email: 'test@example.com',
        code: '123456',
        newPassword: 'NewPassword123!',
      })
    ).rejects.toThrow(AuthError);
  });

  it('пробрасывает ошибку истёкшего кода от сервера', async () => {
    vi.mocked(mockAuthRepository.confirmPasswordReset).mockRejectedValue(
      new AuthError(AuthErrorCode.CodeExpired, 'Code expired')
    );

    try {
      await confirmPasswordResetUseCase.execute({
        email: 'test@example.com',
        code: '123456',
        newPassword: 'NewPassword123!',
      });
    } catch (e) {
      expect((e as AuthError).code).toBe(AuthErrorCode.CodeExpired);
    }
  });
});
