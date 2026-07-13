import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import process from 'node:process';

export function terminateProcessGroup(pid, { platform = process.platform, kill = process.kill } = {}) {
  if (!pid || platform === 'win32') return false;
  try {
    kill(-pid, 'SIGTERM');
    return true;
  } catch {
    return false;
  }
}

export function assertCommandSucceeded(
  label,
  result,
  { timeoutMs, fail, terminate = terminateProcessGroup } = {},
) {
  if (result.error) {
    terminate(result.pid);
    if (result.error.code === 'ETIMEDOUT') {
      fail(`${label} timed out after ${timeoutMs}ms`);
    }
    fail(`${label} failed to run: ${result.error.message}`);
  }

  if (result.status !== 0) {
    terminate(result.pid);
    const signal = result.signal ? ` signal=${result.signal}` : '';
    fail(`${label} failed with exit code ${result.status ?? 'unknown'}${signal}`);
  }
}

export function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function resolvePaidE2eEnvironment({ env = process.env } = {}) {
  return {
    acknowledged: env.VOICETEXT_RUN_PAID_E2E === '1',
    apiKey: env.OPENAI_E2E_API_KEY?.trim() ?? '',
  };
}

export function sanitizedAudioTestEnvironment({ env = process.env } = {}) {
  const sanitized = { ...env };
  delete sanitized.OPENAI_API_KEY;
  delete sanitized.OPENAI_E2E_API_KEY;
  delete sanitized.VOICETEXT_RUN_PAID_E2E;
  return sanitized;
}

export function liveAudioCargoEnvironment({ env = process.env, root = process.cwd() } = {}) {
  const targetDirectory = env.CARGO_TARGET_DIR?.trim();
  const incremental = env.CARGO_INCREMENTAL?.trim();
  const splitDebuginfo = env.CARGO_PROFILE_TEST_SPLIT_DEBUGINFO?.trim();

  return {
    ...env,
    CARGO_TARGET_DIR:
      targetDirectory || join(root, 'src-tauri', 'target', 'live-audio-cargo'),
    CARGO_INCREMENTAL: incremental || '0',
    CARGO_PROFILE_TEST_SPLIT_DEBUGINFO: splitDebuginfo || 'packed',
  };
}

export function liveAudioCargoPreflightCommands(tests) {
  const uniqueCommands = new Map();

  for (const { command, testName } of tests) {
    if (command[0] !== 'cargo' || command[1] !== 'test') {
      throw new Error(`unsupported live audio test command: ${command.join(' ')}`);
    }

    const harnessSeparator = command.indexOf('--');
    const cargoArguments = command.slice(
      2,
      harnessSeparator === -1 ? command.length : harnessSeparator,
    );
    const testFilterIndex = cargoArguments.indexOf(testName);
    if (testFilterIndex === -1) {
      throw new Error(`cargo command does not contain test filter: ${testName}`);
    }
    cargoArguments.splice(testFilterIndex, 1);

    const preflightArguments = cargoArguments.filter(
      (argument) => argument !== '--no-run' && argument !== '--locked',
    );
    const preflightCommand = [
      'cargo',
      'test',
      '--locked',
      ...preflightArguments,
      '--no-run',
    ];
    uniqueCommands.set(preflightCommand.join('\0'), preflightCommand);
  }

  return [...uniqueCommands.values()];
}

export function parsePositiveIntegerEnv(value, fallback) {
  const raw = String(value ?? '').trim();
  if (!/^\d+$/.test(raw)) {
    return fallback;
  }

  const parsed = Number.parseInt(raw, 10);
  return parsed > 0 ? parsed : fallback;
}

export function nativeSpokenSoakMetricViolations(
  metrics,
  { soakSeconds, releaseGrade },
) {
  const violations = [];
  const finiteNumber = (value) => typeof value === 'number' && Number.isFinite(value);
  const requireNumber = (field, predicate) => {
    const value = metrics?.[field];
    if (!finiteNumber(value) || !predicate(value)) violations.push(field);
  };

  if (metrics?.schema_version !== 1) violations.push('schema_version');
  if (metrics?.soak_seconds !== soakSeconds) violations.push('soak_seconds');
  if (metrics?.release_grade !== releaseGrade) violations.push('release_grade');
  requireNumber('source_play_count', (value) => value === (releaseGrade ? 7 : 2));
  requireNumber('translated_text_chars', (value) => value > 0);
  requireNumber('translated_audible_samples', (value) => value > 0);
  requireNumber('playback_dropped_batches', (value) => value === 0);
  requireNumber('playback_pending_high_water_ms', (value) => value >= 0 && value <= 2000);
  requireNumber('playback_pending_at_close_ms', (value) => value >= 0 && value <= 30);
  requireNumber('rss_growth_kib', (value) => value >= 0 && value <= 32768);
  requireNumber('last_translation_text_age_ms', (value) => value >= 0 && value <= 120000);
  requireNumber('last_translation_audio_age_ms', (value) => value >= 0 && value <= 120000);
  if (
    !Array.isArray(metrics?.rss_samples_kib)
    || metrics.rss_samples_kib.length < 2
    || !metrics.rss_samples_kib.every(finiteNumber)
  ) {
    violations.push('rss_samples_kib');
  }
  if (!Array.isArray(metrics?.errors) || metrics.errors.length !== 0) {
    violations.push('errors');
  }

  return violations;
}

