import test from 'node:test';
import { execFile } from 'node:child_process';
import { createRequire } from 'node:module';
import { promisify } from 'node:util';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, cp, link, mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { liveTrials } from './nativeContinuation.mjs';
import { assertUnpaidReplay, executionEnvironment, isolatedTauriConfig, parseArguments, prepareReuseBuild,
  snapshotDigest, sourceInputFingerprint, validateCachedBinary, validateReusableBuild } from '../run-native-window-e2e.mjs';

const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';
async function fixture(t) {
  const make = async prefix => {
    const dir = await realpath(await mkdtemp(path.join(os.tmpdir(), prefix)));
    t.after(() => rm(dir, { recursive: true, force: true }));
    return dir;
  };
  const checkout = await make('runner-reuse-source-');
  const origin = await make('voicetext-native-e2e-');
  await mkdir(path.join(checkout, 'src-tauri'));
  const originalTauriConfig = JSON.stringify({ identifier: 'production', productName: 'original',
    app: { windows: [{}] }, build: {}, bundle: {}, plugins: { updater: { pubkey: 'fixture-key' } } });
  await writeFile(path.join(checkout, 'src-tauri/tauri.conf.json'), originalTauriConfig);
  await writeFile(path.join(checkout, 'input.rs'), 'final integrated fixture source');
  const snapshot = path.join(origin, 'frontend');
  await cp(checkout, snapshot, { recursive: true });
  const config = isolatedTauriConfig(JSON.parse(originalTauriConfig), 'CompiledOnce123');
  await writeFile(path.join(snapshot, 'src-tauri/tauri.conf.json'), JSON.stringify(config));
  // Text bytes only: this fixture is never executable and no process is launched.
  const bytes = Buffer.from(`${marker}\0${config.identifier}\0fixture`);
  await writeFile(path.join(origin, 'native-window-e2e'), bytes);
  const binding = { sha256: sha(bytes), identifier: config.identifier,
    sourceSha256: await snapshotDigest(snapshot), sourceInputSha256: await sourceInputFingerprint(checkout) };
  const manifest = { schema: 'native-build-reuse-v1', marker, binary: 'native-window-e2e', ...binding,
    buildOrigin: binding, originalTauriConfig, mode: 'continuation-live', trialId: 'warm-continue-1',
    endpoint: 'ws://old.invalid', capability: true };
  await writeFile(path.join(origin, 'native-build.json'), JSON.stringify(manifest));
  const reuse = async selection => {
    const options = parseArguments(['--reuse-build', origin, ...selection]);
    const directory = await prepareReuseBuild(options, checkout);
    t.after(() => rm(directory, { recursive: true, force: true }));
    return { directory, options, env: executionEnvironment(directory, options),
      manifest: JSON.parse(await readFile(path.join(directory, 'native-build.json'))) };
  };
  return { checkout, origin, snapshot, manifest, reuse };
}

test('reuse selects exactly one of 12 planned trials or 9 cases; rejects replay/matrix/implicit selection', () => {
  const root = '/tmp/voicetext-native-e2e-Abc123';
  for (const trial of liveTrials) assert.equal(parseArguments(['--reuse-build', root, '--qualification-live', '/tmp/backend.json', trial.id]).trialId, trial.id);
  for (const name of ['seal-stop', 'seal-hold', 'seal-close', 'seal-toggle', 'stale-epoch', 'terminal-before-write', 'E04', 'E41', 'E42']) {
    assert.equal(parseArguments(['--reuse-build', root, '--continuation-case', name]).continuationCase, name);
  }
  for (const tail of [[], ['--continuation-fake'], ['--no-build', root], ['--live-elevenlabs', '/tmp/audio'],
    ['--continuation-case', 'all'], ['--qualification-live', '/tmp/backend.json', 'retry'],
    ['--qualification-live', '/tmp/backend.json', 'long', 'long'], ['--continuation-case', 'seal-toggle', '--retry']]) {
    assert.throws(() => parseArguments(['--reuse-build', root, ...tail]), /Usage/);
  }
  assert.throws(() => parseArguments(['--reuse-build', 'relative', '--continuation-case', 'seal-toggle']));
});

