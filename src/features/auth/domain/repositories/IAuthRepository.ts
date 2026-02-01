import type { Session } from '../entities/Session';
import type { User } from '../entities/User';

/**
 * Результат логина - либо нужна верификация, либо сессия
 */
export interface LoginResult {
  needsVerification: boolean;
  session?: Session;
}

/**
 * Интерфейс репозитория для auth операций
 * Абстрагирует работу с API от бизнес-логики
 */
export interface IAuthRepository {
  /**
   * Авторизация по email/password
   */
  login(email: string, password: string, deviceId: string): Promise<LoginResult>;

  /**
   * Регистрация нового пользователя
   * Всегда требует последующую верификацию email
   */
  register(email: string, password: string, deviceId: string): Promise<void>;

  /**
   * Подтверждение email 6-значным кодом
   */
  verifyEmail(email: string, code: string, deviceId: string): Promise<Session>;

  /**
   * Повторная отправка кода подтверждения
   */
  resendVerificationCode(email: string): Promise<void>;

  /**
   * Запуск OAuth flow - возвращает URL для открытия в браузере
   */
  startOAuth(deviceId: string, redirectUri: string): Promise<string>;

  /**
   * Обмен OAuth кода на токены
   */
  exchangeOAuthCode(exchangeCode: string, deviceId: string): Promise<Session>;

  /**
   * Polling для получения OAuth токенов по device_id
   * Возвращает сессию если OAuth завершён, иначе null
   */
  pollOAuth(deviceId: string): Promise<{ status: string; session?: Session }>;

  /**
   * Обновление токенов по refresh token
   */
  refreshTokens(refreshToken: string, deviceId: string): Promise<Session>;

  /**
   * Выход - инвалидация refresh token на сервере
   */
  logout(refreshToken: string): Promise<void>;

  /**
   * Запрос сброса пароля - отправляет код на email
   */
  requestPasswordReset(email: string): Promise<void>;

  /**
   * Подтверждение сброса пароля и установка нового
   */
  confirmPasswordReset(
    email: string,
    code: string,
    newPassword: string,
    deviceId: string
  ): Promise<Session>;

  /**
   * Получение данных текущего пользователя по access token
   */
  getCurrentUser(accessToken: string): Promise<User>;
}
