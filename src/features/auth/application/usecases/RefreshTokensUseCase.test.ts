import { describe, it, expect, vi, beforeEach } from 'vitest';
import { RefreshTokensUseCase } from './RefreshTokensUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import { AuthError, AuthErrorCode } from '../../domain/errors';

describe('RefreshTokensUseCase', () => {
  let refreshTokensUseCase: RefreshTokensUseCase;
  let mockAuthRepository: IAuthRepository;
  let mockTokenRepository: ITokenRepository;

  const now = Date.now();

  const currentSession: Session = {
    accessToken: 'old-access-token',
    refreshToken: 'refresh-token',
    accessExpiresAt: new Date(now - 1000), // истёк
    refreshExpiresAt: new Date(now + 86400000),
  };

  const newSession: Session = {
    accessToken: 'new-access-token',
    refreshToken: 'new-refresh-token',
    accessExpiresAt: new Date(now + 3600000),
    refreshExpiresAt: new Date(now + 86400000),
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

    refreshTokensUseCase = new RefreshTokensUseCase(mockAuthRepository, mockTokenRepository);
  });

  it('успешно обновляет токены', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(currentSession);
    vi.mocked(mockAuthRepository.refreshTokens).mockResolvedValue(newSession);

    const result = await refreshTokensUseCase.execute();

    expect(result).toEqual(newSession);
    expect(mockAuthRepository.refreshTokens).toHaveBeenCalledWith('refresh-token', 'device-123');
    expect(mockTokenRepository.save).toHaveBeenCalledWith(newSession);
  });

  it('возвращает null если нет сохранённой сессии', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(null);

    const result = await refreshTokensUseCase.execute();

    expect(result).toBeNull();
    expect(mockAuthRepository.refreshTokens).not.toHaveBeenCalled();
  });

  it('возвращает null если нет refresh токена', async () => {
    const sessionWithoutRefresh: Session = {
      accessToken: 'access-token',
      accessExpiresAt: new Date(now + 3600000),
    };
    vi.mocked(mockTokenRepository.get).mockResolvedValue(sessionWithoutRefresh);

    const result = await refreshTokensUseCase.execute();

    expect(result).toBeNull();
    expect(mockAuthRepository.refreshTokens).not.toHaveBeenCalled();
  });

  it('очищает токены при ошибке SessionExpired', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(currentSession);
    vi.mocked(mockAuthRepository.refreshTokens).mockRejectedValue(
      new AuthError(AuthErrorCode.SessionExpired, 'Session expired')
    );

    const result = await refreshTokensUseCase.execute();

    expect(result).toBeNull();
    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });

  it('не очищает токены при SessionExpired если другая вкладка/окно уже обновило refresh токен', async () => {
    const rotatedSession: Session = {
      accessToken: 'rotated-access-token',
      refreshToken: 'rotated-refresh-token',
      accessExpiresAt: new Date(now + 3600000),
      refreshExpiresAt: new Date(now + 86400000),
    };

    // execute() вызывает get() минимум 3 раза:
    // 1) sessionBefore
    // 2) currentSession внутри single-flight
    // 3) current в catch при SessionExpired
    vi.mocked(mockTokenRepository.get)
      .mockResolvedValueOnce(currentSession)
      .mockResolvedValueOnce(currentSession)
      .mockResolvedValueOnce(rotatedSession);

    vi.mocked(mockAuthRepository.refreshTokens).mockRejectedValue(
      new AuthError(AuthErrorCode.SessionExpired, 'Session expired')
    );

    const result = await refreshTokensUseCase.execute();

    expect(result).toEqual(rotatedSession);
    expect(mockTokenRepository.clear).not.toHaveBeenCalled();
  });

  it('возвращает null при других ошибках без очистки токенов', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(currentSession);
    vi.mocked(mockAuthRepository.refreshTokens).mockRejectedValue(
      new Error('Network error')
    );

    const result = await refreshTokensUseCase.execute();

    expect(result).toBeNull();
    expect(mockTokenRepository.clear).not.toHaveBeenCalled();
  });
});
