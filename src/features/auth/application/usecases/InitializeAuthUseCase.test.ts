import { describe, it, expect, vi, beforeEach } from 'vitest';
import { InitializeAuthUseCase } from './InitializeAuthUseCase';
import { RefreshTokensUseCase } from './RefreshTokensUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import type { User } from '../../domain/entities/User';

describe('InitializeAuthUseCase', () => {
  let initializeAuthUseCase: InitializeAuthUseCase;
  let refreshTokensUseCase: RefreshTokensUseCase;
  let mockAuthRepository: IAuthRepository;
  let mockTokenRepository: ITokenRepository;

  const now = Date.now();

  const validSession: Session = {
    accessToken: 'access-token',
    refreshToken: 'refresh-token',
    accessExpiresAt: new Date(now + 3600000), // +1 час
    refreshExpiresAt: new Date(now + 86400000), // +1 день
  };

  const expiredAccessSession: Session = {
    accessToken: 'expired-access-token',
    refreshToken: 'refresh-token',
    accessExpiresAt: new Date(now - 1000), // истёк
    refreshExpiresAt: new Date(now + 86400000), // refresh ещё валиден
  };

  const fullyExpiredSession: Session = {
    accessToken: 'expired-access-token',
    refreshToken: 'expired-refresh-token',
    accessExpiresAt: new Date(now - 3600000),
    refreshExpiresAt: new Date(now - 1000), // refresh тоже истёк
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
    initializeAuthUseCase = new InitializeAuthUseCase(mockTokenRepository, mockAuthRepository, refreshTokensUseCase);
  });

  it('возвращает unauthenticated если нет сохранённой сессии', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(null);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(false);
    expect(result.session).toBeNull();
    expect(result.sessionExpired).toBe(false);
  });

  it('возвращает authenticated если access token валиден', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(validSession);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(true);
    expect(result.session).toEqual(validSession);
    expect(result.sessionExpired).toBe(false);
  });

  it('обновляет токены если access истёк но refresh валиден', async () => {
    const refreshedSession: Session = {
      accessToken: 'new-access-token',
      refreshToken: 'new-refresh-token',
      accessExpiresAt: new Date(now + 3600000),
      refreshExpiresAt: new Date(now + 86400000),
    };

    vi.mocked(mockTokenRepository.get).mockResolvedValue(expiredAccessSession);
    vi.mocked(mockAuthRepository.refreshTokens).mockResolvedValue(refreshedSession);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(true);
    expect(result.session).toEqual(refreshedSession);
    expect(mockTokenRepository.save).toHaveBeenCalledWith(refreshedSession);
  });

  it('возвращает sessionExpired и очищает storage если refresh тоже истёк', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(fullyExpiredSession);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(false);
    expect(result.session).toBeNull();
    expect(result.sessionExpired).toBe(true);
    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });

  it('не разлогинивает пользователя если refresh не удался (например, временная ошибка сети)', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(expiredAccessSession);
    vi.mocked(mockAuthRepository.refreshTokens).mockRejectedValue(new Error('Refresh failed'));

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(true);
    expect(result.session).toEqual(expiredAccessSession);
    expect(result.sessionExpired).toBe(false);
    expect(mockTokenRepository.clear).not.toHaveBeenCalled();
  });

  it('не пытается обновить и очищает storage если нет refresh токена', async () => {
    const sessionWithoutRefresh: Session = {
      accessToken: 'access-token',
      accessExpiresAt: new Date(now - 1000), // истёк
      // без refreshToken
    };

    vi.mocked(mockTokenRepository.get).mockResolvedValue(sessionWithoutRefresh);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(false);
    expect(result.sessionExpired).toBe(true);
    expect(mockAuthRepository.refreshTokens).not.toHaveBeenCalled();
    expect(mockTokenRepository.clear).toHaveBeenCalled();
  });

  it('загружает user если он отсутствует в сессии', async () => {
    const mockUser: User = {
      id: 'user-123',
      email: 'test@example.com',
      emailVerified: true,
    };

    vi.mocked(mockTokenRepository.get).mockResolvedValue(validSession);
    vi.mocked(mockAuthRepository.getCurrentUser).mockResolvedValue(mockUser);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(true);
    expect(result.session?.user).toEqual(mockUser);
    expect(mockAuthRepository.getCurrentUser).toHaveBeenCalledWith('access-token');
    expect(mockTokenRepository.save).toHaveBeenCalled();
  });

  it('не загружает user если он уже есть в сессии', async () => {
    const mockUser: User = {
      id: 'user-123',
      email: 'test@example.com',
      emailVerified: true,
    };

    const sessionWithUser: Session = {
      ...validSession,
      user: mockUser,
    };

    vi.mocked(mockTokenRepository.get).mockResolvedValue(sessionWithUser);

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(true);
    expect(result.session?.user).toEqual(mockUser);
    expect(mockAuthRepository.getCurrentUser).not.toHaveBeenCalled();
  });

  it('возвращает сессию без user если загрузка user не удалась', async () => {
    vi.mocked(mockTokenRepository.get).mockResolvedValue(validSession);
    vi.mocked(mockAuthRepository.getCurrentUser).mockRejectedValue(new Error('Network error'));

    const result = await initializeAuthUseCase.execute();

    expect(result.isAuthenticated).toBe(true);
    expect(result.session).toEqual(validSession);
    expect(result.session?.user).toBeUndefined();
  });
});
