import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { VerifyEmailRequest, VerifyEmailResponse } from '../dto/VerifyEmailDto';
import { validateEmail, validateVerificationCode } from '../../domain/validators';

/**
 * Use case для подтверждения email кодом
 */
export class VerifyEmailUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(request: VerifyEmailRequest): Promise<VerifyEmailResponse> {
    // Валидация на domain уровне
    validateEmail(request.email);
    validateVerificationCode(request.code);

    const deviceId = this.tokenRepository.getDeviceId();
    const session = await this.authRepository.verifyEmail(
      request.email,
      request.code,
      deviceId
    );
    await this.tokenRepository.save(session);
    return { session };
  }
}
