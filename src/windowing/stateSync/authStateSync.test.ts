import { describe, expect, it, vi } from 'vitest';
import { createAuthStateSync } from './authStateSync';
import type { TauriInvoke, TauriListen } from 'state-sync-tauri';

describe('createAuthStateSync', () => {
  it('не вызывает onExternalAuthState если состояние совпадает с локальным', async () => {
    const onExternal = vi.fn();

    const listen: TauriListen = vi.fn(async (_eventName: string, _handler: any) => () => {}) as unknown as TauriListen;
    const invoke: TauriInvoke = vi.fn(async () => ({
      revision: '1',
      data: { is_authenticated: true },
    })) as unknown as TauriInvoke;

    const handle = createAuthStateSync({
      listen,
      invoke,
      getLocalIsAuthenticated: () => true,
      onExternalAuthState: onExternal,
    });

    await handle.start();
    expect(onExternal).toHaveBeenCalledTimes(0);
    handle.stop();
  });

  it('вызывает onExternalAuthState если состояние отличается от локального', async () => {
    const onExternal = vi.fn();

    const listen: TauriListen = vi.fn(async (_eventName: string, _handler: any) => () => {}) as unknown as TauriListen;
    const invoke: TauriInvoke = vi.fn(async () => ({
      revision: '1',
      data: { is_authenticated: false },
    })) as unknown as TauriInvoke;

    const handle = createAuthStateSync({
      listen,
      invoke,
      getLocalIsAuthenticated: () => true,
      onExternalAuthState: onExternal,
    });

    await handle.start();
    expect(onExternal).toHaveBeenCalledTimes(1);
    expect(onExternal).toHaveBeenCalledWith(false);
    handle.stop();
  });
});

