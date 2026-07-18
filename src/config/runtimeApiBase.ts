import { isTruthyEnvFlag, resolveApiBaseUrl } from './apiBase';

const isDevelopmentApi =
  import.meta.env.DEV || isTruthyEnvFlag(import.meta.env.TAURI_DEBUG);

export const API_BASE_URL = resolveApiBaseUrl(
  import.meta.env.VITE_API_URL,
  isDevelopmentApi
);
