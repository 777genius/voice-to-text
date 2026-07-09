import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
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

export function readEnvOpenAiKey({
  env = process.env,
  dotenvPath = 'src-tauri/.env',
  exists = existsSync,
  readFile = readFileSync,
} = {}) {
  if (env.OPENAI_API_KEY?.trim()) {
    return env.OPENAI_API_KEY.trim();
  }

  if (!exists(dotenvPath)) {
    return '';
  }

  for (const entry of readFile(dotenvPath, 'utf8').split(/\r?\n/)) {
    const match = entry.trim().match(/^(?:export\s+)?OPENAI_API_KEY\s*=\s*(.*)$/);
    if (!match) {
      continue;
    }

    const rawValue = match[1].trim();
    if (!rawValue) {
      return '';
    }

    if (
      (rawValue.startsWith('"') && rawValue.endsWith('"')) ||
      (rawValue.startsWith("'") && rawValue.endsWith("'"))
    ) {
      return rawValue.slice(1, -1).trim();
    }

    return rawValue.split(/\s+#/)[0].trim();
  }

  return '';
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
