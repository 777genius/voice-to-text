import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { stripTypeScriptTypes } from 'node:module';
import { verifyPreparationArtifact, parseArguments, executionEnvironment, validatePreparationResult, validateResult, createQualificationCollector } from '../run-native-window-e2e.mjs';
const source = await readFile(new URL('../../src/e2e/nativeReaderPreparation.ts', import.meta.url), 'utf8');
const body = stripTypeScriptTypes(source.replace(/^import .*;\n/gm, '')).replace(/export /g, '');
const liveSource = await readFile(new URL('../../src/e2e/nativeContinuationLive.ts', import.meta.url), 'utf8');
const liveBody = stripTypeScriptTypes(liveSource.replace(/^import .*;\n/gm, '')).replace(/export /g, '');
const directory = '/tmp/voicetext-native-e2e-Unit';
function envelope() {
  return { diagnosticEffectRefused: false, marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', readerPreparation: true, qualificationPassed: false,
    passed: true, readerWorkerJoined: true, ownedPath: `${directory}/p4-textedit-a.txt`,
    report: { preflightComplete: true, unexpectedEvents: 0, mode: 'reader-preparation', qualificationPassed: false, passed: true, errors: [], observationMs: 5000,
      targetDocument: 'p4-textedit-a.txt', clockExchanges: Array.from({length: 3}, () => ({phase: 'pre'})) },
    fixture: { captureStarts: 0, livePcmBytes: 0, providerPcmBytes: 0, providerStarts: 0, providerResumes: 0,
      activeCaptures: 0, activeProviders: 0, audioChunks: 0, providerAudioChunks: 0, intentObservations: [], observationOverflow: false },
    nativeReadback: { armed: true, stopped: true, valid: true, records: [{text: '', identityValid: true}],
      identity: { path: `${directory}/p4-textedit-a.txt`, bundle: 'com.apple.TextEdit', pid: 123 } } };
}
test('explicit unpaid CLI has no backend/key/trial requirement and rejects mixed modes', () => {
  for (const args of [['--reader-preparation'], ['--reuse-build', directory, '--reader-preparation']]) {
    const options = parseArguments(args), env = executionEnvironment(directory, options);
    assert.equal(options.readerPreparation, true);
    assert.equal(env.VOICETEXT_NATIVE_READER_PREPARATION, 'unpaid-v1');
    for (const key of ['VOICETEXT_NATIVE_CONTINUATION', 'VOICETEXT_QUALIFICATION_ENDPOINT', 'VOICETEXT_NATIVE_LIVE']) assert.equal(env[key], undefined);
    assert.equal(options.harnessConfig, undefined);
  }
  for (const args of [['--reader-preparation', '--qualification-live'], ['--reuse-build', 'relative', '--reader-preparation'], ['--reader-preparation', '--reader-preparation']]) assert.throws(() => parseArguments(args));
  assert.throws(() => executionEnvironment(directory, { readerPreparation: true, trialId: 'warm-baseline-1' }));
});
test('separate readiness cannot pass qualification and rejects every side effect and identity/join failure', async () => {
  validatePreparationResult(envelope(), directory);
  assert.throws(() => validateResult(envelope()));
  await assert.rejects(createQualificationCollector({id: 'warm-baseline-1'}, [], async () => envelope(), () => 0)(() => true));
  for (const key of ['captureStarts', 'livePcmBytes', 'providerPcmBytes', 'providerStarts', 'providerResumes', 'activeCaptures', 'activeProviders', 'audioChunks', 'providerAudioChunks']) {
    const e = envelope(); e.fixture[key] = 1; assert.throws(() => validatePreparationResult(e, directory));
    delete e.fixture[key]; assert.throws(() => validatePreparationResult(e, directory));
  }
  for (const mutate of [e => e.fixture.intentObservations.push({}), e => e.readerWorkerJoined = false,
    e => e.nativeReadback.identity.path += '.other', e => e.nativeReadback.valid = false,
    e => e.nativeReadback.shutdownError = 'join timeout', e => e.report.errors.push('native fatal'),
    e => e.diagnosticEffectRefused = true, e => e.report.preflightComplete = false, e => e.report.unexpectedEvents = 1,
    e => e.report.qualificationPassed = true, e => e.report.observationMs = NaN]) {
    const e = envelope(); mutate(e); assert.throws(() => validatePreparationResult(e, directory));
  }
});
for (const fault of ['none', 'prepare', 'invalid', 'calibration', 'fresh', 'observe', 'shutdown', 'stop-rpc', 'fatal-and-shutdown', 'preflight-listen', 'preflight-config', 'preflight-permission', 'preflight-event']) {
  test(`orchestrator ${fault}: bounded observation and unconditional stop/finish`, async () => {
    let t = 100, reads = 0, report, listened = 0, unlistened = 0; const commands = [], delays = [];
    const reader = { armed: true, valid: true, stopped: false, error: null, records: [{text: ''}], diagnostics: {firstFatal: 'original'} };
    const invoke = async (command, args) => {
      commands.push(command);
      if (command === 'native_e2e_prepare_live_target') { if (fault === 'prepare') throw Error('prepare fatal'); return 'p4-textedit-a.txt'; }
      if (command === 'native_e2e_delay') { delays.push(args.durationMs); t += args.durationMs; }
      if (command === 'native_e2e_state') {
        reads++;
        if ((fault === 'invalid' && reads === 2) || (fault === 'fresh' && reads === 6) ||
            (fault === 'observe' && reads === 7) || (fault === 'fatal-and-shutdown' && reads === 2)) { reader.valid = false; reader.error = 'first native fatal'; }
        if (args.stopReadback) {
          if (fault === 'stop-rpc') throw Error('stop RPC failed');
          reader.stopped = true;
          if (['shutdown', 'fatal-and-shutdown'].includes(fault)) { reader.valid = false; reader.error ??= 'join timeout'; reader.shutdownError = 'join timeout'; }
        }
        return {readerPreparation: true, nativeClockMs: fault === 'calibration' && reads === 4 ? NaN : t, nativeReadback: structuredClone(reader)};
      }
      if (command === 'update_app_config' && fault === 'preflight-config') throw Error('config failed');
      if (command === 'check_accessibility_permission') return fault !== 'preflight-permission';
      if (command === 'native_e2e_finish') report = args.report;
    };
    const run = new Function('invoke', 'performance', 'listen', 'useAppConfigStore', 'useTranscriptionStore', `${liveBody}; ${body}; return runNativeReaderPreparation;`)(invoke, {now: () => t}, async (name, handler) => { listened++; if (fault === 'preflight-listen' && listened === 3) throw Error('listen failed'); if (fault === 'preflight-event') handler({payload: {text: 'MUST NOT STORE'}}); return () => unlistened++; }, () => ({ startSync: async () => {}, refresh: async () => {} }), () => ({}));
    await run();
    assert.equal(unlistened, fault === 'preflight-listen' ? 2 : 4);
    assert.equal(JSON.stringify(report).includes('MUST NOT STORE'), false);
    if (fault.startsWith('preflight-')) assert.equal(commands.includes('native_e2e_prepare_live_target'), false);
    assert.equal(report.passed, fault === 'none'); assert.equal(report.qualificationPassed, false);
    assert.equal(commands.at(-1), 'native_e2e_finish');
    assert.ok(commands.every(c => ['native_e2e_state', 'native_e2e_delay', 'native_e2e_prepare_live_target', 'native_e2e_finish', 'update_app_config', 'native_e2e_configure', 'check_accessibility_permission'].includes(c)));
    if (fault !== 'stop-rpc') { assert.equal(report.final.nativeReadback.stopped, true); assert.deepEqual(report.final.nativeReadback.diagnostics, {firstFatal: 'original'}); }
    else assert.ok(report.errors.some(e => e.includes('stop RPC failed')));
    if (fault === 'none') { assert.deepEqual(delays, [500, 5000]); assert.equal(report.clockExchanges.length, 3); }
    if (fault === 'fatal-and-shutdown') assert.ok(report.errors[0].includes('first native fatal'));
    if (['invalid', 'calibration', 'fresh'].includes(fault)) assert.equal(delays.includes(5000), false);
  });
}

for (const outcome of ['missing', 'malformed', 'invalid']) {
  test(`runner persists ${outcome} envelope failure and original runtime error`, async () => {
    const dir = await mkdtemp(path.join(os.tmpdir(), 'voicetext-preparation-failure-'));
    const result = path.join(dir, 'native-result.json');
    const bytes = outcome === 'malformed' ? '{broken' : '{}';
    try {
      if (outcome !== 'missing') await writeFile(result, bytes, {flag: 'wx'});
      await assert.rejects(verifyPreparationArtifact(dir, result, new Error('native runtime stopped')), /native runtime stopped/);
      const evidence = JSON.parse(await readFile(path.join(dir, 'reader-preparation-verification.json'), 'utf8'));
      assert.equal(evidence.passed, false);
      assert.equal(evidence.qualificationPassed, false);
      assert.equal(evidence.resultPath, result);
      assert.equal(evidence.errors.length, 2);
      assert.match(evidence.errors[0], /native runtime stopped/);
      if (outcome !== 'missing') assert.equal(await readFile(result, 'utf8'), bytes);
    } finally { await rm(dir, {recursive: true, force: true}); }
  });
}
