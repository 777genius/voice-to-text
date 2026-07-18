import { describe, expect, it } from 'vitest';
import {
  DEVELOPMENT_API_BASE_URL,
  PRODUCTION_API_BASE_URL,
  isTruthyEnvFlag,
  resolveApiBaseUrl,
} from './apiBase';

describe('resolveApiBaseUrl', () => {
  it('uses localhost by default in development', () => {
    expect(resolveApiBaseUrl(undefined, true)).toBe(DEVELOPMENT_API_BASE_URL);
  });

  it('preserves an explicit loopback URL in development', () => {
    expect(resolveApiBaseUrl('http://127.0.0.1:9090/', true)).toBe(
      'http://127.0.0.1:9090'
    );
  });

  it.each([
    'http://localhost:8080',
    'https://dev.localhost:8080',
    'http://127.42.0.1:8080',
    'http://[::1]:8080',
    'http://api.voicetext.site',
    'not-a-url',
  ])('never embeds unsafe API URL in production: %s', (configuredUrl) => {
    expect(resolveApiBaseUrl(configuredUrl, false)).toBe(PRODUCTION_API_BASE_URL);
  });

  it('preserves a production HTTPS override for staging', () => {
    expect(resolveApiBaseUrl('https://staging-api.voicetext.site/', false)).toBe(
      'https://staging-api.voicetext.site'
    );
  });

  it('rejects credentials, query parameters and fragments in an API base URL', () => {
    expect(resolveApiBaseUrl('https://user:pass@example.com', false)).toBe(
      PRODUCTION_API_BASE_URL
    );
    expect(resolveApiBaseUrl('https://example.com?target=other', false)).toBe(
      PRODUCTION_API_BASE_URL
    );
    expect(resolveApiBaseUrl('https://example.com/#other', false)).toBe(
      PRODUCTION_API_BASE_URL
    );
  });
});

describe('isTruthyEnvFlag', () => {
  it.each(['1', 'true', 'TRUE', 'yes', 'on'])(
    'recognizes enabled debug flag %s',
    (value) => {
      expect(isTruthyEnvFlag(value)).toBe(true);
    }
  );

  it.each([undefined, '', '0', 'false', 'off'])(
    'rejects disabled flag %s',
    (value) => {
      expect(isTruthyEnvFlag(value)).toBe(false);
    }
  );
});
