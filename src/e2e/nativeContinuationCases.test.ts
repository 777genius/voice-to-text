import { expect, it, vi } from 'vitest';
const fixture = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: fixture.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@/stores/appConfig', () => ({ useAppConfigStore: () => ({ startSync: async () => {}, refresh: async () => {} }) }));
import { runNativeContinuationCase, serviceTerminal, validE42PhysicalEvidence, coordinatorReadyForHandoff } from './nativeContinuationCases';
import type { Pinia } from 'pinia';
it('seal-close dispatches native close while Continue is pending and retains timing/write assertions', async () => {
  let presses = 0; let closed = false; let readsAfterClose = 0; let report: any;
  fixture.invoke.mockImplementation(async (command, args) => {
    if (command === 'native_e2e_hotkey' && args.action === 'press') presses++;
    if (command === 'native_e2e_close_recording') { expect(presses).toBe(3); closed = true; }
    if (command === 'native_e2e_state') {
      if (closed) readsAfterClose++;
      return { preparedCaptureTokenCount: 0, fixture: {
        activeCaptures: closed || presses === 2 ? 0 : 1,
        activeProviders: readsAfterClose >= 2 ? 0 : 1,
        captureStarts: presses >= 3 ? 2 : 1, providerAudioChunks: 1,
        controlResults: [ { operation: 'pause', delivered: true, result: { decision: 'accepted' } },
          { operation: 'continue', delivered: readsAfterClose >= 2, result: { decision: 'accepted' } } ],
        firstBWrites: readsAfterClose >= 2 ? [{}] : [] } };
    }
    if (command === 'native_e2e_finish') report = args.report;
  });
  await runNativeContinuationCase({} as Pinia, 'seal-close');
  expect(closed).toBe(true);
  expect(report.errors).toEqual([]);
  expect(report.passed).toBe(true);
  expect(report.micReleasedBeforeAccepted).toBe(true);
  expect(report.firstBWrites).toBe(1);
  expect(report.cleanup).toBe(true);
  expect(fixture.invoke.mock.calls.some(([name]) => name === 'stop_recording')).toBe(false);
});

for (const selected of ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle']) {
  it(`${selected} never dispatches B stop on Accepted without first native B write`, async () => {
    let reads = 0; let clock = 0; let report: any;
    const now = vi.spyOn(performance, 'now').mockImplementation(() => clock);
    fixture.invoke.mockClear();
    fixture.invoke.mockImplementation(async (command, args) => {
      if (command === 'native_e2e_delay') clock += args.durationMs;
      if (command === 'native_e2e_state') {
        reads++;
        const a = reads === 1;
        return { logicalProviderRunId: 9, captureEpisode: { runId: a ? 1 : 2, generation: a ? 1 : 2 },
          preparedCaptureTokenCount: 0, fixture: { activeCaptures: reads === 2 ? 0 : 1,
            captureStarts: a ? 1 : 2, firstBWrites: [], controlResults: [
              { operation: 'pause', delivered: true, result: { decision: 'accepted' } },
              { operation: 'continue', delivered: true, result: { decision: 'accepted' } } ],
            providerMarkers: [{ captureRunId: a ? 1 : 2, captureFenceGeneration: a ? 1 : 2,
              captureGeneration: a ? 1 : 2, providerSessionId: 7, count: 1 }] } };
      }
      if (command === 'native_e2e_finish') report = args.report;
    });
    try {
      await runNativeContinuationCase({} as Pinia, selected);
      expect(report.passed).toBe(false);
      expect(report.errors).toEqual(['Error: Actual continued B first write missing']);
      expect(fixture.invoke.mock.calls.some(([name]) => name === 'stop_recording' || name === 'native_e2e_close_recording')).toBe(false);
      const actions = fixture.invoke.mock.calls.filter(([name]) => name === 'native_e2e_hotkey');
      expect(actions.length).toBe(selected === 'after-write-hold' ? 3 : 6);
    } finally { now.mockRestore(); }
  });
}

it('E63 service gate rejects absent A terminal even when fake resource counters are zero', () => {
  const a = { logicalProviderRunId: 9 } as any;
  const b = { coordinatorTrace: [{ sequence: 3 }] } as any;
  for (const completedReport of [null, { run_id: 2 }]) {
    expect(Boolean(serviceTerminal(a, b, { fixture: { activeCaptures: 0, activeProviders: 0 },
      afterWriteService: { owner: 9, status: 'Idle', logicalProviderRunId: 0, pausedContinuation: null,
        coordinatorIdle: true, pendingStart: false, processingJobs: 0, continuationPending: false,
        terminal: [{ runId: 9, sequence: 6, outcome: 'Some(FinalizeCommitted)', error: null }], completedReport } } as any))).toBe(false);
  }
});

