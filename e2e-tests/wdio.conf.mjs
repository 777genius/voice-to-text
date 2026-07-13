import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import {
  assertTcpPortFree,
  createExpectedProcessExitTracker,
  waitForTcpPort,
} from './helpers/processUtils.mjs';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const WEBDRIVER_HOST = '127.0.0.1';
const WEBDRIVER_PORT = 4444;
const TAURI_DRIVER_READY_TIMEOUT_MS = 10_000;

// держим ссылку на tauri-driver процесс
let tauriDriver;
const tauriDriverExit = createExpectedProcessExitTracker();

function resolveNodeBin(name) {
  const suffix = process.platform === 'win32' ? '.cmd' : '';
  return path.resolve(__dirname, '../node_modules/.bin', `${name}${suffix}`);
}

function resolveAppBinaryPath() {
  const base = path.resolve(__dirname, '../src-tauri/target/debug');
  // На Windows бинарь с .exe
  if (process.platform === 'win32') {
    return path.resolve(base, 'voice-to-text.exe');
  }
  return path.resolve(base, 'voice-to-text');
}

export const config = {
  hostname: WEBDRIVER_HOST,
  port: WEBDRIVER_PORT,

  specs: [path.resolve(__dirname, './specs/**/*.e2e.mjs')],

  maxInstances: 1,
  capabilities: [
    {
      maxInstances: 1,
      'tauri:options': {
        application: resolveAppBinaryPath(),
      },
    },
  ],

  reporters: ['spec'],
  framework: 'mocha',
  mochaOpts: {
    ui: 'bdd',
    timeout: 120000,
  },

  onPrepare: () => {
    // Собираем debug бинарь без бандла, чтобы путь был стабильным.
    const res = spawnSync(
      resolveNodeBin('tauri'),
      ['build', '--debug', '--no-bundle', '--ci', '--features', 'webdriver-e2e'],
      {
        cwd: path.resolve(__dirname, '..'),
        stdio: 'inherit',
        shell: process.platform === 'win32',
        env: {
          ...process.env,
          VITE_E2E: '1',
        },
      },
    );
    if (res.error) {
      throw new Error(`[e2e] failed to run local Tauri CLI: ${res.error.message}`);
    }
    if ((res.status ?? 1) !== 0) {
      throw new Error(`[e2e] failed to build tauri app (exit=${res.status})`);
    }
  },

  beforeSession: async () => {
    // Запускаем tauri-driver, который проксирует WebDriver запросы в нативный драйвер (WebKitWebDriver на Linux).
    const driverBin =
      process.env.TAURI_DRIVER_PATH ??
      path.resolve(os.homedir(), '.cargo', 'bin', 'tauri-driver');

    await assertTcpPortFree({
      host: WEBDRIVER_HOST,
      port: WEBDRIVER_PORT,
    });

    tauriDriverExit.markStarted();
    tauriDriver = spawn(driverBin, [], {
      detached: process.platform !== 'win32',
      stdio: [null, process.stdout, process.stderr],
    });

    tauriDriver.on('error', (error) => {
      console.error('[e2e] tauri-driver error:', error);
      process.exit(1);
    });

    tauriDriver.on('exit', (code) => {
      if (!tauriDriverExit.isExpected()) {
        console.error('[e2e] tauri-driver exited with code:', code);
        process.exit(1);
      }
    });

    try {
      await waitForTcpPort({
        host: WEBDRIVER_HOST,
        port: WEBDRIVER_PORT,
        timeoutMs: TAURI_DRIVER_READY_TIMEOUT_MS,
      });
    } catch (error) {
      closeTauriDriver();
      throw new Error(`[e2e] tauri-driver did not become ready: ${error.message}`);
    }
  },

  afterSession: () => {
    closeTauriDriver();
  },
};

function closeTauriDriver() {
  tauriDriverExit.markStopping();
  terminateProcessTree(tauriDriver);
  tauriDriver = undefined;
}

function terminateProcessTree(child) {
  if (!child?.pid) {
    return;
  }

  if (process.platform !== 'win32') {
    try {
      process.kill(-child.pid, 'SIGTERM');
      return;
    } catch {}
  }

  try {
    child.kill('SIGTERM');
  } catch {}
}

function onShutdown(fn) {
  let cleaned = false;
  const cleanup = () => {
    if (cleaned) {
      return;
    }
    cleaned = true;
    fn();
  };

  process.on('exit', cleanup);
  const signalExitCodes = {
    SIGHUP: 129,
    SIGINT: 130,
    SIGTERM: 143,
    SIGBREAK: 130,
  };

  for (const [signal, exitCode] of Object.entries(signalExitCodes)) {
    process.on(signal, () => {
      cleanup();
      process.exit(exitCode);
    });
  }
}

onShutdown(() => {
  closeTauriDriver();
});
