import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, readFile, lstat, rm, symlink, realpath } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { closeOwnedDocument, ownedClosureScript, ownedDocumentMatches, textEditPathAliases } from './nativeOwnedDocument.mjs';

for (const scenario of ['success', 'absent', 'no-owner', 'bad-owner', 'changed-inode', 'symlink', 'missing-termination', 'alive', 'different-pid', 'script-error', 'unverified']) {
  test(`owned closure ${scenario}: exact identity and termination required; independent evidence`, async () => {
    const dir = await realpath(await mkdtemp(path.join(os.tmpdir(), 'voicetext-owned-cleanup-')));
    const target = path.join(dir, 'p4-textedit-a.txt');
    const unrelated = path.join(dir, 'user.txt');
    let calls = 0;
    try {
      await writeFile(unrelated, 'USER DATA UNTOUCHED');
      await writeFile(target, 'owned final insertion');
      const info = await lstat(target, { bigint: true });
      const directoryInfo = await lstat(dir, { bigint: true });
      const owner = { marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', path: target, appPid: 123, device: String(info.dev), inode: String(info.ino),
        directoryDevice: String(directoryInfo.dev), directoryInode: String(directoryInfo.ino), directoryMode: Number(directoryInfo.mode), directoryUid: Number(directoryInfo.uid) };
      if (scenario === 'bad-owner') owner.path = unrelated;
      if (scenario === 'changed-inode') owner.inode = '0';
      if (scenario === 'symlink') { await rm(target); await symlink(unrelated, target); }
      if (scenario !== 'no-owner') await writeFile(path.join(dir, 'owned-document.json'), JSON.stringify(owner));
      if (scenario !== 'missing-termination') await writeFile(path.join(dir, 'native-process-termination.json'), JSON.stringify({
        pid: scenario === 'different-pid' ? 456 : 123, exited: true, groupGone: scenario !== 'alive' }));
      const execute = async (command, args, options) => {
        calls++;
        assert.equal(command, '/usr/bin/osascript');
        assert.equal(args[1], ownedClosureScript(target));
        assert.equal(options.timeout, 5000);
        assert.equal(options.maxBuffer, 8192);
        if (scenario === 'script-error') throw Error('TextEdit denied close');
        return { stdout: scenario === 'unverified' ? 'still-open' : 'absent\n' };
      };
      const success = ['success', 'absent', 'no-owner', 'changed-inode'].includes(scenario);
      if (success) assert.equal((await closeOwnedDocument(dir, execute)).passed, true);
      else await assert.rejects(closeOwnedDocument(dir, execute));
      const evidence = JSON.parse(await readFile(path.join(dir, 'owned-document-cleanup.json'), 'utf8'));
      assert.equal(evidence.passed, success);
       assert.equal(evidence.attempted, ['success', 'absent', 'changed-inode', 'script-error', 'unverified'].includes(scenario));
      assert.equal(calls, Number(evidence.attempted));
      assert.equal(await readFile(unrelated, 'utf8'), 'USER DATA UNTOUCHED');
      assert.ok(evidence.endMs >= evidence.startMs);
      assert.equal(JSON.stringify(evidence).includes('USER DATA'), false);
    } finally { await rm(dir, { recursive: true, force: true }); }
  });
}
test('closure script never reads text, quits TextEdit, uses front document, or restores clipboard', () => {
  const script = ownedClosureScript('/tmp/TEST path/p4-textedit-a.txt');
  assert.match(script, /every document whose path is/);
  assert.match(script, /close item 1 of matches saving no/);
  assert.match(script, /repeat 20 times/);
  assert.match(script, /set matches to missing value/);
  assert.match(script, /count remainingMatches\) is 0 then return "absent"/);
  assert.doesNotMatch(script, /\b(?:clipboard|quit|front document|text of|name of)\b/);
});
test('TextEdit identity accepts only the canonical TEST path and its macOS private alias', () => {
  assert.deepEqual(textEditPathAliases('/private/var/TEST/p4-textedit-a.txt'), [
    '/private/var/TEST/p4-textedit-a.txt', '/var/TEST/p4-textedit-a.txt',
  ]);
  assert.deepEqual(textEditPathAliases('/Users/test/p4-textedit-a.txt'), ['/Users/test/p4-textedit-a.txt']);
  const matches = ownedDocumentMatches('/private/var/TEST/p4-textedit-a.txt');
  assert.match(matches, /whose path is "\/private\/var\/TEST\/p4-textedit-a\.txt"/);
  assert.match(matches, /whose path is "\/var\/TEST\/p4-textedit-a\.txt"/);
  assert.doesNotMatch(matches, /front document|text of|name of/);
});

