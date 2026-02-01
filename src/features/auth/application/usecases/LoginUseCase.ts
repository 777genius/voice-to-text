import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { LoginRequest, LoginResponse } from '../dto/LoginDto';
import { validateEmail } from '../../domain/validators';

/**
 * Use case для авторизации пользователя
 */
export class LoginUseCase {
  constructor(
    private readonly authRepository: IAuthRepository,
    private readonly tokenRepository: ITokenRepository
  ) {}

  async execute(request: LoginRequest): Promise<LoginResponse> {
    // Валидация на domain уровне
    validateEmail(request.email);

    const deviceId = this.tokenRepository.getDeviceId();
    const result = await this.authRepository.login(
      request.email,
      request.password,
      deviceId
    );

    if (result.needsVerification) {
      return { needsVerification: true };
    }

    if (result.session) {
      await this.tokenRepository.save(result.session);
      return { needsVerification: false, session: result.session };
    }

    return { needsVerification: true };
  }
}
