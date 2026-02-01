import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { RegisterRequest, RegisterResponse } from '../dto/RegisterDto';
import { validateEmail, validatePassword } from '../../domain/validators';

/**
 * Use case для регистрации нового пользователя
 */
export class RegisterUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(request: RegisterRequest): Promise<RegisterResponse> {
    // Валидация на domain уровне
    validateEmail(request.email);
    validatePassword(request.password);

    const deviceId = this.tokenRepository.getDeviceId();
    await this.authRepository.register(
      request.email,
      request.password,
      deviceId
    );
    return { needsVerification: true };
  }
}
