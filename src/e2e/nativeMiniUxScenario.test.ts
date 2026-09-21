import { describe, expect, it } from 'vitest';
import { bindWarmVisibleFrameAfterNative, bindWarmVisibleFrameEvidence, hasMatchingRecoveryPcm,
  type WarmReopenEvidence } from './nativeMiniUxScenario';

describe('warm mini-window first-visible evidence', () => {
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

  it('preserves a bad frame only when the native visible epoch is stable around it', async () => {
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
    }));
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
      }))).resolves.toEqual([]);
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