// Execute the real parent main with only its OS/provider boundaries replaced.
const runner = await readFile(new URL('../run-native-window-e2e.mjs', import.meta.url), 'utf8');
const mainBody = runner.slice(runner.indexOf('export async function main('), runner.indexOf('\nif (process.argv[1]'))
  .replace('export ', '').replace("await import('./helpers/nativeContinuationProxy.mjs')", '({ startConfigDelayProxy: mockProxy })');
for (const mode of ['diagnostic', 'live']) for (const failure of ['none', 'setup', 'runtime', 'verification', 'cleanup', 'runtime-and-cleanup']) {
  test(`parent ${mode}/${failure}: cleanup every exit, readback before closure, original error retained`, async () => {
    const calls = [];
    const directory = '/TEST/owned';
    const trial = { id: 'warm-baseline-1', configDelayMs: 0, continuation: false };
    const options = { reuseBuild: '/TEST/build', readerPreparation: mode === 'diagnostic',
      ...(mode === 'live' ? { trialId: trial.id, harnessConfig: '/TEST/config' } : {}) };
    const envelope = { marker: 'test-marker', passed: true, report: { passed: true, errors: [], trial,
      expectedInsertion: 'synthetic', targetDocument: 'p4-textedit-a.txt', final: { fixture: {} } } };
    const dependencies = {
      path, process: { platform: 'darwin', argv: [], on() {}, removeListener() {} }, source: '/TEST/source', performance,
      parseArguments: () => options, prepareReuseBuild: async () => directory,
      executionEnvironment: () => ({ HOME: '/TEST/home', VOICE_TO_TEXT_NATIVE_E2E_RESULT: '/TEST/result' }),
      mkdir: async () => { if (failure === 'setup') throw Error('primary setup'); },
      liveTrials: [trial], realpath: async p => p, lstat: async () => ({ isFile: () => true, mode: 0o600 }),
      readFile: async p => p === '/TEST/config' ? '{}' : JSON.stringify(envelope),
      writeFile: async p => calls.push(path.basename(p)), readApprovedFixtures: async () => [],
      validateHarnessConfig: () => ({ endpoint: 'disabled' }), validateReusableBuild: async () => {}, validateCachedBinary: async () => '/TEST/binary',
      mockProxy: async () => ({ url: 'disabled', close: async () => calls.push('proxy-close') }),
      createQualificationCollector: () => undefined, randomUUID: () => 'fixed',
      runOwned: async () => { calls.push('terminated'); if (failure.startsWith('runtime')) throw Error('primary runtime'); },
      verifyPreparationArtifact: async (_directory, _result, runtimeFailure) => { if (runtimeFailure) throw runtimeFailure; calls.push('verify'); if (failure === 'verification') throw Error('primary verification'); },
      promisify: fn => fn, execFile: async () => { calls.push('readback'); return { stdout: 'synthetic\n' }; },
      exactInsertionEvidence: () => { if (failure === 'verification') throw Error('primary verification'); return {}; },
      verifyQualificationConnections: () => ({}), verifyQualificationRoute: () => ({}),
      verifyQualificationSources: () => {}, verifyQualificationTerminals: () => {},
      ownedDocumentMatches,
      closeOwnedDocument: async () => { calls.push('close'); if (failure.includes('cleanup')) throw Error('cleanup failure'); },
      marker: 'test-marker', console: { log() {} },
    };
    const run = new Function(...Object.keys(dependencies), `${mainBody}; return main;`)(...Object.values(dependencies));
    if (failure === 'none') await run([]);
    else await assert.rejects(run([]), error => {
      if (failure === 'runtime-and-cleanup') {
        assert.equal(error.errors[0].message, 'primary runtime');
        assert.equal(error.cause.message, 'primary runtime');
        assert.equal(error.errors[1].message, 'cleanup failure');
      } else assert.match(error.message, new RegExp(failure === 'cleanup' ? 'cleanup failure' : `primary ${failure}`));
      return true;
    });
    assert.equal(calls.filter(c => c === 'close').length, 1);
    assert.equal(calls.at(-1), 'close');
    if (calls.includes('readback')) assert.ok(calls.indexOf('terminated') < calls.indexOf('readback'));
    if (mode === 'live' && ['none', 'cleanup', 'verification'].includes(failure)) assert.ok(calls.includes('readback'));
  });
}

