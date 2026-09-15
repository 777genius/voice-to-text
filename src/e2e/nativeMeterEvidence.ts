import { toRaw } from 'vue';

type Trace = { runId: number; generation: number; capturedAt: number; receivedAt: number; receivedMono: number };
const traces = new WeakMap<number[], Trace>();
const seen = new Set<string>();
const rendered: Array<{runId: number; sampleAgeMs: number; receiveToRenderMs: number; clockValid: boolean}> = [];
const enabled = () => import.meta.env.VITE_NATIVE_WINDOW_E2E === '1';

export function tagNativeMeterSource(bars: number[], payload: {
  run_id: number; capture_generation: number | null; source_timestamp_ms?: number;
}) {
  if (!enabled() || payload.capture_generation === null || !Number.isSafeInteger(payload.source_timestamp_ms)) return;
  traces.set(toRaw(bars), {runId: payload.run_id, generation: payload.capture_generation,
    capturedAt: payload.source_timestamp_ms!, receivedAt: Date.now(), receivedMono: performance.now()});
}

export function carryNativeMeterTrace(source: number[], destination: number[]) {
  if (!enabled()) return;
  const trace = traces.get(toRaw(source));
  if (trace) traces.set(toRaw(destination), trace);
}

/** Called after canvas draw, preserving the sample selected by source throttling. */
export function recordNativeMeterRender(bars: number[]) {
  if (!enabled() || rendered.length >= 10000) return;
  const trace = traces.get(toRaw(bars));
  if (!trace) return;
  const key = `${trace.runId}:${trace.generation}:${trace.capturedAt}`;
  if (seen.has(key)) return;
  seen.add(key);
  const wallElapsed = Date.now() - trace.receivedAt;
  const monoElapsed = performance.now() - trace.receivedMono;
  const sampleAgeMs = Date.now() - trace.capturedAt;
  rendered.push({runId: trace.runId, sampleAgeMs, receiveToRenderMs: wallElapsed,
    clockValid: sampleAgeMs >= 0 && Math.abs(wallElapsed - monoElapsed) <= 25});
}

export function nativeMeterEvidence() {
  const ages = rendered.filter(row => row.clockValid).map(row => row.sampleAgeMs).sort((a,b) => a-b);
  return {clock: 'Rust Unix milliseconds and JS Date.now; wall/monotonic drift checked on receipt-to-render',
    endpoint: 'first canvas draw incorporating each selected source sample',
    samples: rendered.length, runs: new Set(rendered.map(row => row.runId)).size,
    invalidClockSamples: rendered.filter(row => !row.clockValid).length,
    p95AgeMs: ages.length ? ages[Math.ceil(ages.length * 0.95)-1] : null,
    maxAgeMs: ages.length ? ages[ages.length - 1] : null, rows: rendered.slice()};
}
