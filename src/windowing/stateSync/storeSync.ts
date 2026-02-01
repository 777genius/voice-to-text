/**
 * Общий helper для Pinia store'ов: wiring state-sync поверх Tauri.
 *
 * Цель: убрать копипасту createTauriRevisionSync(...) по всем сторам и
 * стандартизировать:
 * - eventName (STATE_SYNC_INVALIDATION_EVENT)
 * - логирование/ошибки
 * - единый контракт args (обычно undefined)
 *
 * Важно: tests могут mock'ать `@tauri-apps/api/*` — этот модуль их использует напрямую.
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { SnapshotEnvelope, SnapshotApplier, RevisionSyncHandle } from 'state-sync';
import { createTauriRevisionSync } from 'state-sync-tauri';

import { STATE_SYNC_INVALIDATION_EVENT } from './tauri';

export interface CreateStoreTauriTopicSyncOptions<T> {
  /** state-sync topic */
  topic: string;

  /** Tauri command name that returns { revision, data } */
  commandName: string;

  /** Optional args passed to invoke(commandName, args) */
  args?: Record<string, unknown>;

  /** For logs, e.g. 'appConfig' / 'sttConfig' */
  label: string;

  applier: SnapshotApplier<T>;

  onError?: (ctx: { phase: string; error: unknown }) => void;
}

export function createStoreTauriTopicSync<T>(
  options: CreateStoreTauriTopicSyncOptions<T>,
): RevisionSyncHandle {
  const { topic, commandName, args, label, applier, onError } = options;

  return createTauriRevisionSync<T>({
    topic,
    listen,
    invoke,
    eventName: STATE_SYNC_INVALIDATION_EVENT,
    commandName,
    args,
    applier: {
      apply(snapshot: SnapshotEnvelope<T>) {
        return applier.apply(snapshot);
      },
    },
    onError: (ctx) => {
      console.error(`[${label}] sync error (${ctx.phase}):`, ctx.error);
      onError?.({ phase: ctx.phase, error: ctx.error });
    },
  });
}

