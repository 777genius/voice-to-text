import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import { validateEmail } from '../../domain/validators';

/**
 * Use case для повторной отправки кода подтверждения
 */
export class ResendVerificationUseCase {
  constructor(private readonly authRepository: IAuthRepository) {}

  async execute(email: string): Promise<void> {
    validateEmail(email);
    await this.authRepository.resendVerificationCode(email);
  }
}
