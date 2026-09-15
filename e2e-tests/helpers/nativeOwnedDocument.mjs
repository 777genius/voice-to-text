import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, readFile, realpath, writeFile } from 'node:fs/promises';
import path from 'node:path';

// No title, front document, clipboard, application quit, or document text access.
export function textEditPathAliases(target) {
  const aliases = [target];
  for (const [canonical, visible] of [['/private/var', '/var'], ['/private/tmp', '/tmp']]) {
    if (target === canonical || target.startsWith(`${canonical}/`)) aliases.push(`${visible}${target.slice(canonical.length)}`);
  }
  return aliases;
}
export function ownedDocumentMatches(target) {
  return textEditPathAliases(target)
    .map(candidate => `(every document whose path is ${JSON.stringify(candidate)})`)
    .join(' & ');
}
export function ownedClosureScript(target) {
  const matches = ownedDocumentMatches(target);
  return `if application "TextEdit" is not running then return "absent"
 tell application "TextEdit"
 set matches to ${matches}
 if (count matches) > 1 then error "ambiguous owned TEST document"
 if (count matches) is 1 then close item 1 of matches saving no
 set matches to missing value
 repeat 20 times
 set remainingMatches to ${matches}
 if (count remainingMatches) is 0 then return "absent"
 set remainingMatches to missing value
 delay 0.05
 end repeat
 error "owned TEST document remains open"
 end tell`;
}
export async function closeOwnedDocument(directory, execute = promisify(execFile)) {
  const evidence = { passed: false, attempted: false, clock: 'runner-performance-now', startMs: performance.now() };
  try {
    let owner;
    try { owner = JSON.parse(await readFile(path.join(directory, 'owned-document.json'), 'utf8')); }
    catch (error) {
      if (error.code !== 'ENOENT') throw error;
      evidence.passed = true; evidence.reason = 'no target ownership; no GUI action';
      return evidence;
    }
    const target = path.join(directory, 'p4-textedit-a.txt');
    const info = await lstat(target, { bigint: true });
    const directoryInfo = await lstat(directory, { bigint: true });
    const uid = typeof process.getuid === 'function' ? process.getuid() : Number(directoryInfo.uid);
    if (owner.marker !== 'VOICETEXT_NATIVE_WINDOW_E2E_V1' || owner.path !== target ||
        await realpath(target) !== target || !info.isFile() || info.isSymbolicLink() ||
        !directoryInfo.isDirectory() || directoryInfo.isSymbolicLink() || await realpath(directory) !== directory ||
        String(directoryInfo.dev) !== owner.directoryDevice || String(directoryInfo.ino) !== owner.directoryInode ||
        Number(directoryInfo.mode) !== owner.directoryMode || Number(directoryInfo.uid) !== owner.directoryUid ||
        Number(directoryInfo.uid) !== uid || (Number(directoryInfo.mode) & 0o077) !== 0 ||
        String(info.dev) !== owner.device) throw Error('Owned TEST file identity not verified');
    const termination = JSON.parse(await readFile(path.join(directory, 'native-process-termination.json'), 'utf8'));
    if (!Number.isSafeInteger(owner.appPid) || owner.appPid <= 0 || termination.pid !== owner.appPid ||
        termination.exited !== true || termination.groupGone !== true) throw Error('Owned native process/reader termination not verified');
    evidence.path = target; evidence.appPid = owner.appPid; evidence.attempted = true;
    evidence.fileReplacedByEditor = String(info.ino) !== owner.inode;
    const result = await execute('/usr/bin/osascript', ['-e', ownedClosureScript(target)], { timeout: 5000, maxBuffer: 8192 });
    if (result.stdout.trim() !== 'absent') throw Error('Owned TEST document closure not verified');
    evidence.passed = true;
  } catch (error) { evidence.error = String(error); }
  finally {
    evidence.endMs = performance.now();
    await writeFile(path.join(directory, 'owned-document-cleanup.json'), JSON.stringify(evidence, null, 2), { flag: 'wx' });
  }
  if (!evidence.passed) throw Error(evidence.error);
  return evidence;
}
