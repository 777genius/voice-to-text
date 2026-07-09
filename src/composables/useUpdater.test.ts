import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';
import { useUpdater } from './useUpdater';
import { useUpdateStore } from '../stores/update';
import {
  EVENT_UPDATE_AVAILABLE,
  EVENT_UPDATE_DOWNLOAD_PROGRESS,
  EVENT_UPDATE_DOWNLOAD_STARTED,
  EVENT_UPDATE_INSTALLING,
} from '@/types';

const listenMock = vi.hoisted(() => vi.fn());
const invokeMock = vi.hoisted(() => vi.fn());
const loadUpdateNotesFromChangelogMock = vi.hoisted(() => vi.fn());

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/app', () => ({
  getVersion: vi.fn().mockResolvedValue('0.0.0-test'),
}));

vi.mock('@/utils/tauri', () => ({
  isTauriAvailable: () => true,
}));

vi.mock('@/utils/changelog', () => ({
  loadUpdateNotesFromChangelog: (...args: unknown[]) => loadUpdateNotesFromChangelogMock(...args),
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

describe('useUpdater listener lifecycle', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    listenMock.mockReset();
    invokeMock.mockReset();
    loadUpdateNotesFromChangelogMock.mockReset();
    loadUpdateNotesFromChangelogMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    useUpdater().cleanupUpdateListener();
  });

  it('cleanupUpdateListener отписывает listener, если setup resolve пришел после cleanup', async () => {
    const pendingListen = deferred<() => void>();
    const unlisten = vi.fn();
    listenMock.mockReturnValueOnce(pendingListen.promise);

    const updater = useUpdater();
    const setup = updater.setupUpdateListener();
    for (let i = 0; i < 20 && listenMock.mock.calls.length === 0; i++) {
      await flushMicrotasks();
    }
    expect(listenMock).toHaveBeenCalledTimes(1);

    updater.cleanupUpdateListener();
    pendingListen.resolve(unlisten);
    await setup;

    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(listenMock).toHaveBeenCalledTimes(1);
  });

  it('concurrent setupUpdateListener не создает дубли listeners', async () => {
    listenMock.mockResolvedValue(vi.fn());
    const updater = useUpdater();

    await Promise.all([
      updater.setupUpdateListener(),
      updater.setupUpdateListener(),
    ]);

    expect(listenMock.mock.calls.map((call) => call[0])).toEqual([
      EVENT_UPDATE_AVAILABLE,
      EVENT_UPDATE_DOWNLOAD_STARTED,
      EVENT_UPDATE_DOWNLOAD_PROGRESS,
      EVENT_UPDATE_INSTALLING,
    ]);
  });

  it('не откатывает update version, если notes для старого события resolve пришел позже', async () => {
    const notesA = deferred<string>();
    const notesB = deferred<string>();
    const handlers = new Map<string, (event: { payload: { version: string; body?: string } }) => void>();
    listenMock.mockImplementation(async (eventName, handler) => {
      handlers.set(String(eventName), handler as (event: { payload: { version: string; body?: string } }) => void);
      return vi.fn();
    });
    loadUpdateNotesFromChangelogMock.mockImplementation((version: string) => {
      if (version === '1.0.0') return notesA.promise;
      if (version === '2.0.0') return notesB.promise;
      return undefined;
    });

    const updater = useUpdater();
    const store = useUpdateStore();
    await updater.setupUpdateListener();

    handlers.get(EVENT_UPDATE_AVAILABLE)!({ payload: { version: '1.0.0', body: 'A' } });
    handlers.get(EVENT_UPDATE_AVAILABLE)!({ payload: { version: '2.0.0', body: 'B' } });
    await flushMicrotasks();
    expect(store.availableVersion).toBe('2.0.0');

    notesA.resolve('notes A');
    await flushMicrotasks();
    expect(store.availableVersion).toBe('2.0.0');
    expect(store.releaseNotes).toBeNull();

    notesB.resolve('notes B');
    await flushMicrotasks();
    expect(store.availableVersion).toBe('2.0.0');
    expect(store.releaseNotes).toBe('notes B');
  });
});