it('E42 physical evidence requires full chord, exact frozen handles and watcher rearm/stop', () => {
  const event = (kind: string, gesture: number, observation: string, result: string) =>
    ({ kind, watcherFinished: kind === 'watcher' ? true : null, sample: observation === 'NotRead' ? null : (gesture - 1) * 2 + (observation === 'Up' ? 1 : 0), handle: { gesture, watcher: gesture }, observation, result });
  const state: any = { coordinatorTrace: [{source:'Some(HoldHotkey)',gesture:2,phase:'IntentApplied',desiredAfter:'Off'}], physicalKeyboard: { source: 'fake', realKeyboardReads: 0, overflow: false,
    observations: ['Down', 'Up', 'Down', 'Up'].map(observation => ({ key: 7, modifiers: 3,
      source: 'fake', observation, downKeys: observation === 'Down' ? [7, 55, 56] : [] })),
    events: [event('pressed', 1, 'Down', 'Accepted'), event('pressed', 1, 'Down', 'Duplicate'),
      event('watcher', 1, 'Up', 'Rearmed'), event('pressed', 2, 'Down', 'Accepted'),
      event('watcher', 1, 'NotRead', 'Stale'), event('released', 2, 'Down', 'Stale'),
      event('watcher', 2, 'Up', 'HoldEnded')] } };
  expect(validE42PhysicalEvidence(state)).toBe(true);
  for (const mutate of [
    (p: any) => p.realKeyboardReads++,
    (p: any) => p.events.splice(2, 1),
    (p: any) => p.events.at(-1).handle.watcher = 1,
    (p: any) => p.observations[0].downKeys = [7, 55],
    (p: any) => p.observations = p.observations.filter((o: any) => o.observation !== 'Up'),
  ]) {
    const bad = structuredClone(state); mutate(bad.physicalKeyboard);
    expect(validE42PhysicalEvidence(bad)).toBe(false);
  }
});

for (const acknowledgement of ['accepted', 'lost', 'never'] as const) {
 for (const delayedAttachment of [false, true]) {
  it(`E63 ${acknowledgement} delayedAttachment=${delayedAttachment} handoff never creates a JS Stop or finish owner`, async () => {
    vi.useFakeTimers();
    fixture.invoke.mockClear();
    let reads = 0;
    let handedOff: any;
    fixture.invoke.mockImplementation((command, args) => {
      if (command === 'native_e2e_state') {
        const n = ++reads;
        const generation = n === 1 ? 1 : 2;
        return Promise.resolve({ logicalProviderRunId: 9, visible: true, windowEpoch: 5, coordinatorShownEpoch: 5,
          coordinatorCapture: { recording: !delayedAttachment || n !== 3, runId: generation },
          captureEpisode: { runId: generation, generation },
          fixture: { activeCaptures: n === 2 ? 0 : 1, captureStarts: generation,
            providerMarkers: [{ captureRunId: generation, captureFenceGeneration: generation,
              captureGeneration: generation, providerSessionId: 7 }],
            firstBWrites: n < 3 ? [] : [{ captureGeneration: 2, logicalRunId: 9, pauseEpoch: 4 }],
            controlResults: [
              { operation: 'pause', delivered: true, result: { decision: 'accepted' } },
              { operation: 'continue', delivered: true, result: { decision: 'accepted', pause_epoch: 4 } },
            ] } });
      }
      if (command === 'native_e2e_terminal_handoff') {
        handedOff = args;
        if (acknowledgement === 'never') return new Promise(() => {});
        if (acknowledgement === 'lost') return Promise.reject(new Error('ack lost'));
      }
      return Promise.resolve();
    });
    try {
      const task = runNativeContinuationCase({} as Pinia, 'after-write-toggle');
      await vi.advanceTimersByTimeAsync(2500);
      await task;
      expect(handedOff.report.case).toBe('after-write-toggle');
      expect(handedOff.report.b.fixture.firstBWrites).toHaveLength(1);
      expect(fixture.invoke.mock.calls.filter(([name]) => name === 'native_e2e_terminal_handoff')).toHaveLength(1);
      expect(fixture.invoke.mock.calls.filter(([name]) => name === 'native_e2e_hotkey')).toHaveLength(6);
      expect(fixture.invoke.mock.calls.some(([name]) =>
        ['stop_recording', 'native_e2e_close_recording', 'native_e2e_finish'].includes(name))).toBe(false);
      expect(reads).toBe(delayedAttachment ? 4 : 3);
      expect(handedOff.report.b.coordinatorCapture.recording).toBe(true);
    } finally { vi.useRealTimers(); }
  });
 }
}

it('handoff waits for both coordinator attachment and window completion without weakening identity checks', () => {
  const ready = { visible: true, windowEpoch: 5, coordinatorShownEpoch: 5,
    captureEpisode: { runId: 2 }, coordinatorCapture: { recording: true, runId: 2 } };
  expect(coordinatorReadyForHandoff(ready)).toBe(true);
  for (const change of [{ coordinatorCapture: { recording: false, runId: 2 } },
    { coordinatorCapture: { recording: true, runId: 3 } }, { coordinatorShownEpoch: null },
    { coordinatorShownEpoch: 4 }, { visible: false }]) {
    expect(coordinatorReadyForHandoff({ ...ready, ...change })).toBe(false);
  }
});
