import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { ExchangeOAuthCodeRequest, ExchangeOAuthCodeResponse } from '../dto/OAuthDto';

/**
 * Use case для обмена OAuth кода на токены
 */
export class ExchangeOAuthCodeUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(request: ExchangeOAuthCodeRequest): Promise<ExchangeOAuthCodeResponse> {
    const deviceId = this.tokenRepository.getDeviceId();
    const session = await this.authRepository.exchangeOAuthCode(
      request.exchangeCode,
      deviceId
    );
    await this.tokenRepository.save(session);
    return { session };
  }
}
