import { describe, it, expect, vi, beforeEach } from 'vitest';
import { LogoutUseCase } from './LogoutUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';

describe('LogoutUseCase', () => {
  let logoutUseCase: LogoutUseCase;
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

    logoutUseCase = new LogoutUseCase(mockAuthRepository, mockTokenRepository);
  });

  it('успешно выходит из системы и очищает токены', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(mockSession);
    vi.mocked(mockAuthRepository.logout).mockResolvedValue(undefined);

    await logoutUseCase.execute();

    expect(mockAuthRepository.logout).toHaveBeenCalledWith('refresh-token');
    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });

  it('очищает токены даже если нет сохранённой сессии', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(null);

    await logoutUseCase.execute();

    expect(mockAuthRepository.logout).not.toHaveBeenCalled();
    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });

  it('очищает токены даже если нет refresh токена', async () => {
    const sessionWithoutRefresh: Session = {
      accessToken: 'access-token',
      accessExpiresAt: new Date(Date.now() + 3600000),
    };
    vi.mocked(mockTokenRepository.get).mockResolvedValue(sessionWithoutRefresh);

    await logoutUseCase.execute();

    expect(mockAuthRepository.logout).not.toHaveBeenCalled();
    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });

  it('очищает токены даже если серверный logout завершился ошибкой', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(mockSession);
    vi.mocked(mockAuthRepository.logout).mockRejectedValue(new Error('Network error'));

    await logoutUseCase.execute();

    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });
});