test('fresh executions copy only build provenance, keep compiled identity and create independent runtime paths', async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.origin, 'home'));
  for (const name of ['home/secret', 'result-old.json', 'qualification-trial.json', 'backend-provenance.json',
    'proxy-evidence.json', 'config.json', 'p4-textedit-a.txt', 'native-progress.jsonl', 'synthetic.pcm']) {
    await writeFile(path.join(f.origin, name), 'old runtime state');
  }
  await mkdir(path.join(f.snapshot, 'dist'));
  await writeFile(path.join(f.snapshot, 'dist/old.js'), 'generated');
  await symlink(f.checkout, path.join(f.snapshot, 'node_modules'));
  const a = await f.reuse(['--qualification-live', '/tmp/fresh-backend.json', 'warm-baseline-2']);
  const b = await f.reuse(['--continuation-case', 'seal-toggle']);
  for (const result of [a, b]) {
    assert.notEqual(result.directory, f.origin);
    assert.equal(await realpath(result.directory), result.directory);
    assert.deepEqual((await readdir(result.directory)).sort(), ['frontend', 'native-build.json', 'native-window-e2e', 'reuse-origin.json']);
    assert.deepEqual((await readdir(path.join(result.directory, 'frontend'))).sort(), ['input.rs', 'src-tauri']);
    assert.deepEqual(result.manifest.buildOrigin, f.manifest.buildOrigin);
    assert.equal(result.manifest.sha256, f.manifest.sha256);
    assert.equal(result.manifest.sourceSha256, f.manifest.sourceSha256);
    assert.equal(result.manifest.identifier, f.manifest.identifier);
    assert.equal(result.manifest.endpoint, undefined);
    assert.equal(result.manifest.capability, undefined);
    assert.equal(result.manifest.originManifestSha256, sha(await readFile(path.join(f.origin, 'native-build.json'))));
    assert.equal(await validateCachedBinary(result.directory), path.join(result.directory, 'native-window-e2e'));
    assert.equal(result.env.HOME, path.join(result.directory, 'home'));
    assert.equal(result.env.VOICE_TO_TEXT_CONFIG_DIR, result.directory);
    assert.equal(path.dirname(result.env.VOICE_TO_TEXT_NATIVE_E2E_RESULT), result.directory);
    assert.equal(result.env.VOICETEXT_QUALIFICATION_ENDPOINT, undefined);
  }
  assert.notEqual(a.directory, b.directory);
  assert.notEqual(a.env.HOME, b.env.HOME);
  assert.notEqual(a.env.VOICE_TO_TEXT_NATIVE_E2E_RESULT, b.env.VOICE_TO_TEXT_NATIVE_E2E_RESULT);
  assert.notEqual(path.join(a.directory, 'p4-textedit-a.txt'), path.join(b.directory, 'p4-textedit-a.txt'));
  assert.equal(a.manifest.mode, 'continuation-live');
  assert.equal(a.manifest.trialId, 'warm-baseline-2');
  assert.equal(a.manifest.continuationCase, null);
  assert.equal(b.manifest.mode, 'continuation-fake');
  assert.equal(b.manifest.trialId, null);
  assert.equal(b.manifest.continuationCase, 'seal-toggle');
  assert.equal(a.env.VOICETEXT_NATIVE_CONTINUATION, 'p4-live-v1');
  assert.equal(a.env.VOICETEXT_EL_PAUSE_CONTINUE_V1, undefined);
  assert.equal(a.env.VOICETEXT_EL_FINALIZE_OUTCOME_V1, undefined);
  assert.equal(a.env.VOICETEXT_NATIVE_CONTINUATION_CASE, undefined);
  assert.equal(b.env.VOICETEXT_NATIVE_CONTINUATION, 'p4-fake-v1');
  assert.equal(b.env.VOICETEXT_NATIVE_CONTINUATION_CASE, 'seal-toggle');
  assert.equal(b.env.VOICETEXT_EL_PAUSE_CONTINUE_V1, 'true');
  const continuing = executionEnvironment(a.directory, { trialId: 'warm-continue-2' });
  assert.equal(continuing.VOICETEXT_EL_PAUSE_CONTINUE_V1, 'true');
  assert.equal(continuing.VOICETEXT_EL_FINALIZE_OUTCOME_V1, 'true');
});

test('paid no-build replay stays forbidden including a newly reused paid manifest', async t => {
  const f = await fixture(t);
  assert.throws(() => assertUnpaidReplay(f.manifest), /Paid/);
  assert.throws(() => assertUnpaidReplay({ mode: 'live-elevenlabs' }), /Paid/);
  const next = await f.reuse(['--qualification-live', '/tmp/backend.json', 'cold-4000']);
  assert.throws(() => assertUnpaidReplay(next.manifest), /Paid/);
  assert.doesNotThrow(() => assertUnpaidReplay({ mode: 'continuation-fake' }));
});

