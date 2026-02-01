import { spawnSync } from 'node:child_process';
import process from 'node:process';

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

const result = spawnSync('pnpm', ['wdio', 'run', 'e2e-tests/wdio.conf.mjs'], {
  stdio: 'inherit',
  shell: true,
  env: {
    ...process.env,
    // Включаем e2e hooks и упрощаем авторизацию в debug режиме.
    VITE_E2E: '1',
    VOICETEXT_E2E: '1',
  },
});

process.exit(result.status ?? 1);

