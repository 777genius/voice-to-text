/**
 * Порт для открытия URL в системном браузере
 * Используется для OAuth flow в desktop приложении
 */
export interface IUrlOpener {
  /**
   * Открывает URL в системном браузере
   */
  open(url: string): Promise<void>;
}
