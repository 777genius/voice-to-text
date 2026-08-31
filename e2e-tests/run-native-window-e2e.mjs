import { createWriteStream } from 'node:fs';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { cp, lstat, mkdir, mkdtemp, readFile, readdir, realpath, stat, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const source = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';
const excluded = /^(?:\.git|\.codex|\.claude|\.ssh|\.aws|\.npmrc|node_modules|target|dist|\.env(?:\..*)?|auth\.(?:json|toml)|credentials(?:\..*)?)$/i;
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');

export function parseArguments(args) {
  if (args.length === 0) return {};
  if (args.length === 2 && args[0] === '--no-build' && path.isAbsolute(args[1])) {
    return { artifactDir: args[1] };
  }
  throw new Error('Usage: node e2e-tests/run-native-window-e2e.mjs [--no-build /tmp/voicetext-native-e2e-XXXXXX]. Arbitrary binary/config flags are forbidden.');
}

export async function validateArtifactDirectory(directory) {
  const canonical = await realpath(directory);
  const temporary = await realpath(os.tmpdir());
  if (canonical !== path.resolve(directory) || path.dirname(canonical) !== temporary ||
      !/^voicetext-native-e2e-[a-zA-Z0-9]+$/.test(path.basename(canonical))) {
    throw new Error('Refusing a noncanonical or non-disposable native test directory');
  }
  const info = await stat(canonical);
  if (typeof process.getuid === 'function' && info.uid !== process.getuid()) throw new Error('Test directory belongs to another user');
  return canonical;
}

export function sanitizedEnvironment(directory, inherited = process.env) {
  return {
    PATH: inherited.PATH || '/usr/bin:/bin:/usr/sbin:/sbin',
    HOME: path.join(directory, 'home'),
    TMPDIR: os.tmpdir(),
    LANG: 'en_US.UTF-8',
    VITE_NATIVE_WINDOW_E2E: '1',
    VITE_E2E: '1',
    // Both Vite's build resolver and the WebView runtime must retain loopback.
    // Otherwise a production build correctly replaces HTTP with the real API.
    TAURI_DEBUG: 'true',
    VITE_API_URL: 'http://127.0.0.1:9',
    VOICE_TO_TEXT_API_URL: 'http://127.0.0.1:9',
    VOICE_TO_TEXT_BACKEND_URL: 'ws://127.0.0.1:9',
    VOICE_TO_TEXT_CONFIG_DIR: directory,
    VOICE_TO_TEXT_NATIVE_E2E_RESULT: path.join(directory, `result-${randomUUID()}.json`),
    XDG_CONFIG_HOME: path.join(directory, 'home', '.config'),
    XDG_CACHE_HOME: path.join(directory, 'home', '.cache'),
  };
}

async function runOwned(command, args, options, timeoutMs, logPath, progressPath) {
  const output = createWriteStream(logPath, { flags: 'wx' });
  const child = spawn(command, args, { ...options, stdio: ['ignore', 'pipe', 'pipe'] });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', (chunk) => {
    output.write(chunk);
    process.stdout.write(chunk);
  });
  let timedOut = false;
  let force;
  const started = Date.now();
  let lastProgress = started;
  const heartbeat = progressPath ? setInterval(async () => {
    try { const info = await stat(progressPath); lastProgress = Math.max(lastProgress, info.mtimeMs); } catch {}
    const silenceLimit = lastProgress === started ? 45_000 : 60_000;
    if (!timedOut && Date.now() - lastProgress > silenceLimit) {
      timedOut = true;
      child.kill('SIGTERM');
      force = setTimeout(() => child.kill('SIGKILL'), 5_000);
    }
  }, 5_000) : undefined;
  const timeout = setTimeout(() => {
    timedOut = true;
    child.kill('SIGTERM');
    force = setTimeout(() => child.kill('SIGKILL'), 5_000);
  }, timeoutMs);
  try {
    const code = await new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', (status, signal) => resolve({ status, signal }));
    });
    if (timedOut || code.status !== 0) throw new Error(`${path.basename(command)} failed: ${JSON.stringify({ ...code, timedOut })}`);
  } finally {
    clearTimeout(timeout);
    clearInterval(heartbeat);
    clearTimeout(force);
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => output.end(resolve));
  }
}