test('parent live verification accepts each planned long Continue and rejects a missing acceptance', async () => {
  const trial = { id: 'long', continuation: true, configDelayMs: 0,
    episodes: ['episode-a.pcm', 'episode-b.pcm', 'long-auto-commit.pcm', 'episode-b.pcm'] };
  for (const acceptedCount of [3, 2]) {
    const artifacts = {};
    let observe;
    const dependencies = {
      path, process: { platform: 'darwin', argv: [], on() {}, removeListener() {} }, source: '/TEST/source', performance,
      parseArguments: () => ({ reuseBuild: '/TEST/build', trialId: trial.id, harnessConfig: '/TEST/config' }),
      prepareReuseBuild: async () => '/TEST/artifacts', executionEnvironment: () => ({ HOME: '/TEST/home', VOICE_TO_TEXT_NATIVE_E2E_RESULT: '/TEST/result' }),
      mkdir: async () => {}, liveTrials: [trial], realpath: async p => p, lstat: async () => ({ isFile: () => true, mode: 0o600 }),
      readFile: async p => p === '/TEST/config' ? '{}' : JSON.stringify({ marker: 'test-marker', passed: true,
        report: { passed: true, errors: [], trial, expectedInsertion: 'synthetic', targetDocument: 'p4-textedit-a.txt', final: { fixture: {} } } }),
      writeFile: async (p, data) => { artifacts[path.basename(p)] = data; }, readApprovedFixtures: async () => [],
      validateHarnessConfig: () => ({ endpoint: 'disabled' }), validateReusableBuild: async () => {}, validateCachedBinary: async () => '/TEST/binary',
      mockProxy: async (_endpoint, _delay, callback) => { observe = callback; return { url: 'disabled', close: async () => {} }; },
      maxProxyEvidenceEvents: 32768, createQualificationCollector: () => undefined, randomUUID: () => 'fixed',
      runOwned: async () => { for (let index = 0; index < acceptedCount; index++) observe('backend_control',
        { type: 'continue_result', decision: 'accepted', eligible_now: true }); },
      promisify: fn => fn, execFile: async () => ({ stdout: 'synthetic\n' }),
      exactInsertionEvidence: () => ({}), verifyQualificationConnections: () => ({}), verifyQualificationRoute: () => ({}),
      verifyQualificationSources: () => {}, verifyQualificationTerminals: () => {}, ownedDocumentMatches,
      closeOwnedDocument: async () => {}, marker: 'test-marker', console: { log() {} },
    };
    const run = new Function(...Object.keys(dependencies), `${mainBody}; return main;`)(...Object.values(dependencies));
    if (acceptedCount === 3) await run([]);
    else await assert.rejects(run([]), /Every planned Continue acceptance required/);
    const verification = JSON.parse(artifacts['qualification-verification.json']);
    assert.equal(verification.passed, acceptedCount === 3);
  }
});

const { EventEmitter } = await import('node:events');
const runOwnedBody = runner.slice(runner.indexOf('export async function runOwned('), runner.indexOf('\nexport async function snapshotDigest')).replace('export ', '');
for (const outcome of ['success', 'failed-exit', 'timeout', 'spawn-error', 'group-retained', 'group-eperm', 'evidence-write-failure']) {
  test(`process owner ${outcome}: termination evidence comes from exit and group disappearance`, async () => {
    const child = Object.assign(new EventEmitter(), { pid: 123, exitCode: null, signalCode: null,
      stdout: new EventEmitter(), stderr: new EventEmitter(), kill: signal => {
        signals.push(`child:${signal}`);
        if (child.exitCode === null && child.signalCode === null) {
          child.signalCode = signal;
          queueMicrotask(() => child.emit('exit', null, signal));
        }
      } });
    const output = Object.assign(new EventEmitter(), { write() {}, end: callback => callback() });
    const signals = [], writes = [];
    const parent = Object.assign(new EventEmitter(), { stdout: { write() {} }, kill: (pid, signal) => {
      assert.equal(pid, -123);
      if (outcome === 'group-eperm') {
        signals.push(signal);
        throw Object.assign(Error('denied'), { code: 'EPERM' });
      }
      if (signal === 0) {
        if (outcome === 'group-retained') return;
        throw Object.assign(Error('gone'), { code: 'ESRCH' });
      }
      signals.push(signal);
      if (child.exitCode === null && child.signalCode === null) {
        child.signalCode = signal;
        queueMicrotask(() => child.emit('exit', null, signal));
      }
    } });
    const spawn = (_command, _args, options) => {
      assert.equal(options.detached, true);
      queueMicrotask(() => {
        if (outcome === 'timeout') return;
        if (outcome === 'spawn-error') { child.pid = undefined; child.emit('error', Error('spawn failed')); return; }
        child.exitCode = ['failed-exit', 'evidence-write-failure'].includes(outcome) ? 1 : 0;
        child.emit('exit', child.exitCode, null);
      });
      return child;
    };
    const writeFile = async (_path, text) => {
      writes.push(JSON.parse(text));
      if (outcome === 'evidence-write-failure') throw Error('evidence disk failed');
    };
    const run = new Function('createWriteStream', 'spawn', 'process', 'path', 'writeFile', 'performance', `${runOwnedBody}; return runOwned;`)
      (() => output, spawn, parent, path, writeFile, performance);
    const promise = run('/TEST/app', [], {}, 5, '/TEST/log', undefined, undefined, '/TEST/termination');
    if (outcome === 'success') await promise;
    else await assert.rejects(promise, error => {
      if (outcome === 'evidence-write-failure') {
        assert.match(error.cause.message, /app failed/);
        assert.match(error.errors[1].message, /evidence disk failed/);
      }
      if (['group-retained', 'group-eperm'].includes(outcome)) {
        assert.match(error.message, /process group did not terminate/);
      }
      return true;
    });
    assert.equal(parent.listenerCount('SIGINT') + parent.listenerCount('SIGTERM'), 0);
    if (outcome === 'spawn-error') assert.equal(writes.length, 0);
    else {
      assert.equal(writes[0].exited, true);
      assert.equal(writes[0].groupGone, !['group-retained', 'group-eperm'].includes(outcome));
      assert.equal(writes[0].pid, 123);
      assert.ok(signals.includes('SIGKILL'));
    }
  });
}

