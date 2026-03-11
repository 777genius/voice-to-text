import { describe, it, expect, vi, beforeEach } from 'vitest';
import { RegisterUseCase } from './RegisterUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import { AuthError, AuthErrorCode } from '../../domain/errors';

describe('RegisterUseCase', () => {
  let registerUseCase: RegisterUseCase;
  let mockAuthRepository: IAuthRepository;
  let mockTokenRepository: ITokenRepository;

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

    registerUseCase = new RegisterUseCase(mockAuthRepository, mockTokenRepository);
  });

  it('успешно регистрирует пользователя', async () => {
    vi.mocked(mockAuthRepository.register).mockResolvedValue({
      needsVerification: true,
      nextStep: 'verify_email',
    });

    const result = await registerUseCase.execute({
      email: 'test@example.com',
      password: 'password12345',
    });

    expect(result.needsVerification).toBe(true);
    expect(result.nextStep).toBe('verify_email');
    expect(mockAuthRepository.register).toHaveBeenCalledWith(
      'test@example.com',
      'password12345',
      'device-123'
    );
  });

  it('возвращает password_setup для oauth-only пользователя', async () => {
    vi.mocked(mockAuthRepository.register).mockResolvedValue({
      needsVerification: false,
      nextStep: 'password_setup',
    });

    const result = await registerUseCase.execute({
      email: 'oauth@example.com',
      password: 'password12345',
    });

    expect(result.needsVerification).toBe(false);
    expect(result.nextStep).toBe('password_setup');
  });

  it('выбрасывает ошибку для некорректного email', async () => {
    await expect(
      registerUseCase.execute({
        email: 'invalid',
        password: 'password12345',
      })
    ).rejects.toThrow(AuthError);

    expect(mockAuthRepository.register).not.toHaveBeenCalled();
  });

  it('выбрасывает ошибку для короткого пароля', async () => {
    try {
      await registerUseCase.execute({
        email: 'test@example.com',
        password: 'short',
      });
    } catch (e) {
      expect((e as AuthError).code).toBe(AuthErrorCode.PasswordWeak);
    }

    expect(mockAuthRepository.register).not.toHaveBeenCalled();
  });

  it('пароль должен быть минимум 12 символов', async () => {
    // 11 символов - должен упасть
    await expect(
      registerUseCase.execute({
        email: 'test@example.com',
        password: '12345678901',
      })
    ).rejects.toThrow();

    // 12 символов - должен пройти
    vi.mocked(mockAuthRepository.register).mockResolvedValue({
      needsVerification: true,
      nextStep: 'verify_email',
    });
    await expect(
      registerUseCase.execute({
        email: 'test@example.com',
        password: '123456789012',
      })
    ).resolves.toBeDefined();
  });

  it('пробрасывает ошибки от репозитория', async () => {
    vi.mocked(mockAuthRepository.register).mockRejectedValue(
      new AuthError(AuthErrorCode.AccountAlreadyExists, 'Email already exists')
    );

    await expect(
      registerUseCase.execute({
        email: 'existing@example.com',
        password: 'password12345',
      })
    ).rejects.toThrow(AuthError);
  });
});