async function snapshotDigest(directory) {
  const hash = createHash('sha256');
  async function visit(current) {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (excluded.test(entry.name) || entry.isSymbolicLink()) continue;
      const file = path.join(current, entry.name);
      if (entry.isDirectory()) await visit(file);
      else if (entry.isFile()) { hash.update(path.relative(directory, file)); hash.update(await readFile(file)); }
    }
  }
  await visit(directory);
  return hash.digest('hex');
}

export async function validateCachedBinary(directory) {
  await validateArtifactDirectory(directory);
  const manifest = JSON.parse(await readFile(path.join(directory, 'native-build.json'), 'utf8'));
  if (manifest.marker !== marker || manifest.binary !== 'native-window-e2e' || !/^[a-f0-9]{64}$/.test(manifest.sourceSha256 || '') ||
      !/^com\.voicetotext\.app\.native-e2e\.[a-zA-Z0-9]+$/.test(manifest.identifier || '')) throw new Error('Invalid native build manifest');
  const binary = path.join(directory, manifest.binary);
  if (await realpath(binary) !== binary) throw new Error('Native binary cannot be a symlink');
  const bytes = await readFile(binary);
  if (digest(bytes) !== manifest.sha256 || !bytes.includes(Buffer.from(marker))) throw new Error('Native binary hash/feature marker mismatch');
  if (!bytes.includes(Buffer.from(manifest.identifier))) throw new Error('Native binary application identity mismatch');
  return binary;
}

export function validateResult(envelope) {
  const report = envelope?.report;
  const fixture = envelope?.fixture;
  if (envelope?.marker !== marker || envelope.passed !== true || !report || !fixture ||
      !Number.isFinite(report.elapsedMs) || report.elapsedMs < 180_000 || report.elapsedMs > 480_000 ||
      !Number.isSafeInteger(fixture.captureStarts) || fixture.captureStarts <= 0 ||
      fixture.activeCaptures !== 0 || fixture.activeProviders !== 0 || fixture.captureStarts !== fixture.captureStops ||
      !Array.isArray(report.scenarios) || new Set(report.scenarios).size !== report.scenarios.length ||
      report.scenarios.some((name) => typeof name !== 'string' || !name) || report.scenarios.length < 12 ||
      report.passed !== true || !Number.isSafeInteger(report.completedCycles) || report.completedCycles < 20 ||
      !Number.isFinite(report.hiddenIdleMs) || report.hiddenIdleMs < 180_000 || report.skipped) {
    throw new Error(`Native result is incomplete: ${JSON.stringify(envelope)}`);
  }
  return report;
}

export function isolatedTauriConfig(original, suffix = randomUUID().replaceAll('-', '')) {
  if (!/^[a-zA-Z0-9]+$/.test(suffix)) throw new Error('Unsafe native identifier suffix');
  const config = structuredClone(original);
  const updaterKey = original?.plugins?.updater?.pubkey;
  // The installed updater plugin requires a string public key during initialization,
  // even in a debug build which never schedules update checks. Keep its required shape
  // while removing every production endpoint and deep-link scheme from this fixture.
  if (typeof updaterKey !== 'string' || !updaterKey) throw new Error('Native bootstrap requires configured updater public key');
  config.identifier = `com.voicetotext.app.native-e2e.${suffix}`;
  config.productName = 'VoicetextAI Native E2E';
  for (const window of config.app.windows) window.incognito = true;
  config.build.devUrl = null;
  config.build.frontendDist = '../dist';
  config.build.beforeBuildCommand = '';
  config.build.beforeBundleCommand = '';
  config.bundle.active = false;
  config.plugins = { updater: { pubkey: updaterKey, endpoints: [] }, 'deep-link': { desktop: { schemes: [] } } };
  return config;
}

