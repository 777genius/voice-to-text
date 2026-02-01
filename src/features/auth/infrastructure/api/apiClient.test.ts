import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';

import type { Session } from '../../domain/entities/Session';
import { createSession } from '../../domain/entities/Session';

const oldSession: Session = createSession({
  accessToken: 'old-access',
  refreshToken: 'old-refresh',
  accessExpiresAt: new Date(Date.now() - 60_000),
  refreshExpiresAt: new Date(Date.now() + 86_400_000),
  deviceId: 'device-123',
});

const newSession: Session = createSession({
  accessToken: 'new-access',
  refreshToken: 'new-refresh',
  accessExpiresAt: new Date(Date.now() + 60_000),
  refreshExpiresAt: new Date(Date.now() + 86_400_000),
  deviceId: 'device-123',
});

// Мокаем TokenRepository singleton, чтобы контролировать сценарий multi-window.
const tokenRepoMock = {
  get: vi.fn<() => Promise<Session | null>>(),
  save: vi.fn<(session: Session) => Promise<void>>(),
  clear: vi.fn<() => Promise<void>>(),
  getDeviceId: vi.fn<() => string>(),
};

vi.mock('../repositories/TokenRepository', () => ({
  getTokenRepository: () => tokenRepoMock,
}));

function makeJsonResponse(params: {
  ok: boolean;
  status: number;
  json: unknown;
}): Response {
  return {
    ok: params.ok,
    status: params.status,
    async json() {
      return params.json;
    },
  } as unknown as Response;
}

describe('apiClient (refresh-fetch integration)', () => {
  beforeEach(() => {
    tokenRepoMock.get.mockReset();
    tokenRepoMock.save.mockReset();
    tokenRepoMock.clear.mockReset();
    tokenRepoMock.getDeviceId.mockReset();

    tokenRepoMock.getDeviceId.mockReturnValue('device-123');

    // Последовательность вызовов tokenRepo.get() в этом тесте:
    // 1) baseFetch первого запроса → oldSession (старый access, старый refresh)
    // 2) refreshToken(): session → oldSession (старый refresh)
    // 3) refreshToken(): currentSession в 401 ветке → newSession (другое окно уже обновило)
    // 4) baseFetch ретрая запроса → newSession (новый access)
    tokenRepoMock.get
      .mockResolvedValueOnce(oldSession)
      .mockResolvedValueOnce(oldSession)
      .mockResolvedValueOnce(newSession)
      .mockResolvedValueOnce(newSession);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
  });

  it('не очищает токены, если refresh получил 401 со старым refresh_token, но сессия уже обновлена другим окном', async () => {
    // Импортируем после установки моков (важно для vi.mock).
    const { apiRequest } = await import('./apiClient');

    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);

      // 1) Первый запрос: отдаём 401, чтобы спровоцировать refresh.
      if (url.includes('/api/v1/some/protected')) {
        const authHeader = (init?.headers as Record<string, string> | undefined)?.Authorization;
        if (authHeader === 'Bearer old-access') {
          return makeJsonResponse({
            ok: false,
            status: 401,
            json: { error: { code: 'AUTH_EXPIRED', message: 'Token expired' } },
          });
        }

        // 3) Ретрай: успех с новым access token.
        return makeJsonResponse({
          ok: true,
          status: 200,
          json: { data: { ok: true } },
        });
      }

      // 2) Refresh: 401 (токен уже ротирован другим окном)
      if (url.includes('/api/v1/auth/refresh')) {
        return makeJsonResponse({
          ok: false,
          status: 401,
          json: { error: { code: 'AUTH_INVALID', message: 'Session not found' } },
        });
      }

      throw new Error(`Unexpected fetch url: ${url}`);
    });

    vi.stubGlobal('fetch', fetchMock);

    const result = await apiRequest<{ ok: boolean }>('/api/v1/some/protected', { method: 'GET' });

    expect(result).toEqual({ ok: true });
    expect(tokenRepoMock.clear).not.toHaveBeenCalled();

    // Проверяем что был и refresh, и ретрай запроса.
    expect(fetchMock).toHaveBeenCalled();
    const calledUrls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(calledUrls.filter((u) => u.includes('/api/v1/auth/refresh')).length).toBe(1);
    expect(calledUrls.filter((u) => u.includes('/api/v1/some/protected')).length).toBe(2);
  });
});

