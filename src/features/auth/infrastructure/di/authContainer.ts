import { AuthApiClient } from '../api/authApiClient';
import { AuthRepository } from '../repositories/AuthRepository';
import { getTokenRepository, resetTokenRepository } from '../repositories/TokenRepository';
import { TauriUrlOpener } from '../adapters/TauriUrlOpener';
import { TauriDeepLinkListener } from '../adapters/TauriDeepLinkListener';
import { LoginUseCase } from '../../application/usecases/LoginUseCase';
import { RegisterUseCase } from '../../application/usecases/RegisterUseCase';
import { VerifyEmailUseCase } from '../../application/usecases/VerifyEmailUseCase';
import { ResendVerificationUseCase } from '../../application/usecases/ResendVerificationUseCase';
import { RefreshTokensUseCase } from '../../application/usecases/RefreshTokensUseCase';
import { LogoutUseCase } from '../../application/usecases/LogoutUseCase';
import { InitializeAuthUseCase } from '../../application/usecases/InitializeAuthUseCase';
import { StartPasswordResetUseCase } from '../../application/usecases/StartPasswordResetUseCase';
import { ConfirmPasswordResetUseCase } from '../../application/usecases/ConfirmPasswordResetUseCase';
import { StartGoogleOAuthUseCase } from '../../application/usecases/StartGoogleOAuthUseCase';
import { ExchangeOAuthCodeUseCase } from '../../application/usecases/ExchangeOAuthCodeUseCase';
import type { IAuthRepository } from '../../domain/repositories/IAuthRepository';
import type { ITokenRepository } from '../../domain/repositories/ITokenRepository';
import type { IUrlOpener } from '../../application/ports/IUrlOpener';
import type { IDeepLinkListener } from '../../application/ports/IDeepLinkListener';

/**
 * Контейнер зависимостей для Auth feature
 * Собирает все компоненты и их зависимости
 */
export interface AuthContainer {
  // Репозитории
  authRepository: IAuthRepository;
  tokenRepository: ITokenRepository;

  // Адаптеры
  urlOpener: IUrlOpener;
  deepLinkListener: IDeepLinkListener;

  // Use Cases
  loginUseCase: LoginUseCase;
  registerUseCase: RegisterUseCase;
  verifyEmailUseCase: VerifyEmailUseCase;
  resendVerificationUseCase: ResendVerificationUseCase;
  refreshTokensUseCase: RefreshTokensUseCase;
  logoutUseCase: LogoutUseCase;
  initializeAuthUseCase: InitializeAuthUseCase;
  startPasswordResetUseCase: StartPasswordResetUseCase;
  confirmPasswordResetUseCase: ConfirmPasswordResetUseCase;
  startGoogleOAuthUseCase: StartGoogleOAuthUseCase;
  exchangeOAuthCodeUseCase: ExchangeOAuthCodeUseCase;
}

let container: AuthContainer | null = null;

/**
 * Создаёт и возвращает singleton контейнер зависимостей
 */
export function getAuthContainer(): AuthContainer {
  if (container) {
    return container;
  }

  // Infrastructure - используем singleton для TokenRepository
  const apiClient = new AuthApiClient();
  const authRepository = new AuthRepository(apiClient);
  const tokenRepository = getTokenRepository();
  const urlOpener = new TauriUrlOpener();
  const deepLinkListener = new TauriDeepLinkListener();

  // Use Cases
  const loginUseCase = new LoginUseCase(authRepository, tokenRepository);
  const registerUseCase = new RegisterUseCase(authRepository, tokenRepository);
  const verifyEmailUseCase = new VerifyEmailUseCase(authRepository, tokenRepository);
  const resendVerificationUseCase = new ResendVerificationUseCase(authRepository);
  const refreshTokensUseCase = new RefreshTokensUseCase(authRepository, tokenRepository);
  const logoutUseCase = new LogoutUseCase(authRepository, tokenRepository);
  const initializeAuthUseCase = new InitializeAuthUseCase(tokenRepository, authRepository, refreshTokensUseCase);
  const startPasswordResetUseCase = new StartPasswordResetUseCase(authRepository);
  const confirmPasswordResetUseCase = new ConfirmPasswordResetUseCase(authRepository, tokenRepository);
  const startGoogleOAuthUseCase = new StartGoogleOAuthUseCase(authRepository, tokenRepository, urlOpener);
  const exchangeOAuthCodeUseCase = new ExchangeOAuthCodeUseCase(authRepository, tokenRepository);

  container = {
    authRepository,
    tokenRepository,
    urlOpener,
    deepLinkListener,
    loginUseCase,
    registerUseCase,
    verifyEmailUseCase,
    resendVerificationUseCase,
    refreshTokensUseCase,
    logoutUseCase,
    initializeAuthUseCase,
    startPasswordResetUseCase,
    confirmPasswordResetUseCase,
    startGoogleOAuthUseCase,
    exchangeOAuthCodeUseCase,
  };

  return container;
}

/**
 * Сброс контейнера (для тестов)
 */
export function resetAuthContainer(): void {
  container = null;
  resetTokenRepository();
}