for (const mutation of ['binary', 'snapshot', 'checkout', 'config', 'identity', 'origin', 'old-manifest']) {
  test(`refuses changed ${mutation} before reuse`, async t => {
    const f = await fixture(t);
    if (mutation === 'binary') await writeFile(path.join(f.origin, 'native-window-e2e'), 'changed');
    if (mutation === 'snapshot') await writeFile(path.join(f.snapshot, 'input.rs'), 'changed');
    if (mutation === 'checkout') await writeFile(path.join(f.checkout, 'input.rs'), 'changed');
    if (mutation === 'config') {
      const config = JSON.parse(await readFile(path.join(f.checkout, 'src-tauri/tauri.conf.json')));
      config.identifier = 'new-production-identity';
      await writeFile(path.join(f.checkout, 'src-tauri/tauri.conf.json'), JSON.stringify(config));
    }
    if (mutation === 'identity') f.manifest.identifier += 'Other';
    if (mutation === 'origin') f.manifest.buildOrigin.sha256 = 'a'.repeat(64);
    if (mutation === 'old-manifest') delete f.manifest.schema;
    await writeFile(path.join(f.origin, 'native-build.json'), JSON.stringify(f.manifest));
    await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']));
  });
}

for (const target of ['native-build.json', 'native-window-e2e', 'frontend', 'frontend/input.rs']) {
  test(`refuses symlink at ${target}`, async t => {
    const f = await fixture(t);
    const file = path.join(f.origin, target);
    const saved = path.join(f.origin, 'saved');
    await cp(file, saved, { recursive: true });
    await rm(file, { recursive: true });
    await symlink(saved, file);
    await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /Untrusted|symlink|Noncanonical/);
  });
}

test('refuses untrusted paths, writable artifacts, hardlinks and checkout source symlinks', async t => {
  const f = await fixture(t);
  await assert.rejects(prepareReuseBuild({ reuseBuild: f.checkout, continuationCase: 'seal-toggle' }, f.checkout), /Refusing/);
  await chmod(f.origin, 0o777);
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /Untrusted/);
  await chmod(f.origin, 0o700);
  const binary = path.join(f.origin, 'native-window-e2e');
  await chmod(binary, 0o666);
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /Untrusted/);
  await chmod(binary, 0o600);
  await link(binary, path.join(f.origin, 'hardlink'));
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /Untrusted/);
  await rm(path.join(f.origin, 'hardlink'));
  await symlink(path.join(f.checkout, 'input.rs'), path.join(f.checkout, 'new.rs'));
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /symlink/);
});

test('launch gate rechecks copied binary, source, provenance and current checkout after preparation', async t => {
  const f = await fixture(t);
  for (const target of ['native-window-e2e', 'frontend/input.rs', 'reuse-origin.json']) {
    const result = await f.reuse(['--continuation-case', 'seal-stop']);
    await writeFile(path.join(result.directory, target), 'mutated after copy');
    await assert.rejects(validateReusableBuild(result.directory, f.checkout));
  }
  const result = await f.reuse(['--continuation-case', 'seal-close']);
  await writeFile(path.join(f.checkout, 'added.rs'), 'new input');
  await assert.rejects(validateReusableBuild(result.directory, f.checkout), /fingerprint/);
});

test('only the exact isolated Tauri transformation may differ from original source inputs', async t => {
  const f = await fixture(t);
  assert.equal((await validateReusableBuild(f.origin, f.checkout)).sourceInputSha256, f.manifest.sourceInputSha256);
  const file = path.join(f.snapshot, 'src-tauri/tauri.conf.json');
  const config = JSON.parse(await readFile(file));
  config.build.beforeBuildCommand = 'untrusted';
  await writeFile(file, JSON.stringify(config));
  f.manifest.sourceSha256 = await snapshotDigest(f.snapshot);
  f.manifest.buildOrigin.sourceSha256 = f.manifest.sourceSha256;
  await writeFile(path.join(f.origin, 'native-build.json'), JSON.stringify(f.manifest));
  await assert.rejects(validateReusableBuild(f.origin, f.checkout), /transformation/);
});

