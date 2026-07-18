export const DEVELOPMENT_API_BASE_URL = 'http://localhost:8080';
export const PRODUCTION_API_BASE_URL = 'https://api.voicetext.site';

function normalizeHostname(hostname: string): string {
  return hostname.toLowerCase().replace(/^\[|\]$/g, '');
}

function isLoopbackHostname(hostname: string): boolean {
  const normalized = normalizeHostname(hostname);

  if (
    normalized === 'localhost' ||
    normalized.endsWith('.localhost') ||
    normalized === '::1'
  ) {
    return true;
  }

  const ipv4Parts = normalized.split('.');
  return (
    ipv4Parts.length === 4 &&
    ipv4Parts[0] === '127' &&
    ipv4Parts.every((part) => /^\d+$/.test(part) && Number(part) <= 255)
  );
}

export function isTruthyEnvFlag(value: unknown): boolean {
  if (typeof value === 'boolean') return value;
  if (typeof value !== 'string') return false;

  return ['1', 'true', 'yes', 'on'].includes(value.trim().toLowerCase());
}

export function resolveApiBaseUrl(
  configuredUrl: string | undefined,
  isDevelopment: boolean
): string {
  const fallback = isDevelopment ? DEVELOPMENT_API_BASE_URL : PRODUCTION_API_BASE_URL;
  const candidate = configuredUrl?.trim() || fallback;

  let parsed: URL;
  try {
    parsed = new URL(candidate);
  } catch {
    return fallback;
  }

  if (
    parsed.username ||
    parsed.password ||
    parsed.search ||
    parsed.hash ||
    !['http:', 'https:'].includes(parsed.protocol)
  ) {
    return fallback;
  }

  if (
    !isDevelopment &&
    (parsed.protocol !== 'https:' || isLoopbackHostname(parsed.hostname))
  ) {
    return PRODUCTION_API_BASE_URL;
  }

  return parsed.toString().replace(/\/$/, '');
}
