import { describe, expect, it, vi } from 'vitest';
import { createTauriRevisionSync } from 'state-sync-tauri';
import type { SnapshotEnvelope } from 'state-sync';
import { STATE_SYNC_INVALIDATION_EVENT } from './tauri';
import { CMD_GET_APP_CONFIG_SNAPSHOT } from './tauri';

type AppConfigData = {
  auto_copy_to_clipboard: boolean;
};

function createInMemoryInvalidationBus() {
  const handlers = new Set<(event: { payload: any }) => void>();

  const listen = async (_eventName: string, handler: (event: { payload: any }) => void) => {
    handlers.add(handler);
    return () => {
      handlers.delete(handler);
    };
  };

  const emit = (payload: any) => {
    for (const handler of Array.from(handlers)) {
      handler({ payload });
    }
  };

  return { listen, emit };
}

describe('state-sync (contract): two windows converge via invalidation → snapshot pull', () => {
  it('late-join + out-of-order invalidation do not break convergence', async () => {
    const bus = createInMemoryInvalidationBus();

    let current: SnapshotEnvelope<AppConfigData> = {
      revision: '0' as any,
      data: { auto_copy_to_clipboard: false },
    };

    const invoke = vi.fn(async (commandName: string) => {
      expect(commandName).toBe(CMD_GET_APP_CONFIG_SNAPSHOT);
      return current;
    });

    const appliedA: SnapshotEnvelope<AppConfigData>[] = [];
    const appliedB: SnapshotEnvelope<AppConfigData>[] = [];

    const handleA = createTauriRevisionSync<AppConfigData>({
      topic: 'app-config',
      listen: bus.listen as any,
      invoke: invoke as any,
      eventName: STATE_SYNC_INVALIDATION_EVENT,
      commandName: CMD_GET_APP_CONFIG_SNAPSHOT,
      applier: {
        apply(snapshot) {
          appliedA.push(snapshot);
        },
      },
    });

    const handleB = createTauriRevisionSync<AppConfigData>({
      topic: 'app-config',
      listen: bus.listen as any,
      invoke: invoke as any,
      eventName: STATE_SYNC_INVALIDATION_EVENT,
      commandName: CMD_GET_APP_CONFIG_SNAPSHOT,
      applier: {
        apply(snapshot) {
          appliedB.push(snapshot);
        },
      },
    });

    // Window A starts first (B is a late-join)
    await handleA.start();
    expect(appliedA).toHaveLength(1);
    expect(appliedA[0]?.revision).toBe('0');

    current = {
      revision: '1' as any,
      data: { auto_copy_to_clipboard: true },
    };

    // B starts later and must still converge on latest snapshot (late-join safe)
    await handleB.start();
    expect(appliedB).toHaveLength(1);
    expect(appliedB[0]?.revision).toBe('1');

    // Out-of-order invalidation must be ignored (should not re-apply older state)
    bus.emit({
      topic: 'app-config',
      revision: '0',
      sourceId: 'test',
      timestampMs: Date.now(),
    });

    await new Promise((r) => setTimeout(r, 10));
    const lastA = appliedA[appliedA.length - 1];
    const lastB = appliedB[appliedB.length - 1];
    expect(lastA?.revision).toBe('0' /* A still has last snapshot it applied */);
    expect(lastB?.revision).toBe('1');

    // New invalidation must converge both windows to rev=2
    current = {
      revision: '2' as any,
      data: { auto_copy_to_clipboard: false },
    };

    bus.emit({
      topic: 'app-config',
      revision: '2',
      sourceId: 'test',
      timestampMs: Date.now(),
    });

    await vi.waitFor(() => {
      const lastA = appliedA[appliedA.length - 1];
      const lastB = appliedB[appliedB.length - 1];
      expect(lastA?.revision).toBe('2');
      expect(lastB?.revision).toBe('2');
    });

    handleA.stop();
    handleB.stop();
  });

  it('stop() prevents further applies (quiescence)', async () => {
    const bus = createInMemoryInvalidationBus();

    let current: SnapshotEnvelope<AppConfigData> = {
      revision: '0' as any,
      data: { auto_copy_to_clipboard: false },
    };

    const invoke = vi.fn(async () => current);
    const applied: SnapshotEnvelope<AppConfigData>[] = [];

    const handle = createTauriRevisionSync<AppConfigData>({
      topic: 'app-config',
      listen: bus.listen as any,
      invoke: invoke as any,
      eventName: STATE_SYNC_INVALIDATION_EVENT,
      commandName: CMD_GET_APP_CONFIG_SNAPSHOT,
      applier: {
        apply(snapshot) {
          applied.push(snapshot);
        },
      },
    });

    await handle.start();
    expect(applied).toHaveLength(1);

    handle.stop();

    current = { revision: '1' as any, data: { auto_copy_to_clipboard: true } };
    bus.emit({ topic: 'app-config', revision: '1', sourceId: 'test', timestampMs: Date.now() });

    await new Promise((r) => setTimeout(r, 10));
    expect(applied).toHaveLength(1);
  });
});

