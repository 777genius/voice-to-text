import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { Session } from '../../domain/entities/Session';
import { AuthError, AuthErrorCode } from '../../domain/errors';
import { runRefreshSingleFlight } from '../services/refreshSingleFlight';

/**
 * Use case для обновления токенов
 */
export class RefreshTokensUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(): Promise<Session | null> {
    const sessionBefore = await this.tokenRepository.get();
    if (!sessionBefore?.refreshToken) {
      return null;
    }

    let usedRefreshToken: string | null = null;

    try {
      let refreshedSession: Session | null = null;

      await runRefreshSingleFlight(async () => {
        const currentSession = await this.tokenRepository.get();
        if (!currentSession?.refreshToken) {
          return;
        }

        const deviceId = currentSession.deviceId || this.tokenRepository.getDeviceId();
        usedRefreshToken = currentSession.refreshToken;
        refreshedSession = await this.authRepository.refreshTokens(
          currentSession.refreshToken,
          deviceId
        );
        await this.tokenRepository.save(refreshedSession);
      });

      return refreshedSession ?? (await this.tokenRepository.get());
    } catch (error) {
      if (error instanceof AuthError && error.code === AuthErrorCode.SessionExpired) {
        // В desktop multi-window другой webview может успеть обновить токены,
        // а этот refresh прилетит со "старым" refresh_token и получит 401.
        // В таком случае НЕ очищаем токены — просто используем актуальную сессию из хранилища.
        const current = await this.tokenRepository.get();
        const tokenToCompare = usedRefreshToken ?? sessionBefore.refreshToken;
        if (current?.refreshToken && tokenToCompare && current.refreshToken !== tokenToCompare) {
          return current;
        }
        await this.tokenRepository.clear();
      }
      return null;
    }
  }
}
