/**
 * Синхронизация auth-state между окнами (Tauri).
 *
 * Здесь только wiring протокола + защита от лишних побочных эффектов.
 * Логику "что делать при внешней авторизации" передаём снаружи.
 */

import type { RevisionSyncHandle, SnapshotEnvelope } from 'state-sync';
import type { TauriInvoke, TauriListen } from 'state-sync-tauri';

import { createTauriRevisionSync } from 'state-sync-tauri';

import { STATE_SYNC_INVALIDATION_EVENT, CMD_GET_AUTH_STATE_SNAPSHOT } from './tauri';
import { TOPIC_AUTH_STATE } from './topics';
import type { AuthStateSnapshotData } from './contracts';

export interface CreateAuthStateSyncOptions {
  listen: TauriListen;
  invoke: TauriInvoke;

  /** Текущее состояние авторизации в этом окне (обычно из store). */
  getLocalIsAuthenticated: () => boolean;

  /**
   * Вызывается когда из другого окна прилетело состояние, отличающееся от локального.
   * Сюда обычно прокидывается `runExternalAuthSync(() => auth.initialize({ silent: true }))`.
   */
  onExternalAuthState: (nextIsAuthenticated: boolean) => void;

  onError?: (ctx: { phase: string; error: unknown }) => void;
}

export function createAuthStateSync(options: CreateAuthStateSyncOptions): RevisionSyncHandle {
  const { listen, invoke, getLocalIsAuthenticated, onExternalAuthState, onError } = options;

  return createTauriRevisionSync<AuthStateSnapshotData>({
    topic: TOPIC_AUTH_STATE,
    listen,
    invoke,
    eventName: STATE_SYNC_INVALIDATION_EVENT,
    commandName: CMD_GET_AUTH_STATE_SNAPSHOT,
    applier: {
      apply(snapshot: SnapshotEnvelope<AuthStateSnapshotData>) {
        const next = snapshot.data.is_authenticated;
        const local = getLocalIsAuthenticated();
        if (next === local) return;
        onExternalAuthState(next);
      },
    },
    onError: (ctx) => {
      console.error(`[auth-state] sync error (${ctx.phase}):`, ctx.error);
      onError?.({ phase: ctx.phase, error: ctx.error });
    },
  });
}

