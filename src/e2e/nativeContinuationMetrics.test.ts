import { describe, it, expect } from 'vitest';
import { continuationMetrics as metrics, clockCalibration, subtractIntervals, appearanceBracket, syntheticReadbackMetrics, type NativeReadback, type ClockExchange, type ContinuationObservation } from './nativeContinuationMetrics';
const probes: ClockExchange[] = [0, 10, 20, 3000, 3010, 3020].map((n, i) => ({ phase: i < 3 ? 'pre' : 'post', n, w0: n + 50, w1: n + 50 }));
const continuationMetrics: typeof metrics = (episodes, events) => metrics(episodes, events, probes);
const episodes = [
  { episode: 0, startMs: 100, sourceCompleteMs: 1100, micReleasedMs: 1120, logicalRunId: 7, source: { sourceDurationMs: 1000, nativeSourceStartMs: 50, nativeSourceEndMs: 1050 } },
  { episode: 1, startMs: 1250, sourceCompleteMs: 2250, micReleasedMs: 2260, logicalRunId: 7, source: { sourceDurationMs: 1000, nativeSourceStartMs: 1200, nativeSourceEndMs: 2200, providerSourceOffsetMs: 1000 } },
];
const event = (atMs: number, sourceStartSeconds: number, stable = true): ContinuationObservation => ({
  event: 'transcription:partial', atMs, sessionId: 7, deliverySeq: 1,
  nonempty: true, stable, timingKnown: true, sourceStartSeconds, sourceDurationSeconds: .5,
});
describe('continuation timing evidence', () => {
  it('does not mistake a late A stable event for first B text', () => {
    const metrics = continuationMetrics(episodes, [event(1400, 0), event(1500, 1, false), event(2400, 1)]);
    expect(metrics[1]).toMatchObject({ firstTextMs: [250, 250], firstStableMs: [1150, 1150], residualStableLagMs: [150, 150] });
    expect(metrics[0].firstTextMs).toEqual([1300, 1300]);
  });
  it('keeps missing/unknown source timing unproven rather than zero latency', () => {
    const metrics = continuationMetrics(episodes, [{ ...event(1500, 1), timingKnown: false }]);
    expect(metrics[1]).toMatchObject({ firstTextMs: null, residualStableLagMs: null, attribution: 'unproven' });
  });
  it('does not assume Pause silence occupies zero ASR time', () => {
    const unmapped = [episodes[0], { ...episodes[1], source: { sourceDurationMs: 1000 } }];
    expect(continuationMetrics(unmapped, [event(1500, 1)])[1]).toMatchObject({
      firstTextMs: null, sourceStartOffsetMs: null, attribution: 'unproven',
    });
  });
  it('resets the source offset only for a new logical run and rejects another owner', () => {
    const cold = [episodes[0], { ...episodes[1], logicalRunId: 8 }];
    const metrics = continuationMetrics(cold, [event(1500, 0), { ...event(1600, 0), sessionId: 8 }]);
    expect(metrics[1]).toMatchObject({ sourceStartOffsetMs: 0, firstTextMs: [350, 350] });
  });
});

import { nativeInsertionMetrics, syntheticPhraseOccurrences, type NativeInsertionTrace } from './nativeContinuationMetrics';
describe('actual native delivery correlation', () => {
  const record: NativeInsertionTrace['records'][number] = { logicalRunId: 7, deliverySeq: 1, copy: false,
    queuedMs: 10, executorStartMs: 20, insertionStartMs: 25, insertionEndMs: 30, endMs: 40,
    insertionConfirmed: false, result: { status: 'uncertain' } };
  it('retains uncertain attempt and duplicate no-op separately without fabricating confirmation', () => {
    const result = nativeInsertionMetrics({ overflow: false, records: [record,
      { ...record, insertionStartMs: null, insertionEndMs: null, result: { status: 'confirmed' } }] }, [event(1000000, 0)]);
    expect(result.records[0]).toMatchObject({ eventIds: [0], insertionDurationMs: 5, insertionConfirmed: false, attemptedInsertion: true });
    expect(result.records[1]).toMatchObject({ eventIds: [0], insertionDurationMs: null, attemptedInsertion: false, attemptOrdinal: 1 });
  });
  it('requires both run and delivery identity; refusal is not insertion', () => {
    const result = nativeInsertionMetrics({ overflow: false, records: [{ ...record, logicalRunId: 8,
      insertionStartMs: null, insertionEndMs: null, result: { status: 'context_mismatch' } }] }, [event(100, 0)]);
    expect(result.records[0]).toMatchObject({ eventIds: [], attemptedInsertion: false, insertionDurationMs: null });
  });
  it('invalidates derived evidence on native or event overflow', () => {
    for (const native of [true, false]) {
      const result = nativeInsertionMetrics({ overflow: native, records: [record] }, [], !native);
      expect(result.valid).toBe(false);
      expect(result.records[0].insertionDurationMs).toBeNull();
    }
  });
  it('preserves repeated synthetic phrases and never assigns causal episodes', () => {
    const text = 'За окном растёт берёза. За окном растёт берёза.';
    expect(syntheticPhraseOccurrences(text).map(x => x.occurrenceOrdinal)).toEqual([0, 1]);
    expect(syntheticPhraseOccurrences('unknown')).toEqual([]);
    expect(syntheticPhraseOccurrences(text.repeat(1000))).toHaveLength(64);
  });
});

