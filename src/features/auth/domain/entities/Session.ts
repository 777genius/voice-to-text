import type { User } from './User';

/**
 * Value object для хранения сессии пользователя
 * Содержит токены и опционально данные пользователя
 */
export interface Session {
  readonly accessToken: string;
  readonly refreshToken?: string;
  readonly accessExpiresAt: Date;
  readonly refreshExpiresAt?: Date;
  /**
   * ID устройства (client_id), к которому привязан refresh token на сервере.
   *
   * Важно для desktop multi-window: localStorage может быть изолирован между webview,
   * поэтому держим deviceId прямо в сессии и используем его для refresh.
   */
  readonly deviceId?: string;
  readonly user?: User;
}

export function createSession(data: {
  accessToken: string;
  refreshToken?: string;
  accessExpiresAt: Date;
  refreshExpiresAt?: Date;
  deviceId?: string;
  user?: User;
}): Session {
  return Object.freeze({
    accessToken: data.accessToken,
    refreshToken: data.refreshToken,
    accessExpiresAt: data.accessExpiresAt,
    refreshExpiresAt: data.refreshExpiresAt,
    deviceId: data.deviceId,
    user: data.user,
  });
}

/**
 * Проверяет истёк ли access token
 * Добавляем буфер в 30 секунд для предотвращения race conditions
 */
export function isAccessTokenExpired(session: Session, bufferMs = 30000): boolean {
  return session.accessExpiresAt.getTime() - bufferMs <= Date.now();
}

/**
 * Проверяет истёк ли refresh token
 */
export function isRefreshTokenExpired(session: Session): boolean {
  if (!session.refreshExpiresAt) return false;
  return session.refreshExpiresAt.getTime() <= Date.now();
}

/**
 * Проверяет можно ли обновить сессию
 */
export function canRefreshSession(session: Session): boolean {
  return !!session.refreshToken && !isRefreshTokenExpired(session);
}