test('fake build reused for live baseline resets selection and retains original build binding', async t => {
  const f = await fixture(t);
  const fake = await f.reuse(['--continuation-case', 'terminal-before-write']);
  const options = parseArguments(['--reuse-build', fake.directory, '--qualification-live', '/tmp/new-backend.json', 'warm-baseline-3']);
  const directory = await prepareReuseBuild(options, f.checkout);
  t.after(() => rm(directory, { recursive: true, force: true }));
  const manifest = await validateReusableBuild(directory, f.checkout);
  assert.deepEqual(manifest.buildOrigin, f.manifest.buildOrigin);
  assert.equal(manifest.trialId, 'warm-baseline-3');
  assert.equal(manifest.continuationCase, null);
  const env = executionEnvironment(directory, options);
  assert.equal(env.VOICETEXT_NATIVE_CONTINUATION, 'p4-live-v1');
  assert.equal(env.VOICETEXT_NATIVE_CONTINUATION_CASE, undefined);
  assert.equal(env.VOICETEXT_EL_PAUSE_CONTINUE_V1, undefined);
  assert.equal(env.VOICETEXT_QUALIFICATION_ENDPOINT, undefined);
});

test('runner integration has one launch, revalidates before proxy effects, and retains failures without retry', async () => {
  const code = await readFile(new URL('../run-native-window-e2e.mjs', import.meta.url), 'utf8');
  const main = code.slice(code.indexOf('export async function main'));
  assert.equal((main.match(/await runOwned\(binary,/g) || []).length, 1);
  assert.ok(main.indexOf('await validateReusableBuild(directory, source)') < main.indexOf('await startConfigDelayProxy('));
  assert.ok(main.indexOf('assertUnpaidReplay(cached)') < main.indexOf('await runOwned(binary,'));
  assert.match(main, /if \(!options.artifactDir && !options.reuseBuild\)/);
  assert.match(main, /catch \(error\) \{ runtimeFailure = error; \}/);
  assert.match(main, /if \(runtimeFailure\) throw runtimeFailure/);
  assert.doesNotMatch(main, /for\s*\([^)]*(?:liveTrials|attempt|retry)|while\s*\(/);
});

const schemaNames = ['acl-manifests.json', 'capabilities.json', 'desktop-schema.json', 'macOS-schema.json'];
async function addSchemas(root) {
  await mkdir(path.join(root, 'src-tauri/gen/schemas'), { recursive: true });
  for (const name of schemaNames) await writeFile(path.join(root, 'src-tauri/gen/schemas', name), `generated ${name}`);
}
async function bindSnapshot(f) {
  f.manifest.sourceSha256 = await snapshotDigest(f.snapshot);
  f.manifest.buildOrigin.sourceSha256 = f.manifest.sourceSha256;
  await writeFile(path.join(f.origin, 'native-build.json'), JSON.stringify(f.manifest));
}
test('clean source accepts four ordinary generated schemas and preserves bytes through reuse', async t => {
  const f = await fixture(t);
  await addSchemas(f.snapshot);
  assert.notEqual(await snapshotDigest(f.snapshot), f.manifest.sourceSha256);
  await assert.rejects(validateReusableBuild(f.origin, f.checkout), /snapshot hash/);
  await bindSnapshot(f); // Simulate original fresh postbuild artifact binding.
  await validateReusableBuild(f.origin, f.checkout);
  const reused = await f.reuse(['--continuation-case', 'seal-toggle']);
  for (const name of schemaNames) assert.deepEqual(await readFile(path.join(reused.directory, 'frontend/src-tauri/gen/schemas', name)), await readFile(path.join(f.snapshot, 'src-tauri/gen/schemas', name)));
  await addSchemas(f.checkout);
  assert.equal(await sourceInputFingerprint(f.checkout), f.manifest.sourceInputSha256);
  await writeFile(path.join(reused.directory, 'frontend/src-tauri/gen/schemas/macOS-schema.json'), 'changed after build');
  await assert.rejects(validateReusableBuild(reused.directory, f.checkout), /snapshot hash/);
  const manifestPath = path.join(reused.directory, 'native-build.json');
  const modified = JSON.parse(await readFile(manifestPath));
  modified.sourceSha256 = await snapshotDigest(path.join(reused.directory, 'frontend'));
  modified.buildOrigin.sourceSha256 = modified.sourceSha256;
  await writeFile(manifestPath, JSON.stringify(modified));
  await assert.rejects(validateReusableBuild(reused.directory, f.checkout), /origin provenance/);
});
test('generated-only empty parents disappear but unknown gen files and directories remain inputs', async t => {
  const f = await fixture(t);
  await mkdir(path.join(f.checkout, 'src-tauri/gen/schemas'), { recursive: true });
  assert.equal(await sourceInputFingerprint(f.checkout), f.manifest.sourceInputSha256);
  for (const relative of ['src-tauri/gen/unknown.json', 'src-tauri/gen/schemas/linux-schema.json',
    'src-tauri/capabilities/default.json', 'Cargo.lock', 'runner.mjs', 'input.rs']) {
    const changed = await fixture(t);
    await mkdir(path.dirname(path.join(changed.snapshot, relative)), { recursive: true });
    await writeFile(path.join(changed.snapshot, relative), 'real input change');
    await bindSnapshot(changed);
    await assert.rejects(validateReusableBuild(changed.origin, changed.checkout), /fingerprint/);
  }
  await mkdir(path.join(f.checkout, 'src-tauri/gen/unknown-empty'));
  assert.notEqual(await sourceInputFingerprint(f.checkout), f.manifest.sourceInputSha256);
});
test('generated outputs still reject symlinks, directories, hardlinks and writable artifacts', async t => {
  const f = await fixture(t);
  await addSchemas(f.snapshot);
  await bindSnapshot(f);
  const file = path.join(f.snapshot, 'src-tauri/gen/schemas/acl-manifests.json');
  for (const kind of ['symlink', 'directory', 'hardlink', 'writable']) {
    await rm(file, { recursive: true, force: true });
    if (kind === 'symlink') await symlink(path.join(f.snapshot, 'input.rs'), file);
    if (kind === 'directory') await mkdir(file);
    if (kind === 'hardlink') await link(path.join(f.snapshot, 'input.rs'), file);
    if (kind === 'writable') { await writeFile(file, 'generated'); await chmod(file, 0o666); }
    await assert.rejects(validateReusableBuild(f.origin, f.checkout), /Untrusted|symlink|regular file/);
  }
  await rm(file);
  await writeFile(file, 'generated');
  await chmod(path.dirname(file), 0o777);
  await assert.rejects(validateReusableBuild(f.origin, f.checkout), /Untrusted/);
  await chmod(path.dirname(file), 0o700);
});

const permitted = [
  'e2e-tests/helpers/nativeContinuation.mjs', 'e2e-tests/helpers/nativeContinuation.test.mjs',
  'e2e-tests/helpers/nativeContinuationProxy.mjs', 'e2e-tests/helpers/nativeContinuationProxy.test.mjs',
  'e2e-tests/helpers/nativeBuildReuse.test.mjs', 'e2e-tests/helpers/nativeQualificationGuards.test.mjs',
  'e2e-tests/run-native-window-e2e.mjs',
  'src-tauri/tests/native_context_pause_continue_e2e.rs',
  'src-tauri/tests/native_context_stalled_ax_e2e.rs',
].sort();
async function inputFixture(t) {
  const f = await fixture(t);
  for (const root of [f.checkout, f.snapshot]) {
    for (const name of [...permitted, 'src/App.vue', 'assets/icon.png', 'package.json', 'pnpm-lock.yaml', 'src-tauri/src/main.rs', 'unknown.txt']) {
      await mkdir(path.dirname(path.join(root, name)), { recursive: true });
      await writeFile(path.join(root, name), 'original');
    }
  }
  f.manifest.sourceInputSha256 = await sourceInputFingerprint(f.checkout);
  f.manifest.buildOrigin.sourceInputSha256 = f.manifest.sourceInputSha256;
  await bindSnapshot(f);
  return f;
}
test('exact runner and stalled AX integration test changes get fresh evidence with immutable origin and successive reuse', async t => {
  const f = await inputFixture(t);
  const originBytes = await readFile(path.join(f.origin, 'native-build.json'));
  for (const name of permitted) await writeFile(path.join(f.checkout, name), 'new external Node');
  const a = await f.reuse(['--continuation-case', 'seal-toggle']);
  assert.deepEqual(a.manifest.allowedChangedPaths, permitted);
  assert.equal(a.manifest.runnerSourceSha256, await sourceInputFingerprint(f.checkout));
  assert.notEqual(a.manifest.runnerSourceSha256, a.manifest.sourceInputSha256);
  assert.deepEqual(a.manifest.buildOrigin, f.manifest.buildOrigin);
  assert.deepEqual(await readFile(path.join(f.origin, 'native-build.json')), originBytes);
  assert.deepEqual(await readFile(path.join(a.directory, 'reuse-origin.json')), originBytes);
  const b = await prepareReuseBuild({ reuseBuild: a.directory, continuationCase: 'seal-stop' }, f.checkout);
  t.after(() => rm(b, { recursive: true, force: true }));
  const bm = await validateReusableBuild(b, f.checkout);
  assert.deepEqual(bm.allowedChangedPaths, permitted);
  assert.deepEqual(bm.buildOrigin, f.manifest.buildOrigin);
  assert.deepEqual(await readFile(path.join(b, 'reuse-origin.json')), await readFile(path.join(a.directory, 'native-build.json')));
  for (const field of ['runnerSourceSha256', 'allowedChangedPaths']) {
    const altered = { ...a.manifest, [field]: field === 'runnerSourceSha256' ? '0'.repeat(64) : permitted.slice(1) };
    await writeFile(path.join(a.directory, 'native-build.json'), JSON.stringify(altered));
    await assert.rejects(validateReusableBuild(a.directory, f.checkout), /evidence/);
  }
  await writeFile(path.join(a.directory, 'native-build.json'), JSON.stringify(a.manifest));
  await writeFile(path.join(f.checkout, permitted[0]), 'later Node edit');
  await assert.rejects(validateReusableBuild(a.directory, f.checkout), /evidence/);
});
for (const allowed of ['src-tauri/tests/native_context_stalled_ax_e2e.rs',
  'src-tauri/tests/native_context_pause_continue_e2e.rs']) {
test(`only the exact ${allowed} integration test may differ from immutable app inputs`, async t => {
  const f = await inputFixture(t);
  const originalManifest = await readFile(path.join(f.origin, 'native-build.json'));
  const originalBinary = await readFile(path.join(f.origin, 'native-window-e2e'));
  const originalSnapshotHash = f.manifest.sourceSha256;
  await writeFile(path.join(f.checkout, allowed), 'new integration test only');
  const reused = await f.reuse(['--continuation-case', 'seal-toggle']);
  assert.deepEqual(reused.manifest.allowedChangedPaths, [allowed]);
  assert.equal(reused.manifest.runnerSourceSha256, await sourceInputFingerprint(f.checkout));
  assert.equal(reused.manifest.sourceSha256, originalSnapshotHash);
  assert.deepEqual(await readFile(path.join(f.origin, 'native-build.json')), originalManifest);
  assert.deepEqual(await readFile(path.join(f.origin, 'native-window-e2e')), originalBinary);
  await writeFile(path.join(f.checkout, 'src-tauri/src/main.rs'), 'changed app Rust source');
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /fingerprint/);
});
}
for (const name of ['src-tauri/src/main.rs', 'src/App.vue', 'assets/icon.png', 'src-tauri/tauri.conf.json', 'package.json', 'pnpm-lock.yaml', 'unknown.txt', 'e2e-tests/helpers/unknown.mjs']) {
  for (const mutation of ['change', 'delete', 'addition']) {
    test(`full input union rejects ${mutation}: ${name}`, async t => {
      const f = await inputFixture(t);
      const file = path.join(f.checkout, name);
      if (mutation === 'delete') {
        // Unknown Node path is absent initially: establish it in the original too.
        if (name.endsWith('unknown.mjs')) {
          for (const root of [f.checkout, f.snapshot]) await writeFile(path.join(root, name), 'original');
          f.manifest.sourceInputSha256 = await sourceInputFingerprint(f.checkout);
          f.manifest.buildOrigin.sourceInputSha256 = f.manifest.sourceInputSha256;
          await bindSnapshot(f);
        }
        await rm(file);
      } else await writeFile(mutation === 'addition' ? file + '.added' : file, 'changed');
      await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /fingerprint/);
    });
  }
}
test('allowed additions/deletions are exact and directories cannot impersonate allowed files', async t => {
  const f = await inputFixture(t);
  await rm(path.join(f.checkout, permitted[0]));
  const deleted = await f.reuse(['--continuation-case', 'seal-toggle']);
  assert.deepEqual(deleted.manifest.allowedChangedPaths, [permitted[0]]);
  await mkdir(path.join(f.checkout, permitted[0]));
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /fingerprint/);
  await rm(path.join(f.checkout, permitted[0]), { recursive: true });
  await rm(path.join(f.snapshot, permitted[0]));
  f.manifest.sourceInputSha256 = await sourceInputFingerprint(f.checkout);
  f.manifest.buildOrigin.sourceInputSha256 = f.manifest.sourceInputSha256;
  await bindSnapshot(f);
  await writeFile(path.join(f.checkout, permitted[0]), 'new');
  const added = await f.reuse(['--continuation-case', 'seal-toggle']);
  assert.deepEqual(added.manifest.allowedChangedPaths, [permitted[0]]);
});

