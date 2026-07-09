import { createApp } from 'vue';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useOAuth } from './useOAuth';

const authStoreMock = vi.hoisted(() => ({
  isLoading: false,
  setLoading: vi.fn(),
  setUnauthenticated: vi.fn(),
  setAuthenticated: vi.fn(),
  setError: vi.fn(),
  setStatusError: vi.fn(),
}));

const containerMock = vi.hoisted(() => ({
  deepLinkListener: {
    subscribe: vi.fn(),
  },
  startGoogleOAuthUseCase: {
    execute: vi.fn(),
  },
  exchangeOAuthCodeUseCase: {
    execute: vi.fn(),
  },
  tokenRepository: {
    getDeviceId: vi.fn(),
    save: vi.fn(),
  },
  authRepository: {
    pollOAuth: vi.fn(),
  },
}));

vi.mock('vue-i18n', () => ({
  useI18n: () => ({
    t: (key: string) => key,
  }),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    setFocus: vi.fn().mockResolvedValue(undefined),
  }),
}));

vi.mock('../../store/authStore', () => ({
  useAuthStore: () => authStoreMock,
}));

vi.mock('../../infrastructure/di/authContainer', () => ({
  getAuthContainer: () => containerMock,
}));

vi.mock('./useAuthState', () => ({
  useAuthState: () => ({}),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function mountUseOAuth() {
  let api!: ReturnType<typeof useOAuth>;
  const root = document.createElement('div');
  const app = createApp({
    setup() {
      api = useOAuth();
      return () => null;
    },
  });

  document.body.appendChild(root);
  app.mount(root);

  return {
    api,
    unmount() {
      app.unmount();
      root.remove();
    },
  };
}

describe('useOAuth lifecycle', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    authStoreMock.isLoading = false;
    authStoreMock.setLoading.mockReset();
    authStoreMock.setLoading.mockImplementation(() => {
      authStoreMock.isLoading = true;
    });
    authStoreMock.setUnauthenticated.mockReset();
    authStoreMock.setUnauthenticated.mockImplementation(() => {
      authStoreMock.isLoading = false;
    });
    authStoreMock.setAuthenticated.mockReset();
    authStoreMock.setError.mockReset();
    authStoreMock.setStatusError.mockReset();

    containerMock.deepLinkListener.subscribe.mockReset();
    containerMock.startGoogleOAuthUseCase.execute.mockReset();
    containerMock.exchangeOAuthCodeUseCase.execute.mockReset();
    containerMock.tokenRepository.getDeviceId.mockReset();
    containerMock.tokenRepository.save.mockReset();
    containerMock.authRepository.pollOAuth.mockReset();

    containerMock.startGoogleOAuthUseCase.execute.mockResolvedValue(undefined);
    containerMock.tokenRepository.getDeviceId.mockReturnValue('device-1');
    containerMock.authRepository.pollOAuth.mockResolvedValue({ status: 'pending' });
  });

  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = '';
  });

  it('concurrent startGoogleOAuth не создает второй deep-link subscribe', async () => {
    const pendingSubscribe = deferred<() => void>();
    const unsubscribe = vi.fn();
    containerMock.deepLinkListener.subscribe.mockReturnValue(pendingSubscribe.promise);
    const wrapper = mountUseOAuth();

    const first = wrapper.api.startGoogleOAuth();
    const second = wrapper.api.startGoogleOAuth();
    for (let i = 0; i < 20 && containerMock.deepLinkListener.subscribe.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }

    expect(containerMock.deepLinkListener.subscribe).toHaveBeenCalledTimes(1);
    pendingSubscribe.resolve(unsubscribe);
    await Promise.all([first, second]);

    expect(containerMock.startGoogleOAuthUseCase.execute).toHaveBeenCalledTimes(1);
    wrapper.unmount();
    expect(unsubscribe).toHaveBeenCalledTimes(1);
  });

  it('unmount отписывает deep-link listener, если subscribe resolve пришел поздно', async () => {
    const pendingSubscribe = deferred<() => void>();
    const unsubscribe = vi.fn();
    containerMock.deepLinkListener.subscribe.mockReturnValue(pendingSubscribe.promise);
    const wrapper = mountUseOAuth();

    const start = wrapper.api.startGoogleOAuth();
    for (let i = 0; i < 20 && containerMock.deepLinkListener.subscribe.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }

    wrapper.unmount();
    pendingSubscribe.resolve(unsubscribe);
    await start;

    expect(unsubscribe).toHaveBeenCalledTimes(1);
    expect(containerMock.startGoogleOAuthUseCase.execute).not.toHaveBeenCalled();
  });

  it('cancelOAuth игнорирует поздний deep-link exchange result', async () => {
    let deepLinkCallback: ((url: string) => Promise<void>) | null = null;
    const exchange = deferred<{ session: { id: string } }>();
    const unsubscribe = vi.fn();
    containerMock.deepLinkListener.subscribe.mockImplementation(async (callback) => {
      deepLinkCallback = callback as (url: string) => Promise<void>;
      return unsubscribe;
    });
    containerMock.exchangeOAuthCodeUseCase.execute.mockReturnValue(exchange.promise);
    const wrapper = mountUseOAuth();

    await wrapper.api.startGoogleOAuth();
    expect(deepLinkCallback).not.toBeNull();

    const callbackPromise = deepLinkCallback!('voicetotext://oauth/callback?exchange_code=code-1');
    await flushMicrotasks();
    expect(containerMock.exchangeOAuthCodeUseCase.execute).toHaveBeenCalledWith({
      exchangeCode: 'code-1',
    });

    wrapper.api.cancelOAuth();
    exchange.resolve({ session: { id: 'session-1' } });
    await callbackPromise;

    expect(authStoreMock.setAuthenticated).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('cancelOAuth игнорирует поздний completed poll response', async () => {
    const poll = deferred<{ status: 'completed'; session: { id: string } }>();
    const unsubscribe = vi.fn();
    containerMock.deepLinkListener.subscribe.mockResolvedValue(unsubscribe);
    containerMock.authRepository.pollOAuth.mockReturnValue(poll.promise);
    const wrapper = mountUseOAuth();

    await wrapper.api.startGoogleOAuth();
    vi.advanceTimersByTime(2000);
    await flushMicrotasks();
    expect(containerMock.authRepository.pollOAuth).toHaveBeenCalledWith('device-1');

    wrapper.api.cancelOAuth();
    poll.resolve({ status: 'completed', session: { id: 'session-1' } });
    await flushMicrotasks();

    expect(containerMock.tokenRepository.save).not.toHaveBeenCalled();
    expect(authStoreMock.setAuthenticated).not.toHaveBeenCalled();
    wrapper.unmount();
  });
});