export function spokenRestartStressMetricViolations(metrics, { expectedCycles }) {
  const violations = [];
  const finiteNumber = (value) => typeof value === 'number' && Number.isFinite(value);
  const requireExact = (field, expected) => {
    if (!finiteNumber(metrics?.[field]) || metrics[field] !== expected) violations.push(field);
  };

  if (metrics?.schema_version !== 1) violations.push('schema_version');
  requireExact('cycles', expectedCycles);
  for (const field of [
    'capture_starts',
    'capture_stops',
    'output_opens',
    'output_closes',
    'translation_sessions_created',
    'translation_sessions_dropped',
    'translation_finishes',
    'post_stop_send_attempts',
  ]) {
    requireExact(field, expectedCycles);
  }
  requireExact('translation_aborts', 0);
  requireExact('error_callbacks', 0);
  for (const field of [
    'active_capture_high_water',
    'active_output_high_water',
    'active_translation_session_high_water',
    'active_translation_task_high_water',
    'active_websocket_high_water',
    'active_server_task_high_water',
  ]) {
    requireExact(field, 1);
  }
  if (
    !Array.isArray(metrics?.rss_samples_kib)
    || metrics.rss_samples_kib.length < 2
    || !metrics.rss_samples_kib.every(finiteNumber)
  ) {
    violations.push('rss_samples_kib');
  }
  if (
    !finiteNumber(metrics?.rss_growth_kib)
    || metrics.rss_growth_kib < 0
    || metrics.rss_growth_kib > 16384
  ) {
    violations.push('rss_growth_kib');
  }

  return violations;
}

export function writeLiveAudioSummary(fileName, summary, { root = process.cwd() } = {}) {
  const artifactDirectory = join(root, 'src-tauri', 'target', 'e2e-artifacts');
  mkdirSync(artifactDirectory, { recursive: true });
  const artifactPath = join(artifactDirectory, fileName);
  writeFileSync(artifactPath, `${JSON.stringify(summary, null, 2)}\n`, 'utf8');
  return artifactPath;
}

export function assertExpectedTestRan(label, testName, output, fail) {
  const testOk = new RegExp(`test ${escapeRegExp(testName)} \\.\\.\\. ok`).test(output);
  if (!testOk) {
    fail(`${label} did not report the expected test as passed: ${testName}`);
  }

  const summary = output.match(/test result: ok\. (\d+) passed; 0 failed;/);
  if (!summary || Number(summary[1]) < 1) {
    fail(`${label} did not report at least one passed test`);
  }
}

export function runLiveAudioCommand({
  command,
  env,
  fail,
  label,
  maxBuffer,
  testName,
  timeoutMs,
}) {
  const result = spawnSync(command[0], command.slice(1), {
    cwd: 'src-tauri',
    detached: process.platform !== 'win32',
    env,
    encoding: 'utf8',
    maxBuffer,
    stdio: 'pipe',
    timeout: timeoutMs,
  });

  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);

  assertCommandSucceeded(label, result, { timeoutMs, fail });
  assertExpectedTestRan(label, testName, `${result.stdout ?? ''}\n${result.stderr ?? ''}`, fail);
}

export function preflightLiveAudioCommands({
  tests,
  env,
  fail,
  maxBuffer,
  timeoutMs,
}) {
  let commands;
  try {
    commands = liveAudioCargoPreflightCommands(tests);
  } catch (error) {
    fail(`cannot prepare Cargo preflight: ${error.message}`);
    return;
  }

  for (const command of commands) {
    const target = command.slice(2, -1).join(' ');
    console.log(`\n[live-audio-preflight] compiling ${target}`);
    const result = spawnSync(command[0], command.slice(1), {
      cwd: 'src-tauri',
      detached: process.platform !== 'win32',
      env,
      encoding: 'utf8',
      maxBuffer,
      stdio: 'pipe',
      timeout: timeoutMs,
    });

    if (result.stdout) process.stdout.write(result.stdout);
    if (result.stderr) process.stderr.write(result.stderr);

    assertCommandSucceeded(`Cargo preflight (${target})`, result, { timeoutMs, fail });
  }
}
