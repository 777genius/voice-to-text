import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, readFile, realpath, writeFile } from 'node:fs/promises';
import path from 'node:path';

// No title, front document, clipboard, application quit, or document text access.
export function ownedClosureScript(target) {
  const quoted = JSON.stringify(target);
  return `if application "TextEdit" is not running then return "absent"
 tell application "TextEdit"
 set matches to every document whose path is ${quoted}
 if (count matches) > 1 then error "ambiguous owned TEST document"
 if (count matches) is 1 then close item 1 of matches saving no
 if (count (every document whose path is ${quoted})) is not 0 then error "owned TEST document remains open"
 return "absent"
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
    if (owner.marker !== 'VOICETEXT_NATIVE_WINDOW_E2E_V1' || owner.path !== target ||
        await realpath(target) !== target || !info.isFile() || info.isSymbolicLink() ||
        String(info.dev) !== owner.device || String(info.ino) !== owner.inode) throw Error('Owned TEST file identity not verified');
    const termination = JSON.parse(await readFile(path.join(directory, 'native-process-termination.json'), 'utf8'));
    if (!Number.isSafeInteger(owner.appPid) || owner.appPid <= 0 || termination.pid !== owner.appPid ||
        termination.exited !== true || termination.groupGone !== true) throw Error('Owned native process/reader termination not verified');
    evidence.path = target; evidence.appPid = owner.appPid; evidence.attempted = true;
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
