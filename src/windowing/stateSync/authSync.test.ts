import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createTauriRevisionSync } from 'state-sync-tauri';
import { CMD_GET_AUTH_STATE_SNAPSHOT, STATE_SYNC_INVALIDATION_EVENT } from './tauri';
import type { SnapshotEnvelope } from 'state-sync';

const invokeMock = vi.fn();
const listenMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: any[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: any[]) => listenMock(...args),
}));

describe('auth-state sync', () => {
  let invalidationHandler: ((event: any) => void) | null = null;

  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
    invalidationHandler = null;

    listenMock.mockImplementation(async (_event: string, handler: any) => {
      invalidationHandler = handler;
      return () => {};
    });
  });

  it('при изменении is_authenticated вызывает apply callback', async () => {
    const apply = vi.fn();

    invokeMock.mockResolvedValue({
      revision: '1',
      data: { is_authenticated: false },
    });

    const handle = createTauriRevisionSync<{ is_authenticated: boolean }>({
      topic: 'auth-state',
      listen: listenMock,
      invoke: invokeMock,
      eventName: STATE_SYNC_INVALIDATION_EVENT,
      commandName: CMD_GET_AUTH_STATE_SNAPSHOT,
      applier: {
        apply(snapshot: SnapshotEnvelope<{ is_authenticated: boolean }>) {
          apply(snapshot.data, snapshot.revision);
        },
      },
    });

    await handle.start();
    expect(apply).toHaveBeenCalledWith({ is_authenticated: false }, '1');

    // Меняем состояние — apply вызывается с новыми данными
    invokeMock.mockResolvedValue({
      revision: '2',
      data: { is_authenticated: true },
    });

    invalidationHandler!({ payload: { topic: 'auth-state', revision: '2', sourceId: undefined, timestampMs: Date.now() } });
    await vi.waitFor(() => expect(apply).toHaveBeenCalledTimes(2));
    expect(apply).toHaveBeenLastCalledWith({ is_authenticated: true }, '2');

    handle.stop();
  });

  it('одинаковое значение revision — engine пропускает pull (revision gate)', async () => {
    const apply = vi.fn();

    invokeMock.mockResolvedValue({
      revision: '5',
      data: { is_authenticated: true },
    });

    const handle = createTauriRevisionSync<{ is_authenticated: boolean }>({
      topic: 'auth-state',
      listen: listenMock,
      invoke: invokeMock,
      eventName: STATE_SYNC_INVALIDATION_EVENT,
      commandName: CMD_GET_AUTH_STATE_SNAPSHOT,
      applier: {
        apply(snapshot: SnapshotEnvelope<{ is_authenticated: boolean }>) {
          apply(snapshot.data, snapshot.revision);
        },
      },
    });

    await handle.start();
    expect(apply).toHaveBeenCalledTimes(1);

    // Invalidation со старой ревизией — engine не делает pull
    invalidationHandler!({ payload: { topic: 'auth-state', revision: '3', sourceId: undefined, timestampMs: Date.now() } });
    await new Promise((r) => setTimeout(r, 10));
    expect(apply).toHaveBeenCalledTimes(1);

    handle.stop();
  });
});