import { boundedSyntheticText } from './nativeContinuationMetrics';
describe('bounded raw synthetic event evidence', () => {
  it('preserves ordinary text, omissions, duplicate revisions and whitespace exactly', () => {
    const texts = [' На столе  книга.\nЗа окном берёза! ', 'partial', 'partial', '', 'x'.repeat(4096)];
    expect(texts.map(text => boundedSyntheticText(text).rawSyntheticText)).toEqual(texts);
    for (const text of texts) expect(boundedSyntheticText(text)).toMatchObject({ syntheticTextValid: true, syntheticTextTruncated: false });
    expect(boundedSyntheticText(undefined).rawSyntheticText).toBeNull();
  });
  it('caps oversized text and invalidates evidence via the existing overflow gate', () => {
    const result = boundedSyntheticText('я'.repeat(4097));
    expect(result.rawSyntheticText).toBe('я'.repeat(4096));
    expect(result).toMatchObject({ syntheticTextValid: false, syntheticTextTruncated: true });
    expect(nativeInsertionMetrics({ overflow: false, records: [] }, [], !result.syntheticTextValid).valid).toBe(false);
  });
});

it('continuous baseline B never reports a fictional hotkey latency or A mic release', () => {
  const continuous = episodes.map(row => ({ ...row, source: { ...row.source, continuousCapture: true } }));
  const result = continuationMetrics([{ ...continuous[0], micReleasedMs: null }, continuous[1]], [event(1500, 1)]);
  expect(result[0].micReleaseLagMs).toBeNull();
  expect(result[1]).toMatchObject({ firstTextMs: [250, 250], firstStableMs: [250, 250], lastStableMs: [250, 250], intentToFirstTextMs: null,
    sourceBoundary: 'native-source' });
});

it('stable-first leaves partial absent and retains negative residual, missing clocks and cross-boundary attribution', () => {
  const rows = [event(900, 0), event(950, 0, false)];
  expect(continuationMetrics(episodes, rows)[0]).toMatchObject({ firstPartialMs: null, firstTextMs: [800, 800],
    residualStableLagMs: [-200, -200], clampedRemainingStableLagMs: [0, 0] });
  expect(metrics(episodes, rows)[0].firstTextMs).toBeNull();
  expect(continuationMetrics([{ ...episodes[0], source: { ...episodes[0].source, nativeSourceStartMs: 400, nativeSourceEndMs: 1400 } }], rows)[0])
    .toMatchObject({ firstTextMs: [450, 450], residualStableLagMs: [-550, -550] });
  expect(continuationMetrics([{ ...episodes[0], source: { ...episodes[0].source, sourceDurationMs: Infinity } }], rows)[0].attribution).toBe('unproven');
  expect(continuationMetrics([{ ...episodes[0], source: { ...episodes[0].source, nativeSourceEndMs: 0 } }], rows)[0].firstTextMs).toBeNull();
  expect(continuationMetrics(episodes, [event(1800, .75)])[1].attribution).toBe('unproven');
});
it('clock bounds retain asymmetric uncertainty and refuse missing, reset, nonfinite and contradictory probes', () => {
  const delayed = probes.map(p => ({ ...p, w0: p.w0 - 2, w1: p.w1 + 3 }));
  expect(clockCalibration(delayed).offset).toEqual([48, 53]);
  expect(subtractIntervals([100, 110], [48, 53])).toEqual([47, 62]);
  expect(metrics(episodes, [event(900, 0)], delayed)[0].firstTextMs).toEqual([797, 802]);
  for (const broken of [probes.slice(1), [...probes, probes[5]],
    probes.map((p, i) => i === 4 ? { ...p, n: 1 } : p),
    probes.map((p, i) => i === 5 ? { ...p, w1: Infinity } : p),
    probes.map((p, i) => i === 5 ? { ...p, w0: p.w0 + 1, w1: p.w1 + 1 } : p)])
    expect(clockCalibration(broken).offset).toBeNull();
  expect(subtractIntervals([Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER], [-10, -10])).toBeNull();
});
const readback = (): NativeReadback => ({ armed: true, stopped: true, valid: true, error: null, maxSamplingGapMs: 25,
  records: ['', 'B', '', 'B'].map((text, sequence) => ({ sequence, text, identityValid: true,
    readStartMs: 10 + sequence * 40, readEndMs: 15 + sequence * 40,
    lastReadStartMs: 30 + sequence * 40, lastReadEndMs: 35 + sequence * 40, samples: 2, maxSamplingGapMs: 25 })) });
it('brackets appearance using last absent read START and first present END, retaining revisions', () => {
  const trace = readback();
  expect(appearanceBracket(trace, t => t.includes('B'))).toEqual([30, 55]);
  expect(appearanceBracket(trace, t => t === 'missing')).toBeNull();
  expect(trace.records.map(r => r.text)).toEqual(['', 'B', '', 'B']);
  const result = syntheticReadbackMetrics(trace, episodes, 'B');
  expect(result.observations[result.observations.length - 1]?.bracket).toEqual([30, 55]);
  expect(result.attribution).toBe('unproven');
});
it('invalidates all derived OS evidence on late identity error, overflow, bad clocks or incomplete lifecycle', () => {
  for (const change of [(r: NativeReadback) => { r.valid = false; },
    (r: NativeReadback) => { r.stopped = false; },
    (r: NativeReadback) => { r.error = 'AX timeout'; },
    (r: NativeReadback) => { r.records[3].identityValid = false; },
    (r: NativeReadback) => { r.records[3].text = 'x'.repeat(4097); },
    (r: NativeReadback) => { r.records[3].readStartMs = NaN; },
    (r: NativeReadback) => { r.records[0].text = 'not initially empty'; }]) {
    const trace = readback(); change(trace);
    expect(appearanceBracket(trace, t => t === 'B')).toBeNull();
  }
});