test('runner evidence requires both exact fields and sorted paths; allowed snapshot edits remain forbidden', async t => {
  const f = await inputFixture(t);
  for (const name of permitted) await writeFile(path.join(f.checkout, name), 'changed');
  const result = await f.reuse(['--continuation-case', 'seal-toggle']);
  for (const altered of [
    { ...result.manifest, runnerSourceSha256: undefined, allowedChangedPaths: undefined },
    { ...result.manifest, runnerSourceSha256: undefined },
    { ...result.manifest, allowedChangedPaths: undefined },
    { ...result.manifest, allowedChangedPaths: [...permitted].reverse() },
    { ...result.manifest, allowedChangedPaths: [...permitted, permitted[0]] },
  ]) {
    await writeFile(path.join(result.directory, 'native-build.json'), JSON.stringify(altered));
    await assert.rejects(validateReusableBuild(result.directory, f.checkout), /evidence/);
  }
  await writeFile(path.join(f.snapshot, permitted[0]), 'tampered original Node');
  await assert.rejects(validateReusableBuild(f.origin, f.checkout), /snapshot hash/);
  await bindSnapshot(f);
  await assert.rejects(validateReusableBuild(f.origin, f.checkout), /fingerprint/);
});
test('unknown empty checkout directory is rejected by full union', async t => {
  const f = await inputFixture(t);
  await mkdir(path.join(f.checkout, 'unknown-empty'));
  await assert.rejects(f.reuse(['--continuation-case', 'seal-toggle']), /fingerprint/);
});

