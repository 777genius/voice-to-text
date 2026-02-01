/**
 * Callback для обработки deep link события
 */
export type DeepLinkCallback = (url: string) => void;

/**
 * Функция отписки от событий
 */
export type UnsubscribeFn = () => void;

/**
 * Порт для прослушивания deep link событий
 * Используется для получения OAuth callback в desktop приложении
 */
export interface IDeepLinkListener {
  /**
   * Подписка на deep link события
   * Возвращает функцию отписки
   */
  subscribe(callback: DeepLinkCallback): Promise<UnsubscribeFn>;
}
