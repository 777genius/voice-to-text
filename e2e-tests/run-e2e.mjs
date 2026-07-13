import { spawnSync } from 'node:child_process';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));

function resolveNodeBin(name) {
  const suffix = process.platform === 'win32' ? '.cmd' : '';
  return path.resolve(__dirname, '../node_modules/.bin', `${name}${suffix}`);
}

/**
 * Tauri WebDriver e2e:
 * - На macOS (darwin) сейчас нельзя (нет WKWebView driver).
 * - На Linux/Windows — запускаем wdio.
 */

if (process.platform === 'darwin') {
  console.log(
    '[e2e] Skipped: Tauri WebDriver tests are not supported on macOS. Run them on Linux/Windows CI.',
  );
  process.exit(0);
}

const result = spawnSync(resolveNodeBin('wdio'), ['run', 'e2e-tests/wdio.conf.mjs'], {
  stdio: 'inherit',
  shell: process.platform === 'win32',
  env: {
    ...process.env,
    // Включаем frontend e2e hooks. Rust fixtures are selected by the build feature.
    VITE_E2E: '1',
  },
});

if (result.error) {
  console.error(`[e2e] failed to run local WebdriverIO binary: ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