// Run an unchanged copy of the real entry point in an isolated Node process.
// Only this child's platform is overridden; the executable is a harmless shell
// sentinel with literal paths, never an app/native binary or a provider client.
async function launchFixture(t) {
  const f = await fixture(t);
  for (const name of ['run-native-window-e2e.mjs', 'helpers/nativeContinuation.mjs',
    'helpers/nativeQualificationSummary.mjs',
    'helpers/nativeContinuationProxy.mjs', 'helpers/nativeOwnedDocument.mjs',
    'helpers/nativeRestartCrash.mjs']) {
    const target = path.join(f.checkout, 'e2e-tests', name);
    await mkdir(path.dirname(target), { recursive: true });
    await cp(new URL('../' + name, import.meta.url), target);
  }
  await cp(f.checkout, f.snapshot, { recursive: true,
    filter: entry => entry !== path.join(f.checkout, 'src-tauri/tauri.conf.json') });
  const bytes = Buffer.from(`#!/bin/sh
# ${marker} ${f.manifest.identifier}
printf launched > launch-sentinel
exit 73
`);
  await writeFile(path.join(f.origin, 'native-window-e2e'), bytes, { mode: 0o700 });
  await chmod(path.join(f.origin, 'native-window-e2e'), 0o700);
  f.manifest.sha256 = sha(bytes);
  f.manifest.buildOrigin.sha256 = f.manifest.sha256;
  f.manifest.sourceInputSha256 = await sourceInputFingerprint(f.checkout);
  f.manifest.buildOrigin.sourceInputSha256 = f.manifest.sourceInputSha256;
  await bindSnapshot(f);
  const result = await f.reuse(['--continuation-case', 'seal-toggle']);
  const runner = pathToFileURL(path.join(f.checkout, 'e2e-tests/run-native-window-e2e.mjs')).href;
  const require = createRequire(import.meta.url);
  const nodePath = path.dirname(path.dirname(require.resolve('jsdom/package.json')));
  const launch = async () => {
    // Arguments are passed without a shell; no fixture path is interpolated into code.
    const code = `Object.defineProperty(process, 'platform', { value: 'darwin' });
      const { main } = await import(process.argv[1]);
      try { await main(['--no-build', process.argv[2]]); }
      catch (error) { console.error(error.stack); process.exitCode = 1; }`;
    try {
      await promisify(execFile)(process.execPath, ['--input-type=module', '-e', code, runner, result.directory], {
        cwd: f.checkout, timeout: 15000, maxBuffer: 1024 * 1024,
        env: { PATH: process.env.PATH, NODE_PATH: nodePath, TMPDIR: os.tmpdir() },
      });
      assert.fail('sentinel exits unsuccessfully, so main must reject');
    } catch (error) {
      assert.equal(error.code, 1, error.stderr || error.message);
      assert.equal(error.killed, false);
      return error.stderr;
    }
  };
  return { ...f, ...result, launch };
}

