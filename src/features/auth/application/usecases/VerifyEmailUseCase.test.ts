import { describe, it, expect, vi, beforeEach } from 'vitest';
import { VerifyEmailUseCase } from './VerifyEmailUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import { AuthError, AuthErrorCode } from '../../domain/errors';

describe('VerifyEmailUseCase', () => {
  let verifyEmailUseCase: VerifyEmailUseCase;
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

    verifyEmailUseCase = new VerifyEmailUseCase(mockAuthRepository, mockTokenRepository);
  });

  it('успешно верифицирует email', async () => {
    vi.mocked(mockAuthRepository.verifyEmail).mockResolvedValue(mockSession);

    const result = await verifyEmailUseCase.execute({
      email: 'test@example.com',
      code: '123456',
    });

    expect(result.session).toEqual(mockSession);
    expect(mockTokenRepository.save).toHaveBeenCalledWith(mockSession);
    expect(mockAuthRepository.verifyEmail).toHaveBeenCalledWith(
      'test@example.com',
      '123456',
      'device-123'
    );
  });

  it('выбрасывает ошибку для некорректного email', async () => {
    await expect(
      verifyEmailUseCase.execute({
        email: 'invalid',
        code: '123456',
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.verifyEmail).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для некорректного кода', async () => {
    await expect(
      verifyEmailUseCase.execute({
        email: 'test@example.com',
        code: '12345', // 5 цифр вместо 6
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.verifyEmail).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для кода с буквами', async () => {
    await expect(
      verifyEmailUseCase.execute({
        email: 'test@example.com',
        code: 'abcdef',
      })
    ).rejects.toThrow();
  });

  it('пробрасывает ошибку невалидного кода', async () => {
    vi.mocked(mockAuthRepository.verifyEmail).mockRejectedValue(
      new AuthError(AuthErrorCode.CodeInvalid, 'Invalid code')
    );

    await expect(
      verifyEmailUseCase.execute({
        email: 'test@example.com',
        code: '123456',
      })
    ).rejects.toThrow(AuthError);
  });

  it('пробрасывает ошибку истёкшего кода', async () => {
    vi.mocked(mockAuthRepository.verifyEmail).mockRejectedValue(
      new AuthError(AuthErrorCode.CodeExpired, 'Code expired')
    );

    try {
      await verifyEmailUseCase.execute({
        email: 'test@example.com',
        code: '123456',
      });
    } catch (e) {
      expect((e as AuthError).code).toBe(AuthErrorCode.CodeExpired);
    }
  });
});