for (const scenario of ['repeated-signals', 'signal-timeout', 'timeout-signal', 'collector-resolve', 'collector-reject', 'collector-reject-before-exit', 'heartbeat-await']) {
  test(`termination is disarmed after teardown: ${scenario}`, async () => {
    const child = Object.assign(new EventEmitter(), { pid: 123, exitCode: null, signalCode: null,
      stdout: new EventEmitter(), stderr: new EventEmitter() });
    const output = Object.assign(new EventEmitter(), { write() {}, end: cb => cb() });
    const signals = [], timers = [], intervals = [];
    const parent = Object.assign(new EventEmitter(), { stdout: { write() {} }, kill: (_pid, signal) => {
      if (signal === 0) throw Object.assign(Error('gone'), { code: 'ESRCH' });
      signals.push(signal);
    } });
    let resolvePending, rejectPending;
    const pending = new Promise((resolve, reject) => { resolvePending = resolve; rejectPending = reject; });
    const schedule = list => (cb, ms) => { const handle = { cb, ms, cleared: false }; list.push(handle); return handle; };
    const clear = handle => { if (handle) handle.cleared = true; };
    const dependencies = { createWriteStream: () => output, spawn: () => child, process: parent, path,
      writeFile: async () => {}, performance, stat: () => pending,
      setTimeout: schedule(timers), clearTimeout: clear, setInterval: schedule(intervals), clearInterval: clear };
    const run = new Function(...Object.keys(dependencies), `${runOwnedBody}; return runOwned;`)(...Object.values(dependencies));
    const collector = scenario.startsWith('collector') ? () => pending : undefined;
    const promise = run('/TEST/app', [], {}, 100, '/TEST/log', scenario === 'heartbeat-await' ? '/TEST/progress' : undefined, collector, '/TEST/termination');
    let callback;
    if (intervals.length) callback = intervals[0].cb();
    if (scenario === 'timeout-signal') timers[0].cb();
    parent.emit('SIGINT'); parent.emit('SIGTERM'); parent.emit('SIGINT'); parent.emit('SIGTERM');
    if (scenario === 'signal-timeout') timers[0].cb();
    assert.equal(signals.filter(s => s === 'SIGTERM').length, 1);
    assert.equal(timers.filter(t => t.ms === 5000).length, 1);
    if (scenario === 'collector-reject-before-exit') { rejectPending(Error('later collector failure')); await callback; }
    child.signalCode = 'SIGTERM'; child.emit('exit', null, 'SIGTERM');
    await assert.rejects(promise, scenario === 'timeout-signal' ? /runtime timeout/ : /Runner interrupted: SIGINT/);
    const retiredSignals = [...signals];
    if (scenario === 'collector-reject') rejectPending(Error('late collector failure'));
    else resolvePending(scenario === 'heartbeat-await' ? { mtimeMs: 0 } : true);
    await callback;
    // Even callbacks already queued when cancelled cannot act on the retired PID.
    for (const timer of [...timers, ...intervals]) { assert.equal(timer.cleared, true); await timer.cb(); }
    assert.deepEqual(signals, retiredSignals);
    assert.equal(timers.filter(t => t.ms === 5000).length, 1);
    assert.equal(parent.listenerCount('SIGINT') + parent.listenerCount('SIGTERM'), 0);
  });
}

