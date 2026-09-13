import { afterEach, expect, test, vi } from 'vitest';
import { ref } from 'vue';

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllEnvs(); vi.resetModules(); });

test('selected sample provenance survives Vue array copying and deduplicates repeated draws', async () => {
  vi.stubEnv('VITE_NATIVE_WINDOW_E2E', '1');
  let wall = 1000; let mono = 0;
  vi.spyOn(Date, 'now').mockImplementation(() => wall);
  vi.spyOn(performance, 'now').mockImplementation(() => mono);
  const evidence = await import('./nativeMeterEvidence');
  const old = [0.1]; const selected = [0.8];
  evidence.tagNativeMeterSource(old, {run_id: 1, capture_generation: 1, source_timestamp_ms: 800});
  evidence.tagNativeMeterSource(selected, {run_id: 2, capture_generation: 2, source_timestamp_ms: 990});
  const drawn = ref([0.7]);
  evidence.carryNativeMeterTrace(selected, drawn.value);
  wall = 1040; mono = 40;
  evidence.recordNativeMeterRender(drawn.value);
  evidence.recordNativeMeterRender(drawn.value);
  const result = evidence.nativeMeterEvidence();
  expect(result.samples).toBe(1);
  expect(result.rows).toEqual([{runId: 2, sampleAgeMs: 50, receiveToRenderMs: 40, clockValid: true}]);
});

test('wall clock discontinuity invalidates a sample instead of producing reassuring latency', async () => {
  vi.stubEnv('VITE_NATIVE_WINDOW_E2E', '1');
  let wall = 1000; let mono = 0;
  vi.spyOn(Date, 'now').mockImplementation(() => wall);
  vi.spyOn(performance, 'now').mockImplementation(() => mono);
  const evidence = await import('./nativeMeterEvidence');
  const bars = [0.1];
  evidence.tagNativeMeterSource(bars, {run_id: 1, capture_generation: 1, source_timestamp_ms: 990});
  wall = 900; mono = 10;
  evidence.recordNativeMeterRender(bars);
  expect(evidence.nativeMeterEvidence().invalidClockSamples).toBe(1);
  expect(evidence.nativeMeterEvidence().p95AgeMs).toBeNull();
});
