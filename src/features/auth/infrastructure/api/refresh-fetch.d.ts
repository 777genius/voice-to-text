// TypeScript определения для refresh-fetch
declare module 'refresh-fetch' {
  export interface ResponseError extends Error {
    name: 'ResponseError';
    status: number;
    response: Response;
    body: unknown;
  }

  export interface FetchJSONResponse<T = unknown> {
    response: Response;
    body: T;
  }

  export interface RefreshFetchConfiguration {
    /** Функция для обновления токена */
    refreshToken: () => Promise<void>;
    /** Определяет нужно ли обновлять токен по ошибке */
    shouldRefreshToken: (error: ResponseError) => boolean;
    /** Базовая fetch функция */
    fetch: (url: RequestInfo | URL, options?: RequestInit) => Promise<Response>;
  }

  /** Создаёт fetch с автоматическим обновлением токена */
  export function configureRefreshFetch(
    configuration: RefreshFetchConfiguration
  ): (url: RequestInfo | URL, options?: RequestInit) => Promise<Response>;

  /** Fetch с автоматическим парсингом JSON и проверкой статуса */
  export function fetchJSON<T = unknown>(
    url: RequestInfo | URL,
    options?: RequestInit
  ): Promise<FetchJSONResponse<T>>;
}