export async function main(args = process.argv.slice(2)) {
  const options = parseArguments(args);
  if (process.platform !== 'darwin') throw new Error('Native macOS E2E requires macOS; unsupported platforms are failures, never passing skips');
  const directory = options.artifactDir
    ? await validateArtifactDirectory(options.artifactDir)
    : await realpath(await mkdtemp(path.join(os.tmpdir(), 'voicetext-native-e2e-')));
  const env = sanitizedEnvironment(directory);
  await mkdir(env.HOME, { recursive: true });
  console.log(`[native-e2e] artifacts: ${directory}`);
  if (!options.artifactDir) {
    const snapshot = path.join(directory, 'frontend');
    await cp(source, snapshot, { recursive: true, dereference: false,
      filter: async (entry) => !path.relative(source, entry).split(path.sep).some((part) => excluded.test(part)) && !(await lstat(entry)).isSymbolicLink(),
    });
    // Only shared, already-installed dependencies are linked; source and build output stay disposable.
    await symlink(path.join(source, 'node_modules'), path.join(snapshot, 'node_modules'), 'dir');
    const config = isolatedTauriConfig(JSON.parse(await readFile(path.join(snapshot, 'src-tauri', 'tauri.conf.json'), 'utf8')));
    await writeFile(path.join(snapshot, 'src-tauri', 'tauri.conf.json'), JSON.stringify(config, null, 2));
    await runOwned(process.execPath, [path.join(snapshot, 'node_modules/vite/bin/vite.js'), 'build', '--mode', 'native-window-e2e'],
      { cwd: snapshot, env }, 180_000, path.join(directory, 'frontend-build.log'));
    const rustEnv = { ...env,
      RUSTUP_HOME: path.join(os.homedir(), '.rustup'),
      CARGO_HOME: path.join(os.homedir(), '.cargo'),
      CARGO_TARGET_DIR: '/tmp/voicetext-rust-target',
      CARGO_BUILD_JOBS: '2',
    };
    await runOwned('cargo', ['build', '--locked', '--features', 'native-window-e2e,tauri/custom-protocol', '--bin', 'voice-to-text'],
      { cwd: path.join(snapshot, 'src-tauri'), env: rustEnv }, 1_800_000, path.join(directory, 'rust-build.log'));
    const sourceSha256 = await snapshotDigest(snapshot);
    const binary = path.join(directory, 'native-window-e2e');
    await cp(path.join(rustEnv.CARGO_TARGET_DIR, 'debug', 'voice-to-text'), binary);
    const binaryBytes = await readFile(binary);
    // A concurrent build sharing the Cargo cache must never be silently bound to
    // this snapshot. Each isolated config has a unique embedded application ID.
    if (!binaryBytes.includes(Buffer.from(config.identifier))) throw new Error('Native binary application identity mismatch');
    await writeFile(path.join(directory, 'native-build.json'), JSON.stringify({ marker, binary: path.basename(binary), sha256: digest(binaryBytes), identifier: config.identifier, sourceSha256 }));
  }
  const binary = await validateCachedBinary(directory);
  let runtimeFailure;
  try {
    await runOwned(binary, [], { cwd: directory, env }, 480_000, path.join(directory, `native-runtime-${randomUUID()}.log`), path.join(directory, 'native-progress.jsonl'));
  } catch (error) { runtimeFailure = error; }
  let envelope;
  try { envelope = JSON.parse(await readFile(env.VOICE_TO_TEXT_NATIVE_E2E_RESULT, 'utf8')); }
  catch (error) { throw runtimeFailure || error; }
  if (runtimeFailure) throw new Error(`${runtimeFailure.message}; native report: ${JSON.stringify(envelope)}`);
  validateResult(envelope);
  console.log(`[native-e2e] PASS ${env.VOICE_TO_TEXT_NATIVE_E2E_RESULT}`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(`[native-e2e] FAIL ${error.stack || error}`); process.exitCode = 1; });
}
