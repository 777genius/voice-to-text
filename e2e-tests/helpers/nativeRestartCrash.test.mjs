import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { parseArguments, runOwned, executionEnvironment } from '../run-native-window-e2e.mjs';
import { validateRestartCheckpoint, runRestartCrash } from './nativeRestartCrash.mjs';
const directory = '/tmp/e54-test';
function checkpoint(phase = 'A', pid = 123) {
  return { marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', report: { mode: 'restart-crash', passed: true, errors: [],
    ui: { error: null, configSynced: true, reconciledStatus: 'Idle', status: 'Idle', desiredOn: false, pendingStart: false, canRequestContinuation: false } }, state: {
    processPid: pid, processEpoch: `${pid === 123 ? 'a' : 'b'}`.repeat(36), configDir: directory, resultPath: '/tmp/result.json',
    continuationCase: 'E54', restartPhase: phase, liveMode: false, continuationMode: true,
    sessionId: 0, historyEntryCount: 0, status: 'Idle', desiredOn: false, pendingStart: false, continuationPending: false, preparedCaptureTokenCount: 0,
    sentinelLoaded: true, persistedTargetEligible: false, captureEpisode: null, logicalProviderRunId: 0,
    pausedContinuation: phase === 'A' ? { logicalRunId: 1, pauseEpoch: 1 } : null,
    fixture: { observationOverflow: false, markerViolations: [], captureStarts: phase === 'A' ? 1 : 0,
      captureStops: phase === 'A' ? 1 : 0, activeCaptures: 0, audioChunks: 0, activeProviders: phase === 'A' ? 1 : 0,
      providerStarts: phase === 'A' ? 1 : 0, providerStops: 0, providerResumes: 0,
      providerPcmBytes: 0, livePcmBytes: 0, finals: 0, autoPastes: 0, providerFailures: 0,
      intentObservations: [], fullPcm: [], sourceEpisodes: [], captureEvents: [], captureRunAssociations: [],
      providerAudioChunks: phase === 'A' ? 1 : 0, captureMarkers: [], providerMarkers: [], firstBWrites: [],
      controlResults: phase === 'A' ? [{ operation: 'pause', delivered: true, result: { decision: 'accepted' } }] : [] } } };
}
const expected = phase => ({ phase, pid: phase === 'A' ? 123 : 456, directory, resultPath: '/tmp/result.json', previous: checkpoint().state });
test('E54 is an explicit unpaid case and rejects extra arguments', () => {
  assert.deepEqual(parseArguments(['--continuation-case', 'E54']), { continuationFake: true, continuationCase: 'E54' });
  assert.throws(() => parseArguments(['--continuation-case', 'E54', 'extra']));
  assert.equal(executionEnvironment(directory, parseArguments(['--continuation-case', 'E54'])).VOICETEXT_NATIVE_CONTINUATION, 'p4-fake-v1');
});
test('exact checkpoint rejects missing paused context, fake state and retained failure', () => {
  validateRestartCheckpoint(checkpoint(), expected('A'));
  for (const mutate of [e => e.state.processPid++, e => e.state.pausedContinuation = null,
    e => e.report.errors.push('late failure'), e => e.state.fixture.controlResults = [],
    e => e.state.fixture.activeCaptures = 1, e => e.state.configDir = '/user', e => e.state.liveMode = true,
    e => e.state.resultPath = '/old', e => e.state.fixture.observationOverflow = true]) {
    const e = checkpoint(); mutate(e); assert.throws(() => validateRestartCheckpoint(e, expected('A')));
  }
});
test('fresh restart rejects replay, same process epoch and persisted eligibility', () => {
  validateRestartCheckpoint(checkpoint('B', 456), expected('B'));
  for (const mutate of [e => e.state.processEpoch = checkpoint().state.processEpoch,
    e => e.state.sentinelLoaded = false, e => delete e.state.sentinelLoaded,
    e => e.state.persistedTargetEligible = true, e => e.state.pendingStart = true,
    e => e.state.continuationPending = true, e => e.state.fixture.audioChunks = 1,
    e => e.state.preparedCaptureTokenCount = 1, e => e.state.fixture.providerMarkers.push({}),
    e => e.report.ui.error = 'listener registration failed', e => delete e.report.ui.error,
    e => e.report.ui.configSynced = false, e => e.report.ui.reconciledStatus = null,
    e => delete e.report.ui.reconciledStatus, e => e.report.ui.canRequestContinuation = true, e => e.state.pausedContinuation = {}]) {
    const e = checkpoint('B', 456); mutate(e); assert.throws(() => validateRestartCheckpoint(e, expected('B')));
  }
});
test('runOwned crashes only owned detached leader and proves group disappearance', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'e54-owned-test-'));
  try {
    const termination = path.join(dir, 'termination.json');
    await runOwned(process.execPath, ['-e', 'setInterval(()=>{},1000)'], { cwd: dir }, 2000,
      path.join(dir, 'log'), undefined, async (alive, pid) => { assert(alive()); assert(pid !== process.pid); return true; }, termination, true);
    const t = JSON.parse(await readFile(termination, 'utf8'));
    assert.equal(t.signal, 'SIGKILL'); assert.equal(t.groupGone, true); assert.equal(t.exited, true);
    assert.equal(t.checkpointCollected, true); assert.equal(t.failure, null);
    assert.throws(() => process.kill(-t.pid, 0), { code: 'ESRCH' });
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('failure checkpoint preserves primary error and termination evidence', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'e54-failure-test-'));
  try {
    const termination = path.join(dir, 'termination.json');
    await assert.rejects(runOwned(process.execPath, ['-e', 'setInterval(()=>{},1000)'], { cwd: dir }, 2000,
      path.join(dir, 'log'), undefined, async () => { throw new Error('rejected checkpoint'); }, termination, true), /rejected checkpoint/);
    const t = JSON.parse(await readFile(termination, 'utf8'));
    assert.match(t.failure, /rejected checkpoint/); assert.equal(t.checkpointCollected, false); assert(t.groupGone);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('two phases reuse config and HOME, rotate result and retain failure artifact', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'e54-sequence-test-'));
  try {
    await writeFile(path.join(dir, 'stt_config.json'), '{}');
    const env = executionEnvironment(dir, { continuationFake: true, continuationCase: 'E54' });
    const calls = [];
    const runner = async (_binary, _args, options, _timeout, _log, _progress, collect, term, crash) => {
      calls.push(options.env);
      const phase = options.env.VOICETEXT_NATIVE_RESTART_PHASE;
      const e = checkpoint(phase, phase === 'A' ? 123 : 456);
      e.state.configDir = dir; e.state.resultPath = options.env.VOICE_TO_TEXT_NATIVE_E2E_RESULT;
      if (phase === 'B') {
        const persisted = JSON.parse(await readFile(path.join(dir, 'stt_config.json'), 'utf8'));
        assert.equal(persisted.continuation_target_eligible, true);
        assert.equal(persisted.language, 'e54-restart-sentinel');
      }
      await writeFile(e.state.resultPath, JSON.stringify(e));
      assert.equal(await collect(() => true, e.state.processPid), true);
      await writeFile(term, JSON.stringify({ pid: e.state.processPid, signal: crash ? 'SIGKILL' : 'SIGTERM', exited: true, groupGone: true, checkpointCollected: true, failure: null }));
    };
    assert.equal((await runRestartCrash('fake', dir, env, runner)).passed, true);
    assert.equal(calls[0].HOME, calls[1].HOME);
    assert.equal(calls[0].VOICE_TO_TEXT_CONFIG_DIR, calls[1].VOICE_TO_TEXT_CONFIG_DIR);
    assert.notEqual(calls[0].VOICE_TO_TEXT_NATIVE_E2E_RESULT, calls[1].VOICE_TO_TEXT_NATIVE_E2E_RESULT);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
test('runner failure cannot be overridden by a result', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'e54-evidence-test-'));
  try {
    await assert.rejects(runRestartCrash('fake', dir, {}, async () => { throw new Error('native crash failed'); }), /native crash failed/);
    const v = JSON.parse(await readFile(path.join(dir, 'restart-verification.json'), 'utf8'));
    assert.equal(v.passed, false); assert.match(v.errors[0], /native crash failed/);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
