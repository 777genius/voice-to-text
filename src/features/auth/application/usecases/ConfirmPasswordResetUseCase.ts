import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { ConfirmPasswordResetRequest, ConfirmPasswordResetResponse } from '../dto/PasswordResetDto';
import { validateEmail, validateVerificationCode, validatePassword } from '../../domain/validators';

/**
 * Use case для подтверждения сброса пароля
 */
export class ConfirmPasswordResetUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(request: ConfirmPasswordResetRequest): Promise<ConfirmPasswordResetResponse> {
    // Валидация на domain уровне
    validateEmail(request.email);
    validateVerificationCode(request.code);
    validatePassword(request.newPassword);

    const deviceId = this.tokenRepository.getDeviceId();
    const session = await this.authRepository.confirmPasswordReset(
      request.email,
      request.code,
      request.newPassword,
      deviceId
    );
    await this.tokenRepository.save(session);
    return { session };
  }
}
