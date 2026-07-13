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

export function parsePositiveIntegerEnv(value, fallback) {
  const raw = String(value ?? '').trim();
  if (!/^\d+$/.test(raw)) {
    return fallback;
  }

  const parsed = Number.parseInt(raw, 10);
  return parsed > 0 ? parsed : fallback;
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
