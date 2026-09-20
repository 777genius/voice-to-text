import test from 'node:test';
import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import os from 'node:os';
import path from 'node:path';
import { snapshotDigest, isolatedTauriConfig, parseArguments, executionEnvironment, sanitizedEnvironment, validateArtifactDirectory, validateCachedBinary, validateResult } from '../run-native-window-e2e.mjs';

const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';

test('refuses arbitrary binary/config flags and relative cached paths', () => {
  for (const args of [['--binary', '/Applications/VoicetextAI.app'], ['--config-dir', '/Users/example'], ['--no-build', '.'], ['--no-build'], ['--no-build', '/tmp/test', '--unsafe']]) {
    assert.throws(() => parseArguments(args), /Usage/);
  }
  assert.deepEqual(parseArguments([]), {});
});

test('does not inherit secrets, runtime overrides, or developer profile', () => {
  const env = sanitizedEnvironment('/tmp/voicetext-native-e2e-AbCd12', { PATH: '/usr/bin', OPENAI_API_KEY: 'secret', NODE_OPTIONS: '--require secret', TAURI_CONFIG: 'override', HOME: '/real/user', VITE_BACKEND_TOKEN: 'token' });
  assert.equal(env.OPENAI_API_KEY, undefined);
  assert.equal(env.NODE_OPTIONS, undefined);
  assert.equal(env.TAURI_CONFIG, undefined);
  assert.equal(env.VITE_BACKEND_TOKEN, undefined);
  assert.equal(env.HOME, '/tmp/voicetext-native-e2e-AbCd12/home');
  assert.equal(env.VITE_E2E, '1');
  assert.equal(env.VITE_NATIVE_WINDOW_E2E, '1');
  assert.equal(env.TAURI_DEBUG, 'true');
  assert.notEqual(env.TMPDIR, env.VOICE_TO_TEXT_CONFIG_DIR);
});

