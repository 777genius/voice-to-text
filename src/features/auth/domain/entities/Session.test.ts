import { describe, it, expect } from 'vitest';
import {
  createSession,
  isAccessTokenExpired,
  isRefreshTokenExpired,
  canRefreshSession,
} from './Session';

describe('Session', () => {
  const now = Date.now();

  describe('createSession', () => {
    it('создаёт иммутабельную сессию', () => {
      const session = createSession({
        accessToken: 'access-token',
        refreshToken: 'refresh-token',
        accessExpiresAt: new Date(now + 3600000),
        refreshExpiresAt: new Date(now + 86400000),
      });

      expect(session.accessToken).toBe('access-token');
      expect(session.refreshToken).toBe('refresh-token');
      expect(Object.isFrozen(session)).toBe(true);
    });

    it('работает без refreshToken', () => {
      const session = createSession({
        accessToken: 'access-token',
        accessExpiresAt: new Date(now + 3600000),
      });

      expect(session.accessToken).toBe('access-token');
      expect(session.refreshToken).toBeUndefined();
    });

    it('включает user если передан', () => {
      const session = createSession({
        accessToken: 'token',
        accessExpiresAt: new Date(now + 3600000),
        user: { id: '1', email: 'test@test.com', emailVerified: true },
      });

      expect(session.user?.id).toBe('1');
      expect(session.user?.email).toBe('test@test.com');
    });
  });

  describe('isAccessTokenExpired', () => {
    it('возвращает false для валидного токена', () => {
      const session = createSession({
        accessToken: 'token',
        accessExpiresAt: new Date(now + 60000), // +1 минута
      });

      expect(isAccessTokenExpired(session)).toBe(false);
    });

    it('возвращает true для истёкшего токена', () => {
      const session = createSession({
        accessToken: 'token',
        accessExpiresAt: new Date(now - 1000), // -1 секунда
      });

      expect(isAccessTokenExpired(session)).toBe(true);
    });

    it('учитывает буфер в 30 секунд по умолчанию', () => {
      const session = createSession({
        accessToken: 'token',
        accessExpiresAt: new Date(now + 20000), // +20 секунд (меньше буфера)
      });

      expect(isAccessTokenExpired(session)).toBe(true);
    });

    it('принимает кастомный буфер', () => {
      const session = createSession({
        accessToken: 'token',
        accessExpiresAt: new Date(now + 20000),
      });

      expect(isAccessTokenExpired(session, 10000)).toBe(false); // буфер 10 сек
    });
  });

  describe('isRefreshTokenExpired', () => {
    it('возвращает false если refreshExpiresAt не задан', () => {
      const session = createSession({
        accessToken: 'token',
        refreshToken: 'refresh',
        accessExpiresAt: new Date(now + 60000),
      });

      expect(isRefreshTokenExpired(session)).toBe(false);
    });

    it('возвращает false для валидного refresh токена', () => {
      const session = createSession({
        accessToken: 'token',
        refreshToken: 'refresh',
        accessExpiresAt: new Date(now + 60000),
        refreshExpiresAt: new Date(now + 86400000),
      });

      expect(isRefreshTokenExpired(session)).toBe(false);
    });

    it('возвращает true для истёкшего refresh токена', () => {
      const session = createSession({
        accessToken: 'token',
        refreshToken: 'refresh',
        accessExpiresAt: new Date(now - 1000),
        refreshExpiresAt: new Date(now - 1000),
      });

      expect(isRefreshTokenExpired(session)).toBe(true);
    });
  });

  describe('canRefreshSession', () => {
    it('возвращает true если есть валидный refresh токен', () => {
      const session = createSession({
        accessToken: 'token',
        refreshToken: 'refresh',
        accessExpiresAt: new Date(now - 1000),
        refreshExpiresAt: new Date(now + 86400000),
      });

      expect(canRefreshSession(session)).toBe(true);
    });

    it('возвращает false если нет refresh токена', () => {
      const session = createSession({
        accessToken: 'token',
        accessExpiresAt: new Date(now + 60000),
      });

      expect(canRefreshSession(session)).toBe(false);
    });

    it('возвращает false если refresh токен истёк', () => {
      const session = createSession({
        accessToken: 'token',
        refreshToken: 'refresh',
        accessExpiresAt: new Date(now - 1000),
        refreshExpiresAt: new Date(now - 1000),
      });

      expect(canRefreshSession(session)).toBe(false);
    });
  });
});
