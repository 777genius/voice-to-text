import { beforeEach, describe, expect, it } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import { createSession } from '../domain/entities/Session';
import { useAuthStore } from './authStore';

describe('authStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
  });

  it('clears stale session data when marked unauthenticated', () => {
    const store = useAuthStore();
    const session = createSession({
      accessToken: 'access-token',
      refreshToken: 'refresh-token',
      accessExpiresAt: new Date(Date.now() + 60_000),
      user: { id: '1', email: 'user@example.com', emailVerified: true },
    });

    store.setNeedsVerification('pending@example.com');
    store.setAuthenticated(session);
    store.setError('previous error');

    store.setUnauthenticated();

    expect(store.status).toBe('unauthenticated');
    expect(store.session).toBeNull();
    expect(store.accessToken).toBeUndefined();
    expect(store.userEmail).toBeNull();
    expect(store.pendingEmail).toBeNull();
    expect(store.error).toBeNull();
    expect(store.errorCode).toBeNull();
  });
});
