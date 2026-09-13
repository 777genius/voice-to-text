/** Raw WebView receipts and native source/AX times have distinct monotonic origins. */
export type ContinuationObservation = {
  event: string; atMs: number; sessionId: number; deliverySeq: number | null;
  nonempty: boolean; stable: boolean; timingKnown: boolean;
  sourceStartSeconds: number; sourceDurationSeconds: number;
};
export type ContinuationEpisode = {
  episode: number; startMs: number; sourceCompleteMs: number; micReleasedMs: number | null;
  nativeEvidence?: { captureEpisode: unknown; pausedContinuation: unknown };
  logicalRunId: number; source: { continuousCapture?: boolean; sourceDurationMs: number; nativeSourceStartMs?: number; nativeSourceEndMs?: number; providerSourceOffsetMs?: number };
};
export function continuationMetrics(episodes: ContinuationEpisode[], events: ContinuationObservation[], probes: ClockExchange[] = []) {
  const calibration = clockCalibration(probes);
  let sourceOffsetMs = 0;
  return episodes.map((episode, index) => {
    if (index === 0 || episode.logicalRunId !== episodes[index - 1].logicalRunId) sourceOffsetMs = 0;
    // Pause may inject provider-only silence. A cumulative source-byte offset is
    // not an ASR offset for B without an explicit native/backend mapping.
    const continued = index > 0 && episode.logicalRunId === episodes[index - 1].logicalRunId;
    const mappingKnown = !continued || (Number.isFinite(episode.source.providerSourceOffsetMs) && episode.source.providerSourceOffsetMs! >= 0);
    const startOffset = continued ? episode.source.providerSourceOffsetMs ?? sourceOffsetMs : 0;
    sourceOffsetMs = startOffset + episode.source.sourceDurationMs;
    const offsetKnown = mappingKnown && [startOffset, sourceOffsetMs, episode.source.sourceDurationMs].every(finiteTime)
      && startOffset >= 0 && episode.source.sourceDurationMs > 0;
    const attributed = events.filter(event => offsetKnown && event.nonempty && event.sessionId === episode.logicalRunId &&
      event.timingKnown && Number.isFinite(event.sourceStartSeconds) &&
      Number.isFinite(event.sourceDurationSeconds) && event.sourceDurationSeconds > 0 &&
      finiteTime(event.sourceStartSeconds * 1000) && finiteTime((event.sourceStartSeconds + event.sourceDurationSeconds) * 1000) &&
      event.sourceStartSeconds * 1000 >= startOffset &&
      (event.sourceStartSeconds + event.sourceDurationSeconds) * 1000 <= sourceOffsetMs);
    const intentClockKnown = !(index > 0 && episode.source.continuousCapture);
    attributed.sort((a, b) => a.atMs - b.atMs);
    const first = attributed[0];
    const partial = first?.stable ? undefined : attributed.find(event => !event.stable);
    const stable = attributed.filter(event => event.stable);
    const lastStable = stable[stable.length - 1];
    const { nativeSourceStartMs: sourceStart, nativeSourceEndMs: sourceEnd } = episode.source;
    const sourceValid = sourceStart !== undefined && sourceEnd !== undefined && finiteTime(sourceStart) &&
      finiteTime(sourceEnd) && sourceStart >= 0 && sourceEnd >= sourceStart;
    const latency = (event: Pick<ContinuationObservation, 'atMs'> | undefined, origin: number | undefined) => {
      if (!sourceValid || !event || !calibration.offset || origin === undefined || !finiteTime(origin) ||
        event.atMs < probes[2].w1 || event.atMs > probes[3].w0) return null;
      const native = subtractIntervals([event.atMs, event.atMs], calibration.offset);
      return native ? subtractIntervals(native, [origin, origin]) : null;
    };
    const residual = latency(lastStable, episode.source.nativeSourceEndMs);
    return { episode: episode.episode, logicalRunId: episode.logicalRunId,
      sourceStartOffsetMs: offsetKnown ? startOffset : null, sourceEndOffsetMs: offsetKnown ? sourceOffsetMs : null,
      clock: 'native-process-monotonic-observation', sourceBoundary: 'native-source',
      nativeSourceStartMs: episode.source.nativeSourceStartMs ?? null,
      nativeSourceEndMs: episode.source.nativeSourceEndMs ?? null,
      firstTextMs: latency(first, episode.source.nativeSourceStartMs),
      firstPartialMs: latency(partial, episode.source.nativeSourceStartMs),
      firstStableMs: latency(stable[0], episode.source.nativeSourceStartMs),
      lastStableMs: latency(lastStable, episode.source.nativeSourceStartMs),
      intentToFirstTextMs: intentClockKnown && first ? first.atMs - episode.startMs : null,
      residualStableLagMs: residual,
      clampedRemainingStableLagMs: residual?.map(value => Math.max(0, value)) ?? null,
      attribution: attributed.length ? 'asr-source-interval' : 'unproven',
      micReleaseLagMs: episode.micReleasedMs === null ? null : latency({ atMs: episode.micReleasedMs }, episode.source.nativeSourceEndMs) };
  });
}