test('real Vite build config and runtime API resolver keep the isolated endpoint without weakening release policy', async () => {
  const directory = await realpath(await mkdtemp(path.join(os.tmpdir(), 'voicetext-native-e2e-')));
  const source = fileURLToPath(new URL('../../', import.meta.url));
  try {
    await mkdir(path.join(directory, 'src/config'), { recursive: true });
    for (const file of ['vite.config.ts', 'src/config/apiBase.ts', 'src/config/runtimeApiBase.ts']) {
      await cp(path.join(source, file), path.join(directory, file));
    }
    await writeFile(path.join(directory, 'package.json'), '{"type":"module"}');
    await symlink(path.join(source, 'node_modules'), path.join(directory, 'node_modules'), 'dir');
    await writeFile(path.join(directory, 'check.mjs'), `
      import { loadConfigFromFile } from 'vite';
      import { createRequire } from 'node:module';
      const { build } = createRequire(import.meta.resolve('vite'))('esbuild');
      const loaded = await loadConfigFromFile({ command: 'build', mode: 'native-window-e2e' });
      const builtApi = JSON.parse(loaded.config.define['import.meta.env.VITE_API_URL']);
      const runtime = await build({ entryPoints: ['src/config/runtimeApiBase.ts'], bundle: true,
        write: false, format: 'esm', define: {
          'import.meta.env.DEV': 'false',
          'import.meta.env.VITE_API_URL': JSON.stringify(builtApi),
          'import.meta.env.TAURI_DEBUG': JSON.stringify(process.env.TAURI_DEBUG || ''),
        } });
      const { API_BASE_URL } = await import('data:text/javascript;base64,' + Buffer.from(runtime.outputFiles[0].text).toString('base64'));
      process.stdout.write(JSON.stringify({ builtApi, runtimeApi: API_BASE_URL }));
    `);
    const env = sanitizedEnvironment(directory, { PATH: process.env.PATH, TAURI_DEBUG: 'false' });
    for (const debug of [true, false]) {
      const childEnv = { ...env };
      if (!debug) delete childEnv.TAURI_DEBUG;
      const result = spawnSync(process.execPath, ['check.mjs'], { cwd: directory, env: childEnv, encoding: 'utf8', timeout: 30_000 });
      assert.equal(result.status, 0, result.stderr || String(result.error));
      const expected = debug ? 'http://127.0.0.1:9' : 'https://api.voicetext.site';
      assert.deepEqual(JSON.parse(result.stdout), { builtApi: expected, runtimeApi: expected });
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('requires owned canonical temp test directory, refuses normal directory and symlink', async () => {
  const temporary = await realpath(os.tmpdir());
  const normal = await mkdtemp(path.join(temporary, 'ordinary-test-'));
  const fixture = await mkdtemp(path.join(temporary, 'voicetext-native-e2e-'));
  const link = `${fixture}Alias`;
  try {
    await assert.rejects(validateArtifactDirectory(normal), /Refusing/);
    assert.equal(await validateArtifactDirectory(fixture), fixture);
    await symlink(fixture, link);
    await assert.rejects(validateArtifactDirectory(link), /Refusing/);
  } finally { await Promise.all([rm(normal, { recursive: true, force: true }), rm(fixture, { recursive: true, force: true }), rm(link, { force: true })]); }
});

test('cached executable requires feature marker and matching checksum; no process is launched', async () => {
  const directory = await realpath(await mkdtemp(path.join(os.tmpdir(), 'voicetext-native-e2e-')));
  try {
    const binary = path.join(directory, 'native-window-e2e');
    const identifier = 'com.voicetotext.app.native-e2e.Contract123';
    const bytes = `${marker}\0${identifier}\0`;
    await writeFile(binary, bytes);
    const sha256 = createHash('sha256').update(bytes).digest('hex');
    const snapshot = path.join(directory, 'frontend');
    await mkdir(snapshot);
    await writeFile(path.join(snapshot, 'source.rs'), 'fixture source');
    const manifest = { marker, binary: 'native-window-e2e', sha256, identifier, sourceSha256: await snapshotDigest(snapshot) };
    await writeFile(path.join(directory, 'native-build.json'), JSON.stringify(manifest));
    assert.equal(await validateCachedBinary(directory), binary);
    await writeFile(path.join(snapshot, 'source.rs'), 'changed source');
    await assert.rejects(validateCachedBinary(directory), /source snapshot hash/);
    await writeFile(path.join(snapshot, 'source.rs'), 'fixture source');
    await writeFile(path.join(directory, 'native-build.json'), JSON.stringify({ ...manifest, identifier: 'com.voicetotext.app.native-e2e.OtherBuild' }));
    await assert.rejects(validateCachedBinary(directory), /identity mismatch/);
    await writeFile(path.join(directory, 'native-build.json'), JSON.stringify(manifest));
    await writeFile(binary, 'production application');
    await assert.rejects(validateCachedBinary(directory), /hash\/feature marker/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('passing envelope requires full non-skipped wall time, distinct cases, balanced capture', () => {
  const valid = { marker, passed: true, fixture: { captureStarts: 35, captureStops: 35, activeCaptures: 0, activeProviders: 0 }, report: { passed: true, completedCycles: 22, hiddenIdleMs: 180000, elapsedMs: 220000, scenarios: Array.from({ length: 12 }, (_, i) => `scenario-${i}`) } };
  assert.equal(validateResult(valid), valid.report);
  for (const edit of [v => { v.marker = 'normal'; }, v => { v.report.skipped = true; }, v => { v.report.hiddenIdleMs = 179999; }, v => { v.report.elapsedMs = Infinity; }, v => { v.fixture.captureStops--; }, v => { v.fixture.activeCaptures = 1; }, v => { v.fixture.activeProviders = 1; }, v => { v.report.scenarios[1] = v.report.scenarios[0]; }]) {
    const invalid = structuredClone(valid); edit(invalid); assert.throws(() => validateResult(invalid), /incomplete/);
  }
});

// Exercise the real project bootstrap configuration instead of a duplicate toy shape.
test('isolated bootstrap preserves updater requirements without real endpoints/profile/protocols', async () => {
  const original = JSON.parse(await readFile(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
  const before = structuredClone(original);
  const isolated = isolatedTauriConfig(original, 'Contract123');
  assert.deepEqual(original, before, 'production configuration must remain untouched');
  assert.equal(typeof isolated.plugins.updater.pubkey, 'string');
  assert.ok(isolated.plugins.updater.pubkey.length > 0, 'pinned updater Config requires pubkey');
  assert.deepEqual(isolated.plugins.updater.endpoints, []);
  assert.deepEqual(isolated.plugins['deep-link'].desktop.schemes, []);
  assert.ok(isolated.app.windows.every((window) => window.incognito === true));
  assert.equal(isolated.build.devUrl, null);
  assert.equal(isolated.build.frontendDist, '../dist');
  assert.equal(isolated.identifier, 'com.voicetotext.app.native-e2e.Contract123');
  assert.throws(() => isolatedTauriConfig({ ...original, plugins: {} }), /public key/);
  assert.throws(() => isolatedTauriConfig(original, '../unsafe'), /suffix/);
});


test('live canary remains explicit and never claims external paste verification', () => {
  assert.deepEqual(parseArguments(['--live-elevenlabs', '/tmp/synthetic.pcm']), { liveFixturePath: '/tmp/synthetic.pcm' });
  assert.throws(() => parseArguments(['--live-elevenlabs', 'relative.pcm']), /Usage/);
  const valid = { marker, passed: true, fixture: {captureStarts: 1, captureStops: 1, activeCaptures: 0},
    report: {mode: 'live-elevenlabs', passed: true, pcmBytes: 788288, targetDocument: 'Untitled test',
      finalText: 'synthetic transcript', actualPasteVerified: false} };
  assert.equal(validateResult(valid), valid.report);
  for (const modify of [r => {r.report.actualPasteVerified = true;}, r => {r.report.pcmBytes = 100;},
    r => {r.fixture.activeCaptures = 1;}, r => {r.report.targetDocument = '';}]) {
    const invalid = structuredClone(valid); modify(invalid);
    assert.throws(() => validateResult(invalid), /external verification/);
  }
});


test('terminal-only evidence cannot bypass any native cleanup phase', () => {
  assert.deepEqual(parseArguments(['--terminal-cleanup']), {terminalCleanup: true});
  const report = { mode: 'terminal-cleanup', passed: true, sleepReleased: true, wakeDidNotRestart: true,
    explicitRestart: true, holdSleepReleased: true, retiredHoldReleaseIgnored: true, holdRestartReleased: true, deviceErrorObserved: true, deviceReleased: true };
  const fixture = {captureStarts: 5, captureStops: 5, activeCaptures: 0, activeProviders: 0};
  const envelope = {marker, passed: true, report, fixture};
  assert.equal(validateResult(envelope), report);
  for (const key of ['sleepReleased', 'wakeDidNotRestart', 'explicitRestart', 'holdSleepReleased', 'retiredHoldReleaseIgnored', 'holdRestartReleased', 'deviceErrorObserved', 'deviceReleased']) {
    assert.throws(() => validateResult({...envelope, report: {...report, [key]: false}}));
  }
  assert.throws(() => validateResult({...envelope, fixture: {...fixture, activeCaptures: 1}}));
  assert.throws(() => validateResult({...envelope, fixture: {...fixture, captureStops: 2}}));
});


test('mini UX mode stays isolated and validates close, successor and delivery evidence', () => {
  assert.deepEqual(parseArguments(['--mini-ux']), { miniUx: true });
  const env = executionEnvironment('/tmp/voicetext-native-e2e-AbCd12', { miniUx: true });
  assert.equal(env.VOICETEXT_NATIVE_MINI_UX, 'unpaid-v1');
  assert.equal(env.VOICE_TO_TEXT_BACKEND_URL, 'ws://127.0.0.1:9');
  assert.equal(env.VOICETEXT_NATIVE_LIVE, undefined);
  const evidence = { marker, passed: true, report: { mode: 'mini-ux', passed: true, errors: [],
    warmActivationFrames: [{ source: 'render', revision: 8, runId: 201,
      captureReady: false, phase: 'mini-status-dot', statusText: '' }],
    cases: ['hotkey', 'native-close', 'background-start-during-hide'].map(stop => ({ stop, hideMs: 180, bufferedBeforeStop: true,
      oldProviderStillFinalizing: true, observations: 20, backgroundDidNotReopen: true,
      successorStayedVisible: true, markerDeliveryComplete: true, backgroundStartingBeforeHide: true })),
    final: { status: 'Idle', visible: false, preparedCaptureTokenCount: 0,
      fixture: { activeCaptures: 0, activeProviders: 0, maxActiveProviders: 1, markerViolations: [] } } } };
  assert.throws(() => validateResult(evidence), /physical warm input evidence/);
  const warm = structuredClone(evidence);
  Object.assign(warm.report, { warmMode: true, warmReopens: 10, idleAcceptedDelta: 0 });
  warm.report.trace = [{ label: 'warm reopen', captureReady: true }];
  warm.report.warmReadyFrames = Array.from({ length: 10 }, (_, i) => ({ runId: i + 1, revision: i + 1, phase: 'mini-status-dot recording' }));
  warm.report.warmVisibleFrames = Array.from({ length: 10 }, (_, i) => ({
    attempt: i + 1, source: 'shown', windowEpoch: i + 1, revision: i + 1, runId: i + 1,
    captureReady: false, readinessReason: 'activating-warm-capture', phase: 'mini-status-dot', statusText: '',
  }));
  warm.report.warmReuseOpenCount = 2;
  warm.report.lifecycle = { sleepClosed: true, wakeOpenedOnce: true, terminalCount: 1, recoveryOpenedOnce: true,
    physical: {
      warmReopenStart: { open: 1, close: 0 }, warmReopenEnd: { open: 1, close: 0 },
      policyActive: { open: 1, close: 0 }, policyClosed: { open: 1, close: 1 },
      policyResumed: { open: 2, close: 1 }, sleepClosed: { open: 2, close: 2 },
      wakeOpened: { open: 3, close: 2 }, terminalClosed: { open: 3, close: 3 },
      recoveryOpened: { open: 4, close: 3 },
    } };
  warm.report.final.fixture.physicalOpenCount = 4;
  warm.report.final.fixture.physicalCloseCount = 3;
  assert.equal(validateResult(warm), warm.report);
  for (const mutate of [
    e => { e.report.warmActivationFrames = []; },
    e => { e.report.trace = []; },
    e => { e.report.warmReopens = 9; },
    e => { e.report.idleAcceptedDelta = 1; },
    e => { e.report.lifecycle.physical.policyResumed.open = 3; },
    e => { e.report.final.fixture.physicalOpenCount = 5; },
    e => { e.report.final.fixture.physicalCloseCount = 2; },
    e => { e.report.warmMode = false; },
    e => { e.report.warmReadyFrames[0].phase = 'mini-status-dot starting'; },
    e => { delete e.report.warmVisibleFrames; },
    e => { e.report.warmVisibleFrames = []; },
    e => { e.report.warmVisibleFrames[0].phase = 'mini-status-dot starting'; },
    e => { e.report.warmVisibleFrames[0].phase = 'mini-status-dot recording'; },
    e => { e.report.warmVisibleFrames[0].captureReady = true; },
    e => { e.report.warmVisibleFrames[0].readinessReason = 'recording'; },
    e => { e.report.warmVisibleFrames[0].runId = 999; },
    e => { e.report.warmVisibleFrames[0].windowEpoch = 0; },
  ]) {
    const broken = structuredClone(warm);
    mutate(broken);
    assert.throws(() => validateResult(broken), /physical warm input evidence/);
  }
  for (const mutate of [
    e => { e.report.cases[0].hideMs = 5000; },
    e => { e.report.cases[0].backgroundDidNotReopen = false; },
    e => { e.report.cases[1].successorStayedVisible = false; },
    e => { e.report.cases[1].markerDeliveryComplete = false; },
    e => { e.report.final.fixture.activeCaptures = 1; },
    e => { e.report.warmActivationFrames[0].captureReady = true; },
    e => { e.report.warmActivationFrames[0].phase = 'mini-status-dot starting'; },
    e => { e.report.warmActivationFrames[0].statusText = 'Listening'; },
    e => { e.report.warmActivationFrames[0].revision = null; },
  ]) {
    const broken = structuredClone(warm); mutate(broken);
    assert.throws(() => validateResult(broken), /mini UX window evidence/);
  }
});
