import { copyFileSync, existsSync, mkdirSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const tauriDir = path.resolve(scriptDir, '..');
const dylibName = 'libswift_Concurrency.dylib';
const destDir = path.join(tauriDir, 'target', 'swift-runtime');
const destPath = path.join(destDir, dylibName);

if (process.platform !== 'darwin') {
  console.log('Skipping Swift runtime bundle on non-macOS build.');
  process.exit(0);
}

function findXcrunSwiftCandidates() {
  try {
    const swiftBin = execFileSync('xcrun', ['--find', 'swift'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();

    if (!swiftBin) {
      return [];
    }

    const toolchainUsr = path.dirname(path.dirname(swiftBin));
    const libRoot = path.join(toolchainUsr, 'lib');
    const candidates = [];

    for (const dirName of ['swift-5.5', 'swift']) {
      candidates.push(path.join(libRoot, dirName, 'macosx', dylibName));
      candidates.push(path.join(libRoot, dirName, dylibName));
    }

    if (existsSync(libRoot)) {
      for (const entry of readdirSync(libRoot, { withFileTypes: true })) {
        if (!entry.isDirectory() || !entry.name.startsWith('swift')) {
          continue;
        }

        candidates.push(path.join(libRoot, entry.name, 'macosx', dylibName));
        candidates.push(path.join(libRoot, entry.name, dylibName));
      }
    }

    return candidates;
  } catch {
    return [];
  }
}

const candidates = [
  ...findXcrunSwiftCandidates(),
  `/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx/${dylibName}`,
  `/Library/Developer/CommandLineTools/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx/${dylibName}`,
  `/usr/lib/swift/${dylibName}`,
];

const sourcePath = candidates.find((candidate) => existsSync(candidate));

if (!sourcePath) {
  throw new Error(`Could not find ${dylibName}. Install or select Xcode with xcode-select.`);
}

mkdirSync(destDir, { recursive: true });
copyFileSync(sourcePath, destPath);

console.log(`Copied Swift Concurrency runtime: ${sourcePath} -> ${destPath}`);
