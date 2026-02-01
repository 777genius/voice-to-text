import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { IUrlOpener } from '../ports/IUrlOpener';
import type { StartOAuthResponse } from '../dto/OAuthDto';

const OAUTH_REDIRECT_URI = import.meta.env.VITE_OAUTH_REDIRECT_URI || 'voicetotext://oauth/callback';

/**
 * Use case для запуска Google OAuth flow
 */
export class StartGoogleOAuthUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository,
    private readonly urlOpener: IUrlOpener
  ) {}

  async execute(): Promise<StartOAuthResponse> {
    const deviceId = this.tokenRepository.getDeviceId();
    const authUrl = await this.authRepository.startOAuth(deviceId, OAUTH_REDIRECT_URI);
    await this.urlOpener.open(authUrl);
    return { authUrl };
  }
}
