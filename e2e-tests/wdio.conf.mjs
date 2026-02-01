import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));

// держим ссылку на tauri-driver процесс
let tauriDriver;
let exit = false;

function resolveAppBinaryPath() {
  const base = path.resolve(__dirname, '../src-tauri/target/debug');
  // На Windows бинарь с .exe
  if (process.platform === 'win32') {
    return path.resolve(base, 'voice-to-text.exe');
  }
  return path.resolve(base, 'voice-to-text');
}

export const config = {
  hostname: '127.0.0.1',
  port: 4444,

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
    const res = spawnSync('pnpm', ['tauri', 'build', '--debug', '--no-bundle', '--ci'], {
      cwd: path.resolve(__dirname, '..'),
      stdio: 'inherit',
      shell: true,
      env: {
        ...process.env,
        VITE_E2E: '1',
        VOICETEXT_E2E: '1',
      },
    });
    if ((res.status ?? 1) !== 0) {
      throw new Error(`[e2e] failed to build tauri app (exit=${res.status})`);
    }
  },

  beforeSession: () => {
    // Запускаем tauri-driver, который проксирует WebDriver запросы в нативный драйвер (WebKitWebDriver на Linux).
    const driverBin =
      process.env.TAURI_DRIVER_PATH ??
      path.resolve(os.homedir(), '.cargo', 'bin', 'tauri-driver');

    tauriDriver = spawn(driverBin, [], { stdio: [null, process.stdout, process.stderr] });

    tauriDriver.on('error', (error) => {
      console.error('[e2e] tauri-driver error:', error);
      process.exit(1);
    });

    tauriDriver.on('exit', (code) => {
      if (!exit) {
        console.error('[e2e] tauri-driver exited with code:', code);
        process.exit(1);
      }
    });
  },

  afterSession: () => {
    closeTauriDriver();
  },
};

function closeTauriDriver() {
  exit = true;
  tauriDriver?.kill();
}

function onShutdown(fn) {
  const cleanup = () => {
    try {
      fn();
    } finally {
      process.exit();
    }
  };

  process.on('exit', cleanup);
  process.on('SIGINT', cleanup);
  process.on('SIGTERM', cleanup);
  process.on('SIGHUP', cleanup);
  process.on('SIGBREAK', cleanup);
}

onShutdown(() => {
  closeTauriDriver();
});

