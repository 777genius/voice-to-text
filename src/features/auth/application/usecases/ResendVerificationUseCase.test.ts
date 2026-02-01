import { describe, it, expect, vi, beforeEach } from 'vitest';
import { ResendVerificationUseCase } from './ResendVerificationUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import { AuthError, AuthErrorCode } from '../../domain/errors';

describe('ResendVerificationUseCase', () => {
  let resendVerificationUseCase: ResendVerificationUseCase;
  let mockAuthRepository: IAuthRepository;

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

    resendVerificationUseCase = new ResendVerificationUseCase(mockAuthRepository);
  });

  it('успешно отправляет код повторно', async () => {
    vi.mocked(mockAuthRepository.resendVerificationCode).mockResolvedValue(undefined);

    await resendVerificationUseCase.execute('test@example.com');

    expect(mockAuthRepository.resendVerificationCode).toHaveBeenCalledWith('test@example.com');
  });

  it('пробрасывает ошибку при rate limit', async () => {
    vi.mocked(mockAuthRepository.resendVerificationCode).mockRejectedValue(
      new AuthError(AuthErrorCode.RateLimitExceeded, 'Too many requests')
    );

    await expect(
      resendVerificationUseCase.execute('test@example.com')
    ).rejects.toThrow(AuthError);
  });

  it('пробрасывает ошибку если пользователь не найден', async () => {
    vi.mocked(mockAuthRepository.resendVerificationCode).mockRejectedValue(
      new AuthError(AuthErrorCode.UserNotFound, 'User not found')
    );

    await expect(
      resendVerificationUseCase.execute('unknown@example.com')
    ).rejects.toThrow(AuthError);
  });

  it('выбрасывает ошибку валидации для некорректного email', async () => {
    await expect(
      resendVerificationUseCase.execute('invalid')
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.resendVerificationCode).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку валидации для пустого email', async () => {
    await expect(
      resendVerificationUseCase.execute('')
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.resendVerificationCode).not.toHaveBeenCalled();
  });
});
