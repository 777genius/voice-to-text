import { describe, expect, it } from 'vitest';
import { bindWarmVisibleFrameEvidence, hasMatchingRecoveryPcm,
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
