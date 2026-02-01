import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';

/**
 * Use case для выхода из системы
 */
export class LogoutUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(): Promise<void> {
    const session = await this.tokenRepository.get();

    if (session?.refreshToken) {
      try {
        await this.authRepository.logout(session.refreshToken);
      } catch {
        // Игнорируем ошибки при logout - всё равно чистим локальные данные
      }
    }

    await this.tokenRepository.clear();
  }
}
