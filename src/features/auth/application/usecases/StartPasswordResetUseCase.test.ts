import { describe, it, expect, vi, beforeEach } from 'vitest';
import { StartPasswordResetUseCase } from './StartPasswordResetUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import { AuthError, AuthErrorCode } from '../../domain/errors';

// Тесты для валидации email

describe('StartPasswordResetUseCase', () => {
  let startPasswordResetUseCase: StartPasswordResetUseCase;
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

    startPasswordResetUseCase = new StartPasswordResetUseCase(mockAuthRepository);
  });

  it('успешно запрашивает сброс пароля', async () => {
    vi.mocked(mockAuthRepository.requestPasswordReset).mockResolvedValue(undefined);

    await startPasswordResetUseCase.execute({ email: 'test@example.com' });

    expect(mockAuthRepository.requestPasswordReset).toHaveBeenCalledWith('test@example.com');
  });

  it('пробрасывает ошибку если пользователь не найден', async () => {
    vi.mocked(mockAuthRepository.requestPasswordReset).mockRejectedValue(
      new AuthError(AuthErrorCode.UserNotFound, 'User not found')
    );

    await expect(
      startPasswordResetUseCase.execute({ email: 'unknown@example.com' })
    ).rejects.toThrow(AuthError);
  });

  it('пробрасывает ошибку при сетевой ошибке', async () => {
    vi.mocked(mockAuthRepository.requestPasswordReset).mockRejectedValue(
      new Error('Network error')
    );

    await expect(
      startPasswordResetUseCase.execute({ email: 'test@example.com' })
    ).rejects.toThrow('Network error');
  });

  it('выбрасывает ошибку валидации для некорректного email', async () => {
    await expect(
      startPasswordResetUseCase.execute({ email: 'invalid' })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.requestPasswordReset).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку валидации для пустого email', async () => {
    await expect(
      startPasswordResetUseCase.execute({ email: '' })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.requestPasswordReset).not.toHaveBeenCalled();
  });
});
