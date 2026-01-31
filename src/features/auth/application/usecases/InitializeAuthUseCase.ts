import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { Session } from '../../domain/entities/Session';
import { isAccessTokenExpired, canRefreshSession, createSession } from '../../domain/entities/Session';
import { RefreshTokensUseCase } from './RefreshTokensUseCase';

export interface InitializeAuthResult {
  isAuthenticated: boolean;
  session: Session | null;
  sessionExpired: boolean;
}

/**
 * Use case для инициализации авторизации при старте приложения
 */
export class InitializeAuthUseCase {
  constructor(
    private readonly tokenRepository: ITokenRepository,
    private readonly authRepository: IAuthRepository,
    private readonly refreshTokensUseCase: RefreshTokensUseCase
  ) {}

  async execute(): Promise<InitializeAuthResult> {
    let session = await this.tokenRepository.get();

    if (!session) {
      return {
        isAuthenticated: false,
        session: null,
        sessionExpired: false,
      };
    }

    // Если access token ещё валиден - используем текущую сессию
    if (!isAccessTokenExpired(session)) {
      // Загружаем user если его нет в сохранённой сессии
      session = await this.enrichSessionWithUser(session);
      return {
        isAuthenticated: true,
        session,
        sessionExpired: false,
      };
    }

    // Если можно обновить - пробуем refresh
    if (canRefreshSession(session)) {
      let refreshedSession = await this.refreshTokensUseCase.execute();
      if (refreshedSession) {
        // После refresh тоже подгружаем user если его нет
        refreshedSession = await this.enrichSessionWithUser(refreshedSession);
        return {
          isAuthenticated: true,
          session: refreshedSession,
          sessionExpired: false,
        };
      }

      // Refresh мог не удаться по временным причинам (сеть/5xx).
      // Важно: не очищаем storage и не считаем сессию "истёкшей", если токены всё ещё на месте.
      const stillStoredSession = await this.tokenRepository.get();
      if (stillStoredSession) {
        return {
          isAuthenticated: true,
          session: stillStoredSession,
          sessionExpired: false,
        };
      }
    }

    // Сессия истекла и обновить не удалось - очищаем storage
    await this.tokenRepository.clear();
    return {
      isAuthenticated: false,
      session: null,
      sessionExpired: true,
    };
  }

  /**
   * Дозагружает данные пользователя если их нет в сессии
   */
  private async enrichSessionWithUser(session: Session): Promise<Session> {
    if (session.user) {
      return session;
    }

    try {
      const user = await this.authRepository.getCurrentUser(session.accessToken);

      // Создаём новую сессию с добавленным user и сохраняем
      const enrichedSession = createSession({
        accessToken: session.accessToken,
        refreshToken: session.refreshToken,
        accessExpiresAt: session.accessExpiresAt,
        refreshExpiresAt: session.refreshExpiresAt,
        user,
      });

      await this.tokenRepository.save(enrichedSession);
      return enrichedSession;
    } catch (error) {
      // Если не удалось загрузить user - не критично, возвращаем сессию как есть
      console.warn('Не удалось загрузить данные пользователя:', error);
      return session;
    }
  }
}
