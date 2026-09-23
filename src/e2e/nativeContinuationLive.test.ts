import { beforeEach, expect, it, vi } from 'vitest';
import type { Pinia } from 'pinia';
const fixture = vi.hoisted(() => ({ invoke: vi.fn(), store: { finalText: '' }, listeners: {} as Record<string, (event: any) => void> }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: fixture.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (name, callback) => { fixture.listeners[name] = callback; return () => {}; }) }));
vi.mock('@/stores/appConfig', () => ({ useAppConfigStore: () => ({ startSync: async () => {}, refresh: async () => {} }) }));
vi.mock('@/stores/transcription', () => ({ useTranscriptionStore: () => fixture.store }));
const nativeReadback = () => ({ armed: true, valid: true, stopped: false, error: null, maxSamplingGapMs: 0, records: [{ sequence: 0, text: '', identityValid: true, readStartMs: 0, readEndMs: 0, lastReadStartMs: 0, lastReadEndMs: 0, samples: 1, maxSamplingGapMs: 0 }] });
import { runNativeContinuationLive } from './nativeContinuationLive';
beforeEach(() => { fixture.invoke.mockReset(); fixture.store.finalText = ''; fixture.listeners = {}; });
for (const id of ['warm-baseline-1', 'warm-continue-1', 'warm-continue-gap-0', 'warm-continue-gap-200', 'cold-0', 'cold-4000', 'cold-8000', 'long', 'short-tail', 'old-tail']) {
  for (const fault of ['none', 'missing', 'duplicate', 'unexpected', 'incomplete', 'same-cold-owner', 'history-count-mismatch', 'retained-connection', 'reader-not-armed', 'reader-calibration-invalid', 'reader-identity-error', 'reader-join-timeout']) {
  it(`${id}: ${fault}; actual Ready gate, terminal owners and insertion ledger`, async () => {
    const baseline = id.startsWith('warm-baseline');
    const continuation = !baseline && !id.startsWith('cold');
    const gated = baseline || continuation;
    let pressed = false; let released = false; let polls = 0; let gateCalls = 0;
    let stateCalls = 0;
    let report: any;
    const source = (name: string, generation: number) => ({ name, bytes: 640, sourceFrames: 320, emittedFrames: 0,
      sourceDurationMs: 20, captureGeneration: generation, sourceGateRequired: gated && generation === 1,
      sourceGateReady: null as any, nativeSourceStartMs: null as number | null, nativeSourceEndMs: null as number | null });
    const gapMs = id.startsWith('warm-continue-gap-') ? Number(id.slice('warm-continue-gap-'.length)) : undefined;
    const episodes = id === 'long' ? ['episode-a.pcm', 'episode-b.pcm', 'long-auto-commit.pcm', 'episode-b.pcm'] : ['episode-a.pcm', 'episode-b.pcm'];
    const value = { nativeReadback: nativeReadback(), qualificationTrial: { id, continuation, configDelayMs: 0, gapMs, episodes },
      qualificationEndpoint: 'ws://127.0.0.1:52999', status: 'Idle', historyEntryCount: 0, preparedCaptureTokenCount: 0,
      logicalProviderRunId: 1, providerTransport: { serverReady: false, connectionRetained: false }, nativeInsertionTrace: { overflow: false, records: [] },
      fixture: { sourceEpisodes: [] as ReturnType<typeof source>[], activeCaptures: 0, captureStarts: 0, captureStops: 0, observationOverflow: false } };
    if (fault === 'reader-not-armed') value.nativeReadback.armed = false;
    fixture.invoke.mockImplementation(async (command, args) => {
      if (command === 'native_e2e_state') {
        stateCalls++;
        // Initial + armed + three pre probes: fail inside the third calibration call.
        if (fault === 'reader-calibration-invalid' && stateCalls === 5) {
          value.nativeReadback.valid = false;
          (value.nativeReadback as { error: string | null }).error = 'AXFocusedUIElement: native-error code=Some(-25204)';
        }
        if (args.stopReadback) {
          value.nativeReadback.stopped = fault !== 'reader-join-timeout';
          if (fault === 'reader-identity-error') value.nativeReadback.valid = false;
        }
        if (value.fixture.activeCaptures) {
          polls++;
          // Two polls of delayed Starting must have zero warm frames, but cold emits immediately.
          if (polls >= 3 || value.fixture.captureStarts === 2) value.status = 'Recording';
          // Recording precedes actual Server Ready by two state observations.
          value.providerTransport.serverReady = polls >= 5 || value.fixture.captureStarts === 2;
          if (gated && !released && polls < 5) expect(gateCalls).toBe(0);
          if (!gated || released || value.fixture.captureStarts === 2) {
            for (const row of value.fixture.sourceEpisodes) {
              row.emittedFrames = 320; row.nativeSourceStartMs = 100; row.nativeSourceEndMs = 120;
              if (baseline) fixture.store.finalText = 'AB';
            }
          } else expect(value.fixture.sourceEpisodes.every(row => row.emittedFrames === 0)).toBe(true);
        }
        return { ...structuredClone(value), nativeClockMs: performance.now() };
      }
      if (command === 'native_e2e_configure') {
        if (args.config.sourceGateReady) {
          gateCalls++;
          expect(value.status).toBe('Recording');
          expect(value.providerTransport.serverReady).toBe(true);
          expect(value.fixture.captureStarts).toBe(1);
          expect(value.fixture.sourceEpisodes.every(row => row.emittedFrames === 0)).toBe(true);
          value.fixture.sourceEpisodes[0].sourceGateReady = { status: 'Recording', serverReady: true, nativeReadyMs: 90, emittedFrames: 0 };
          released = true;
        } else expect(args.config.keepAlive).toBe(false);
      }
      if (command === 'native_e2e_hotkey' && args.action === 'press') {
        pressed = !pressed;
        if (pressed) {
          value.fixture.captureStarts++;
          value.providerTransport.connectionRetained = true;
          if (!gated && fault !== 'same-cold-owner') value.logicalProviderRunId = value.fixture.captureStarts;
          value.fixture.activeCaptures = 1; value.status = 'Starting';
          const index = value.fixture.captureStarts - 1;
          value.fixture.sourceEpisodes.push(source(value.qualificationTrial.episodes[index], index + 1));
          if (baseline) value.fixture.sourceEpisodes.push(source('episode-b.pcm', 1));
        } else {
          value.fixture.captureStops++; value.fixture.activeCaptures = 0;
          if (continuation && value.fixture.captureStops < episodes.length) value.status = 'Paused';
          else {
            value.status = 'Idle';
            if (gated) value.historyEntryCount++;
            else {
              const stableCount = id === 'cold-0' && value.fixture.captureStops === 1 ? 7 : 1;
              for (let segment = 1; segment <= stableCount; segment++) {
                fixture.listeners['transcription:final']({ payload: { session_id: value.logicalProviderRunId,
                  delivery_seq: segment, text: `stable segment ${segment}` } });
                value.historyEntryCount++;
              }
            }
            if (fault === 'history-count-mismatch' && value.fixture.captureStops === (baseline ? 1 : episodes.length)) value.historyEntryCount++;
            value.providerTransport = { serverReady: false, connectionRetained: fault === 'retained-connection' };
            if (!baseline) fixture.store.finalText = value.fixture.captureStops === 1 ? 'A' : 'B';
            const terminal = { payload: { session_id: fault === 'unexpected' ? 999 : value.logicalProviderRunId,
              delivery_complete: fault !== 'incomplete', error: null } };
            if (fault !== 'missing') fixture.listeners['transcription:terminal'](terminal);
            if (fault === 'duplicate') fixture.listeners['transcription:terminal'](terminal);
          }
        }
      }
      if (command === 'check_accessibility_permission') return true;
      if (command === 'native_e2e_prepare_live_target') return 'p4-textedit-a.txt';
      if (command === 'native_e2e_finish') report = args.report;
    });
    await runNativeContinuationLive({} as Pinia);
    if (fault !== 'none' && !(fault === 'same-cold-owner' && gated)) {
      expect(report.passed).toBe(false); expect(report.errors.length).toBeGreaterThan(0);
      if (fault === 'history-count-mismatch') expect(report.errors).toContain('Error: History count mismatch');
      if (fault === 'reader-not-armed' || fault === 'reader-calibration-invalid') {
        expect(value.fixture.captureStarts).toBe(0);
        expect(fixture.invoke.mock.calls.some(([command]) => command === 'native_e2e_hotkey')).toBe(false);
        expect(gateCalls).toBe(0);
        if (fault === 'reader-calibration-invalid') {
          expect(report.errors.some((e: string) => e.includes('after-pre-calibration: owned OS reader invalid: AXFocusedUIElement'))).toBe(true);
          expect(report.clockExchanges.filter((p: any) => p.phase === 'pre')).toHaveLength(3);
          expect(report.final.nativeReadback.armed).toBe(true);
        } else expect(report.errors.some((e: string) => e.includes('not armed'))).toBe(true);
      }
      expect(fixture.invoke.mock.calls.some(([command, args]) => command === 'native_e2e_state' && args?.stopReadback)).toBe(true);
      return;
    }
    expect(report.errors).toEqual([]);
    expect(report.passed).toBe(true);
    expect(report.episodes).toHaveLength(episodes.length);
    expect(report.clockExchanges.map((p: any) => p.phase)).toEqual(['pre', 'pre', 'pre', 'post', 'post', 'post']);
    expect(fixture.invoke.mock.calls.filter(([command, args]) => command === 'native_e2e_state' && args?.stopReadback)).toHaveLength(1);
    expect(value.fixture.captureStarts).toBe(baseline ? 1 : episodes.length);
    expect(value.fixture.captureStops).toBe(baseline ? 1 : episodes.length);
    if (gapMs !== undefined) expect(fixture.invoke.mock.calls.filter(([command, args]) => command === 'native_e2e_delay' && args.durationMs === gapMs)).toHaveLength(1);
    expect(gateCalls).toBe(gated ? 1 : 0);
    expect(report.expectedInsertion).toBe(baseline ? 'AB' : continuation ? 'B' : 'AB');
    if (id === 'cold-0') {
      expect(report.events.filter((event: any) => event.event === 'transcription:final' && event.nonempty)).toHaveLength(8);
      expect(report.final.historyEntryCount).toBe(8);
    }
    if (baseline) expect(report.episodes[0].micReleasedMs).toBeNull();
  });
  }
}
it('failed Ready release stops the pending capture and retains the failure', async () => {
  let stopped = 0; let report: any;
  const value = { nativeReadback: nativeReadback(), qualificationTrial: { id: 'warm-baseline-1', continuation: false, configDelayMs: 0, episodes: ['episode-a.pcm', 'episode-b.pcm'] },
    qualificationEndpoint: 'ws://127.0.0.1:52999', status: 'Recording', historyEntryCount: 0, providerTransport: { serverReady: true, connectionRetained: true },
    nativeInsertionTrace: { overflow: false, records: [] },
    fixture: { activeCaptures: 1, sourceEpisodes: [{ emittedFrames: 0 }], observationOverflow: false } };
  fixture.invoke.mockImplementation(async (command, args) => {
    if (command === 'native_e2e_state') { if (args.stopReadback) value.nativeReadback.stopped = true; return { ...structuredClone(value), nativeClockMs: performance.now() }; }
    if (command === 'native_e2e_configure' && args.config.sourceGateReady) throw new Error('provider lost Ready');
    if (command === 'check_accessibility_permission') return true;
    if (command === 'stop_recording') { stopped++; value.fixture.activeCaptures = 0; }
    if (command === 'native_e2e_finish') report = args.report;
  });
  await runNativeContinuationLive({} as Pinia);
  expect(stopped).toBe(1);
  expect(report.passed).toBe(false);
  expect(report.errors).toContain('Error: provider lost Ready');
});