/** Native process clock only. WebView event reception is deliberately not subtracted. */
export type NativeInsertionTrace = { overflow: boolean; records: Array<{
  logicalRunId: number; deliverySeq: number; copy: boolean; queuedMs: number;
  executorStartMs: number | null; insertionStartMs: number | null;
  insertionEndMs: number | null; endMs: number | null;
  insertionConfirmed: boolean; result: { status: string } | null;
}> };
export function nativeInsertionMetrics(trace: NativeInsertionTrace, events: ContinuationObservation[], eventOverflow = false) {
  const valid = !trace.overflow && !eventOverflow;
  return { valid, clock: 'native-process-monotonic', records: trace.records.map((record, attemptOrdinal) => {
    const correlated = events.flatMap((event, eventId) =>
      event.sessionId === record.logicalRunId && event.deliverySeq === record.deliverySeq ? [eventId] : []);
    return { ...record, attemptOrdinal, eventIds: correlated,
      attemptedInsertion: record.insertionStartMs !== null,
      queueMs: valid && record.executorStartMs !== null ? record.executorStartMs - record.queuedMs : null,
      insertionDurationMs: valid && record.insertionStartMs !== null && record.insertionEndMs !== null
        ? record.insertionEndMs - record.insertionStartMs : null };
  }) };
}

// Synthetic fixture diagnostic only. Occurrences are retained per raw event;
// repeated provider revisions cannot establish causal episode identity.
export function syntheticPhraseOccurrences(text: string) {
  const normalized = text.toLocaleLowerCase('ru').replace(/ё/g, 'е').replace(/[.,!?]/g, '').replace(/\s+/g, ' ');
  const phrases = ['на столе лежит книга', 'за окном растет береза'];
  return phrases.flatMap((phrase, markerId) => {
    const found: Array<{ markerId: number; occurrenceOrdinal: number; characterOffset: number }> = [];
    let offset = 0;
    while (found.length < 64) {
      const position = normalized.indexOf(phrase, offset);
      if (position < 0) break;
      found.push({ markerId, occurrenceOrdinal: found.length, characterOffset: position });
      offset = position + phrase.length;
    }
    return found;
  });
}

/** Opt-in synthetic e2e report only; preserve spelling, whitespace and revisions.
 * The limit counts UTF-16 code units. Truncation invalidates evidence. */
export function boundedSyntheticText(value: unknown) {
  const text = typeof value === 'string' ? value : null;
  const truncated = text !== null && text.length > 4096;
  return { rawSyntheticText: text === null ? null : text.slice(0, 4096),
    syntheticTextTruncated: truncated, syntheticTextValid: !truncated };
}

