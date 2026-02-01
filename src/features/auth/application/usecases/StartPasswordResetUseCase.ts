import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { StartPasswordResetRequest } from '../dto/PasswordResetDto';
import { validateEmail } from '../../domain/validators';

/**
 * Use case для запроса сброса пароля
 */
export class StartPasswordResetUseCase {
  constructor(private readonly authRepository: IAuthRepository) {}

  async execute(request: StartPasswordResetRequest): Promise<void> {
    validateEmail(request.email);
    await this.authRepository.requestPasswordReset(request.email);
  }
}
