import type { Session } from '../entities/Session';

/**
 * Данные пользователя для хранения
 */
export interface StoredUser {
  id: string;
  email: string;
  emailVerified: boolean;
}

/**
 * Данные сессии для хранения
 * Даты в ISO строках для сериализации
 */
export interface StoredSession {
  accessToken: string;
  refreshToken?: string;
  accessExpiresAt: string;
  refreshExpiresAt?: string;
  deviceId?: string;
  user?: StoredUser;
}

/**
 * Интерфейс репозитория для работы с токенами
 * Абстрагирует хранилище токенов от бизнес-логики
 */
export interface ITokenRepository {
  /**
   * Сохранение сессии в хранилище
   */
  save(session: Session): Promise<void>;

  /**
   * Получение сохранённой сессии
   * Возвращает null если сессии нет
   */
  get(): Promise<Session | null>;

  /**
   * Удаление сессии из хранилища
   */
  clear(): Promise<void>;

  /**
   * Получение уникального ID устройства
   * Создаёт новый если не существует
   */
  getDeviceId(): string;
}