export type Interval = [number, number];
export type ClockExchange = { phase: 'pre' | 'post'; w0: number; n: number; w1: number };
const finiteTime = (n: number) => Number.isFinite(n) && Math.abs(n) <= Number.MAX_SAFE_INTEGER;
export function subtractIntervals(a: Interval, b: Interval): Interval | null {
  const result: Interval = [a[0] - b[1], a[1] - b[0]];
  return [...a, ...b, ...result].every(finiteTime) && a[0] <= a[1] && b[0] <= b[1] ? result : null;
}
export function clockCalibration(probes: ClockExchange[]) {
  let offset: Interval = [-Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER];
  const assumption = 'constant WebView-minus-native offset; equal clock rates throughout the trial; pre/post agreement does not prove an arbitrary drift bound; no runner clock subtraction';
  if (probes.length !== 6) return { offset: null, assumption, error: 'six pre/post probes required' };
  for (let i = 0; i < probes.length; i++) {
    const p = probes[i], previous = probes[i - 1];
    const bounds = subtractIntervals([p.w0, p.w1], [p.n, p.n]);
    if (!bounds || p.w0 < 0 || p.n < 0 || p.phase !== (i < 3 ? 'pre' : 'post') ||
      (previous && (p.w0 < previous.w1 || p.n < previous.n)))
      return { offset: null, assumption, error: 'nonfinite, overflow, reset or unordered probes' };
    offset = [Math.max(offset[0], bounds[0]), Math.min(offset[1], bounds[1])];
    if (offset[0] > offset[1]) return { offset: null, assumption, error: 'contradictory clock exchanges' };
  }
  return { offset, assumption, error: null };
}
export type NativeReadback = { armed: boolean; stopped: boolean; valid: boolean; error: string | null;
  maxSamplingGapMs: number; records: Array<{ sequence: number; text: string; identityValid: boolean;
    readStartMs: number; readEndMs: number; lastReadStartMs: number; lastReadEndMs: number; samples: number; maxSamplingGapMs: number }> };
export function appearanceBracket(trace: NativeReadback, present: (text: string) => boolean): Interval | null {
  if (!trace.valid || !trace.armed || !trace.stopped || trace.error || !trace.records.length ||
    trace.records.length > 512 || !finiteTime(trace.maxSamplingGapMs) || trace.maxSamplingGapMs < 0 || trace.records[0].text !== '') return null;
  let previousEnd = -Infinity;
  for (const [i, r] of trace.records.entries()) {
    if (r.sequence !== i || !r.identityValid || r.text.length > 4096 || !Number.isSafeInteger(r.samples) || r.samples < 1 ||
      ![r.readStartMs, r.readEndMs, r.lastReadStartMs, r.lastReadEndMs, r.maxSamplingGapMs].every(finiteTime) ||
      r.readStartMs < previousEnd || r.readEndMs < r.readStartMs || r.lastReadStartMs < r.readStartMs ||
      r.lastReadEndMs < Math.max(r.lastReadStartMs, r.readEndMs) || r.maxSamplingGapMs < 0 ||
      (r.samples > 1 && r.lastReadStartMs < r.readEndMs) ||
      (r.samples === 1 && (r.lastReadStartMs !== r.readStartMs || r.lastReadEndMs !== r.readEndMs))) return null;
    previousEnd = r.lastReadEndMs;
  }
  let lastAbsent: number | null = null;
  for (const r of trace.records) {
    if (present(r.text)) return lastAbsent === null ? null : [lastAbsent, r.readEndMs];
    lastAbsent = r.lastReadStartMs;
  }
  return null;
}
export function syntheticReadbackMetrics(trace: NativeReadback, episodes: ContinuationEpisode[], exactStableText: string) {
  const predicates = [
    { label: 'any nonempty text', predicate: (text: string) => text.length > 0 },
    ...['на столе', 'за окном'].map((prefix, markerId) => ({ label: `empirical ${markerId ? 'B' : 'A'} prefix`,
      predicate: (text: string) => text.toLocaleLowerCase('ru').includes(prefix) })),
    ...[0, 1].map(markerId => ({ label: `empirical ${markerId ? 'B' : 'A'} full sentence marker`,
      predicate: (text: string) => syntheticPhraseOccurrences(text).some(row => row.markerId === markerId) })),
    { label: 'exact cumulative stable text', predicate: (text: string) => exactStableText.length > 0 && text === exactStableText },
  ];
  return { attribution: 'unproven', limitation: 'first observed appearance, not paint or causal ASR timing; full sentence is not earliest B word; transients between reads can be missed',
    maxSamplingGapMs: trace.maxSamplingGapMs,
    observations: predicates.map(({ label, predicate }) => {
      const bracket = appearanceBracket(trace, predicate);
      return { label, bracket, sourceComparisons: episodes.map(episode => {
        const { nativeSourceStartMs: start, nativeSourceEndMs: end } = episode.source;
        const valid = start !== undefined && end !== undefined && finiteTime(start) && finiteTime(end) && start >= 0 && end >= start;
        return { episode: episode.episode,
          fromSourceStartMs: bracket && valid ? subtractIntervals(bracket, [start, start]) : null,
          residualFromSourceEndMs: bracket && valid ? subtractIntervals(bracket, [end, end]) : null };
      }) };
    }) };
}
