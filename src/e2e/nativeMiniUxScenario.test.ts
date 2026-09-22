import { describe, expect, it } from 'vitest';
import { bindWarmVisibleFrameAfterNative, bindWarmVisibleFrameEvidence,
  drainPendingWarmVisibleObservations, hasMatchingRecoveryPcm, reserveWarmVisibleFrameObservation,
  sealWarmVisibleObservations,
  warmVisibleFramesHaveNoStaleStatus, type WarmReopenEvidence,
  type WarmVisibleFrame } from './nativeMiniUxScenario';

describe('warm mini-window first-visible evidence', () => {
  it('coalesces identical render proofs without dropping a distinct status frame', () => {
    const reopen: WarmReopenEvidence = { attempt: 1, baselineWindowEpoch: 1,
      windowEpoch: 2, closed: false };
    const frame = { source: 'render' as const, revision: 3, runId: 4, captureReady: false,
      readinessReason: 'activating-warm-capture', phase: 'mini-status-dot', statusText: '' };
    expect(reserveWarmVisibleFrameObservation(reopen, frame, 2)).toBe(true);
    expect(reserveWarmVisibleFrameObservation(reopen, frame, 2)).toBe(false);
    expect(reserveWarmVisibleFrameObservation(reopen,
      { ...frame, statusText: 'Starting' }, 2)).toBe(true);
  });

  it('binds frames only to an authoritative visible native epoch', () => {
    const reopen: WarmReopenEvidence = { attempt: 3, baselineWindowEpoch: 16,
      windowEpoch: null, closed: false };
    const stale = { source: 'render' as const, revision: 8, runId: 21, captureReady: false,
      readinessReason: 'activating-warm-capture', phase: 'mini-status-dot starting', statusText: 'Starting' };
    expect(bindWarmVisibleFrameEvidence(reopen, stale, { visible: false, windowEpoch: 16 })).toEqual([]);
    expect(bindWarmVisibleFrameEvidence(reopen, stale, { visible: true, windowEpoch: 18 }, 17)).toEqual([]);
    const frames = bindWarmVisibleFrameEvidence(reopen, { ...stale, source: 'shown', captureReady: true,
      readinessReason: 'recording', phase: 'mini-status-dot recording', statusText: 'Recording' },
    { visible: true, windowEpoch: 17 }, 17);
    expect(frames).toHaveLength(1);
    expect(frames[0]).toMatchObject({ attempt: 3, source: 'shown', windowEpoch: 17, captureReady: true });
    expect(bindWarmVisibleFrameEvidence(reopen, stale,
      { visible: true, windowEpoch: 18 })).toEqual([]);
  });

  it('preserves the callback-time bad frame while delayed native reads prove its epoch', async () => {
    const reopen: WarmReopenEvidence = { attempt: 4, baselineWindowEpoch: 20,
      windowEpoch: null, closed: false };
    let statusText = 'Starting';
    let resolveAfter!: (value: { visible: boolean; windowEpoch: number }) => void;
    const after = new Promise<{ visible: boolean; windowEpoch: number }>(resolve => { resolveAfter = resolve; });
    let reads = 0;
    const stable = bindWarmVisibleFrameAfterNative(reopen,
      () => ++reads === 1 ? Promise.resolve({ visible: true, windowEpoch: 21 }) : after, () => ({
      source: 'render', revision: 9, runId: 22, captureReady: statusText === 'Recording',
      readinessReason: statusText === 'Recording' ? 'recording' : 'activating-warm-capture',
      phase: `mini-status-dot ${statusText.toLowerCase()}`, statusText,
    }), 21);
    await Promise.resolve();
    statusText = 'Recording';
    resolveAfter({ visible: true, windowEpoch: 21 });
    await expect(stable).resolves.toEqual([expect.objectContaining({
      attempt: 4, windowEpoch: 21, statusText: 'Starting', captureReady: false,
    })]);

    const transitioning = { ...reopen, windowEpoch: null };
    const samples = [{ visible: false, windowEpoch: 20 }, { visible: true, windowEpoch: 21 }];
    await expect(bindWarmVisibleFrameAfterNative(transitioning,
      async () => samples.shift()!, () => ({
        source: 'render', revision: 9, runId: 22, captureReady: false,
        readinessReason: 'activating-warm-capture', phase: 'mini-status-dot starting',
        statusText: 'Starting',
      }), 21)).resolves.toEqual([]);

    const unproven = { ...reopen, windowEpoch: null };
    await expect(bindWarmVisibleFrameAfterNative(unproven,
      async () => ({ visible: true, windowEpoch: 22 }), () => ({
        source: 'render', revision: 10, runId: 23, captureReady: false,
        readinessReason: 'activating-warm-capture', phase: 'mini-status-dot',
        statusText: '',
      }))).resolves.toEqual([]);
  });

  it('retains an unproven non-neutral frame and never adopts a later mutable epoch', async () => {
    const reopen: WarmReopenEvidence = { attempt: 5, baselineWindowEpoch: 21,
      windowEpoch: null, closed: false };
    let nativeReads = 0;
    await expect(bindWarmVisibleFrameAfterNative(reopen,
      async () => { nativeReads += 1; return { visible: true, windowEpoch: 22 }; }, () => ({
        source: 'render', revision: 10, runId: 23, captureReady: false,
        readinessReason: 'activating-warm-capture', phase: 'mini-status-dot starting',
        statusText: 'Starting',
      }))).resolves.toEqual([]);
    expect(nativeReads).toBe(0);
    await expect(bindWarmVisibleFrameAfterNative(reopen,
      async () => ({ visible: true, windowEpoch: 22 }), () => ({
        source: 'shown', revision: 11, runId: 23, captureReady: true,
        readinessReason: 'recording', phase: 'mini-status-dot recording', statusText: 'Recording',
      }), 22)).resolves.toEqual([
      expect.objectContaining({ source: 'render', statusText: 'Starting', windowEpoch: 22 }),
      expect.objectContaining({ source: 'shown', statusText: 'Recording', windowEpoch: 22 }),
    ]);

    const delayed: WarmReopenEvidence = { attempt: 6, baselineWindowEpoch: 21,
      windowEpoch: 22, closed: false };
    let resolveNative!: (value: { visible: boolean; windowEpoch: number }) => void;
    const firstRead = new Promise<{ visible: boolean; windowEpoch: number }>(resolve => {
      resolveNative = resolve;
    });
    const observation = bindWarmVisibleFrameAfterNative(delayed,
      () => firstRead, () => ({
        source: 'render', revision: 11, runId: 24, captureReady: false,
        readinessReason: 'activating-warm-capture', phase: 'mini-status-dot', statusText: '',
      }));
    delayed.windowEpoch = 23;
    resolveNative({ visible: true, windowEpoch: 23 });
    await expect(observation).resolves.toEqual([]);
  });

  it('removes pending frames by identity when epoch proofs complete concurrently', async () => {
    const reopen: WarmReopenEvidence = { attempt: 7, baselineWindowEpoch: 21,
      windowEpoch: null, closed: false };
    const frame = (source: 'render' | 'shown' | 'sample', statusText: string) => ({
      source, revision: 12, runId: 25, captureReady: statusText === 'Recording',
      readinessReason: statusText === 'Recording' ? 'recording' : 'activating-warm-capture',
      phase: `mini-status-dot ${statusText.toLowerCase()}`, statusText,
    });
    await bindWarmVisibleFrameAfterNative(reopen,
      async () => ({ visible: true, windowEpoch: 22 }), () => frame('render', 'Starting'));
    let resolveNative!: (value: { visible: boolean; windowEpoch: number }) => void;
    const native = new Promise<{ visible: boolean; windowEpoch: number }>(resolve => {
      resolveNative = resolve;
    });
    const first = bindWarmVisibleFrameAfterNative(reopen, () => native,
      () => frame('shown', 'Recording'), 22);
    const second = bindWarmVisibleFrameAfterNative(reopen, () => native,
      () => frame('sample', 'Recording'), 22);
    await bindWarmVisibleFrameAfterNative(reopen,
      async () => ({ visible: true, windowEpoch: 22 }), () => frame('render', 'Processing'));
    resolveNative({ visible: true, windowEpoch: 22 });
    await Promise.all([first, second]);
    expect(reopen.pendingFrames?.map(candidate => candidate.statusText)).toEqual(['Processing']);
    await expect(bindWarmVisibleFrameAfterNative(reopen,
      async () => ({ visible: true, windowEpoch: 22 }), () => frame('sample', 'Recording'),
      22)).resolves.toEqual([
      expect.objectContaining({ statusText: 'Processing', windowEpoch: 22 }),
      expect.objectContaining({ statusText: 'Recording', windowEpoch: 22 }),
    ]);
    expect(reopen.pendingFrames).toEqual([]);
  });

  it('drains observations admitted while an earlier proof is settling', async () => {
    const pending = new Set<Promise<void>>();
    let resolveFirst!: () => void;
    let resolveSecond!: () => void;
    const second = new Promise<void>(resolve => { resolveSecond = resolve; })
      .finally(() => pending.delete(second));
    const first = new Promise<void>(resolve => { resolveFirst = resolve; })
      .then(() => { pending.add(second); })
      .finally(() => pending.delete(first));
    pending.add(first);
    const drained = drainPendingWarmVisibleObservations(pending);
    resolveFirst();
    await Promise.resolve();
    expect(pending.has(second)).toBe(true);
    let finished = false;
    void drained.then(() => { finished = true; });
    await Promise.resolve();
    expect(finished).toBe(false);
    resolveSecond();
    await drained;
    expect(pending.size).toBe(0);
  });

  it('closes admission with the final sample and still drains admitted native proofs', async () => {
    const reopen = { attempt: 1, baselineWindowEpoch: 1, windowEpoch: 2, closed: false,
      acceptingObservations: true };
    const pending = new Set<Promise<void>>();
    let release!: () => void;
    const first = new Promise<void>(resolve => { release = resolve; })
      .finally(() => pending.delete(first));
    pending.add(first);
    let sampled = false;
    const sealed = sealWarmVisibleObservations(reopen, pending, () => { sampled = true; });
    await Promise.resolve();
    expect(reopen.acceptingObservations).toBe(false);
    expect(sampled).toBe(true);
    release();
    await sealed;
    expect(reopen.acceptingObservations).toBe(false);
  });

  it('seals atomically when the final pending set becomes empty', async () => {
    const reopen = { attempt: 1, baselineWindowEpoch: 1, windowEpoch: 2, closed: false,
      acceptingObservations: true };
    let armLateAdmission = false;
    let lateAdmissionAttempted = false;
    let lateAdmissionAccepted = false;
    class AdmissionSet extends Set<Promise<void>> {
      override get size() {
        const current = super.size;
        if (current === 0 && armLateAdmission && !lateAdmissionAttempted) {
          lateAdmissionAttempted = true;
          queueMicrotask(() => {
            if (!reopen.acceptingObservations) return;
            lateAdmissionAccepted = true;
            const observation = Promise.resolve().finally(() => this.delete(observation));
            this.add(observation);
          });
        }
        return current;
      }
    }
    const pending = new AdmissionSet();
    await sealWarmVisibleObservations(reopen, pending, () => { armLateAdmission = true; });
    await Promise.resolve();
    expect(lateAdmissionAttempted).toBe(true);
    expect(lateAdmissionAccepted).toBe(false);
    expect(reopen.acceptingObservations).toBe(false);
    expect(pending.size).toBe(0);
  });

  it('rejects a stale label even when its native proof settles after a valid frame', () => {
    const frame = (statusText: string): WarmVisibleFrame => ({
      attempt: 1, source: 'render', windowEpoch: 2, revision: 1, runId: 1,
      captureReady: true, readinessReason: 'recording', phase: 'mini-status-dot recording', statusText,
    });
    expect(warmVisibleFramesHaveNoStaleStatus(
      [frame('Listening...'), frame('Starting...')], ['Starting...', 'Processing...'],
    )).toBe(false);
  });

  it('requires positive matching capture and provider PCM for recovery', () => {
    const snapshot = { fixture: {
      captureRunAssociations: [{ captureRunId: 42, captureGeneration: 7 }],
      capturePcmLedgers: [{ captureGeneration: 7, samples: 320, hash: 'abc' }],
      providerPcmLedgers: [{ captureGeneration: 7, samples: 320, hash: 'abc' }],
    } } as never;
    expect(hasMatchingRecoveryPcm(snapshot, 42)).toBe(true);
    const empty = structuredClone(snapshot) as typeof snapshot;
    (empty as any).fixture.capturePcmLedgers[0].samples = 0;
    expect(hasMatchingRecoveryPcm(empty, 42)).toBe(false);
    const corrupt = structuredClone(snapshot) as typeof snapshot;
    (corrupt as any).fixture.providerPcmLedgers[0].hash = 'def';
    expect(hasMatchingRecoveryPcm(corrupt, 42)).toBe(false);
  });
});