for (const mutation of ['valid', 'valid-legacy-same-source', 'missing-evidence', 'partial-evidence', 'tampered-evidence', 'outdated-evidence',
  'build-origin', 'origin-bytes', 'origin-binding', 'checkout-mismatch', 'unsupported', 'paid']) {
  test(`main --no-build launch authorization: ${mutation}`, async t => {
    const f = await launchFixture(t);
    let expected;
    if (mutation === 'valid-legacy-same-source') {
      delete f.manifest.runnerSourceSha256;
      delete f.manifest.allowedChangedPaths;
    }
    if (mutation === 'partial-evidence') {
      delete f.manifest.runnerSourceSha256;
      expected = /Runner source evidence mismatch/;
    }
    if (mutation === 'missing-evidence') {
      // Missing evidence must fail when the runner differs from original build inputs.
      await writeFile(path.join(f.checkout, 'e2e-tests/helpers/nativeBuildReuse.test.mjs'), 'later runner');
      delete f.manifest.runnerSourceSha256;
      delete f.manifest.allowedChangedPaths;
      expected = /Runner source evidence required/;
    }
    if (mutation === 'tampered-evidence') {
      f.manifest.runnerSourceSha256 = '0'.repeat(64);
      expected = /Runner source evidence mismatch/;
    }
    if (mutation === 'outdated-evidence') {
      await writeFile(path.join(f.checkout, 'e2e-tests/helpers/nativeBuildReuse.test.mjs'), 'later runner');
      expected = /Runner source evidence mismatch/;
    }
    if (mutation === 'build-origin') {
      f.manifest.buildOrigin.sha256 = '0'.repeat(64);
      expected = /fresh build provenance/;
    }
    if (mutation === 'origin-bytes' || mutation === 'origin-binding') {
      const file = path.join(f.directory, 'reuse-origin.json');
      const origin = JSON.parse(await readFile(file));
      origin.sha256 = '0'.repeat(64);
      const bytes = JSON.stringify(origin);
      await writeFile(file, bytes);
      if (mutation === 'origin-binding') f.manifest.originManifestSha256 = sha(bytes);
      expected = /origin provenance mismatch/;
    }
    if (mutation === 'checkout-mismatch') {
      await writeFile(path.join(f.checkout, 'input.rs'), 'changed native source');
      expected = /Source input fingerprint mismatch/;
    }
    if (mutation === 'unsupported') {
      delete f.manifest.schema;
      expected = /fresh build provenance/;
    }
    if (mutation === 'paid') {
      f.manifest.mode = 'continuation-live';
      expected = /Paid qualification cannot be replayed/;
    }
    await writeFile(path.join(f.directory, 'native-build.json'), JSON.stringify(f.manifest));
    const stderr = await f.launch();
    const sentinel = path.join(f.directory, 'launch-sentinel');
    if (mutation === 'valid' || mutation === 'valid-legacy-same-source') {
      assert.equal(await readFile(sentinel, 'utf8'), 'launched');
      assert.match(stderr, /native-window-e2e failed:.*"status":73/);
    } else {
      assert.match(stderr, expected);
      await assert.rejects(readFile(sentinel), { code: 'ENOENT' });
    }
  });
}


test('unpaid reader preparation reuse needs no backend and retains exact build provenance', async t => {
  const f = await fixture(t);
  const a = await f.reuse(['--reader-preparation']);
  assert.equal(a.manifest.mode, 'reader-preparation');
  assert.equal(a.manifest.trialId, null);
  assert.deepEqual(a.manifest.buildOrigin, f.manifest.buildOrigin);
  assert.equal(a.env.VOICETEXT_NATIVE_READER_PREPARATION, 'unpaid-v1');
  assert.equal(a.env.VOICETEXT_QUALIFICATION_ENDPOINT, undefined);
  assert.deepEqual((await readdir(a.directory)).sort(), ['frontend', 'native-build.json', 'native-window-e2e', 'reuse-origin.json']);
  assert.throws(() => assertUnpaidReplay(a.manifest), /explicit/);
  await writeFile(path.join(f.checkout, 'input.rs'), 'changed reader');
  await assert.rejects(f.reuse(['--reader-preparation']), /fingerprint mismatch/);
});
