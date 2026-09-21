import { describe, expect, it } from 'vitest';
import { bindWarmVisibleFrameEvidence, type WarmReopenEvidence } from './nativeMiniUxScenario';

describe('warm mini-window first-visible evidence', () => {
  it('preserves frames observed before the native window epoch arrives', () => {
    const reopen: WarmReopenEvidence = { attempt: 3, windowEpoch: null, pendingFrames: [] };
    const stale = { source: 'render' as const, revision: 8, runId: 21, captureReady: false,
      readinessReason: 'activating-warm-capture', phase: 'mini-status-dot starting', statusText: 'Starting' };
    expect(bindWarmVisibleFrameEvidence(reopen, stale)).toEqual([]);
    reopen.windowEpoch = 17;
    const frames = bindWarmVisibleFrameEvidence(reopen, { ...stale, source: 'shown', captureReady: true,
      readinessReason: 'recording', phase: 'mini-status-dot recording', statusText: 'Recording' });
    expect(frames).toHaveLength(2);
    expect(frames[0]).toMatchObject({ attempt: 3, source: 'render', windowEpoch: 17,
      phase: 'mini-status-dot starting', statusText: 'Starting' });
    expect(frames[1]).toMatchObject({ source: 'shown', windowEpoch: 17, captureReady: true });
    expect(reopen.pendingFrames).toEqual([]);
  });
});