const { exactInsertionEvidence } = await import('./nativeContinuation.mjs');
for (const outcome of ['success', 'mismatch', 'read-failure', 'mismatch-cleanup', 'read-failure-cleanup', 'success-cleanup', 'mismatch-evidence']) {
  test(`legacy live ${outcome}: exact readback and retained evidence precede cleanup`, async () => {
    const calls = [], artifacts = {}, messages = [];
    const report = { mode: 'live-elevenlabs', targetDocument: 'p4-textedit-a.txt', finalText: 'synthetic\n', actualPasteVerified: false };
    const dependencies = {
      path, process: { platform: 'darwin', on() {}, removeListener() {} }, source: '/TEST/source', performance,
      parseArguments: () => ({ reuseBuild: '/TEST/build', liveFixturePath: '/TEST/pcm' }),
      prepareReuseBuild: async () => '/TEST/artifacts', executionEnvironment: () => ({ HOME: '/TEST/home', VOICE_TO_TEXT_NATIVE_E2E_RESULT: '/TEST/result' }),
      mkdir: async () => {}, liveTrials: [], readFile: async p => p === '/TEST/result' ? JSON.stringify({ report }) : Buffer.from('pcm'),
      digest: () => '46b449e09435d1694fd118c2c78725fae3be0af414648b16ff005e3bb76cc472',
      writeFile: async (p, bytes) => {
        calls.push(path.basename(p)); artifacts[path.basename(p)] = bytes;
        if (outcome === 'mismatch-evidence' && p.endsWith('live-verification.json')) throw Error('evidence failed');
      },
      validateReusableBuild: async () => {}, validateCachedBinary: async () => '/TEST/binary', randomUUID: () => 'fixed',
      runOwned: async () => calls.push('terminated'), validateResult: () => calls.push('validated'),
      ownedDocumentMatches,
      promisify: fn => fn, execFile: async (command, args, options) => {
        calls.push('readback');
        assert.equal(command, '/usr/bin/osascript');
        assert.match(args[1], /every document whose path is "\/TEST\/artifacts\/p4-textedit-a.txt"/);
        assert.match(args[1], /\(count matches\) is not 1 then error/);
        assert.doesNotMatch(args[1], /front document|clipboard/);
        assert.equal(options.timeout, 5000);
        if (outcome.startsWith('read-failure')) throw Error('read failed');
        return { stdout: outcome.startsWith('mismatch') ? 'wrong\n' : 'synthetic\n\n' };
      }, exactInsertionEvidence,
      closeOwnedDocument: async () => { calls.push('close'); if (outcome.endsWith('cleanup')) throw Error('cleanup failed'); },
      console: { log: text => messages.push(text) },
    };
    const run = new Function(...Object.keys(dependencies), `${mainBody}; return main;`)(...Object.values(dependencies));
    if (outcome === 'success') await run([]);
    else await assert.rejects(run([]), error => {
      const expected = outcome.startsWith('mismatch') ? /insertion does not match/ : outcome.startsWith('read-failure') ? /read failed/ : /cleanup failed/;
      assert.match(error.message, expected);
      if (outcome !== 'success-cleanup' && /cleanup|evidence/.test(outcome)) {
        assert.match(error.errors[0].message, expected); assert.equal(error.cause, error.errors[0]);
        assert.match(error.errors[1].message, /cleanup failed|evidence failed/);
      }
      return true;
    });
    const verification = JSON.parse(artifacts['live-verification.json']);
    assert.equal(verification.passed, outcome.startsWith('success'));
    assert.equal(verification.actualPasteVerified, outcome.startsWith('success'));
    assert.equal(verification.qualificationPassed, false);
    assert.equal(report.actualPasteVerified, false);
    assert.deepEqual(calls.slice(-5), ['terminated', 'validated', 'readback', 'live-verification.json', 'close']);
    assert.equal(messages.some(m => m.includes('exact OS insertion verified')), outcome === 'success');
    if (outcome === 'success') assert.equal(verification.utf8Bytes, Buffer.byteLength(report.finalText));
  });
}
