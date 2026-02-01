import { HttpClient } from "./http-client";

/**
 * Composable для получения HTTP клиента.
 * Создаёт инстанс с baseURL из runtimeConfig.
 * Безопасен для SSR — useRuntimeConfig() вызывается в контексте composable.
 */
export function useApiClient(): HttpClient {
  const config = useRuntimeConfig();
  const baseUrl = (config.public.apiBaseUrl as string) || "http://localhost:8080";

  return new HttpClient({ baseURL: baseUrl });
}

export { HttpClient } from "./http-client";
