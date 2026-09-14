import { readFile, writeFile, lstat } from 'node:fs/promises';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';
function requireEvidence(ok) { if (!ok) throw new Error('Invalid E54 restart/crash checkpoint'); }
export function validateRestartCheckpoint(e, expected) {
  const s = e?.state, f = s?.fixture, r = e?.report;
  requireEvidence(e?.marker === marker && r?.mode === 'restart-crash' && r.passed === true &&
    Array.isArray(r.errors) && r.errors.length === 0 && !r.error && !r.skipped && !e.error && s?.continuationCase === 'E54' &&
    s.restartPhase === expected.phase && s.processPid === expected.pid && Number.isSafeInteger(s.processPid) && s.processPid > 1 &&
    typeof s.processEpoch === 'string' && /^[a-f0-9-]{36}$/.test(s.processEpoch) &&
    s.configDir === expected.directory && s.resultPath === expected.resultPath &&
    s.liveMode === false && s.continuationMode === true && f?.observationOverflow === false &&
    Array.isArray(f.markerViolations) && f.markerViolations.length === 0);
  if (expected.phase === 'A') {
    requireEvidence(s.pausedContinuation?.logicalRunId > 0 && s.pausedContinuation.pauseEpoch > 0 &&
      s.desiredOn === false && s.pendingStart === false && f.captureStops === 1 && f.captureStarts === 1 && f.activeCaptures === 0 && f.activeProviders === 1 && f.providerStarts === 1 &&
      f.providerAudioChunks > 0 && f.controlResults.some(c => c.operation === 'pause' && c.delivered && c.result.decision === 'accepted'));
  } else {
    requireEvidence(expected.previous && s.processPid !== expected.previous.processPid && s.processEpoch !== expected.previous.processEpoch &&
      s.sessionId === 0 && s.historyEntryCount === 0 && s.status === 'Idle' && s.desiredOn === false && s.pendingStart === false && s.continuationPending === false &&
      s.preparedCaptureTokenCount === 0 && s.pausedContinuation === null && s.captureEpisode === null &&
      s.logicalProviderRunId === 0 && s.persistedTargetEligible === false && s.sentinelLoaded === true &&
      r.ui?.error === null && r.ui.configSynced === true && r.ui.reconciledStatus === 'Idle' && r.ui.status === 'Idle' && r.ui.desiredOn === false && r.ui.pendingStart === false && r.ui.canRequestContinuation === false);
    for (const k of ['captureStarts', 'captureStops', 'activeCaptures', 'audioChunks', 'providerStarts', 'providerResumes', 'providerStops', 'activeProviders', 'providerAudioChunks', 'providerPcmBytes', 'livePcmBytes', 'finals', 'autoPastes', 'providerFailures']) requireEvidence(f[k] === 0);
    for (const k of ['captureMarkers', 'providerMarkers', 'firstBWrites', 'controlResults', 'intentObservations', 'fullPcm', 'sourceEpisodes', 'captureEvents', 'captureRunAssociations']) requireEvidence(Array.isArray(f[k]) && f[k].length === 0);
  }
  return s;
}
export async function runRestartCrash(binary, directory, baseEnv, runOwned) {
  const verification = { mode: 'restart-crash', passed: false, qualificationPassed: false, ttlCleanupTested: false, errors: [], processes: [] };
  try {
    let previous;
    for (const phase of ['A', 'B']) {
      const resultPath = path.join(directory, `restart-${phase}-${randomUUID()}.json`);
      const env = { ...baseEnv, VOICETEXT_NATIVE_RESTART_PHASE: phase, VOICE_TO_TEXT_NATIVE_E2E_RESULT: resultPath };
      const terminationPath = path.join(directory, `restart-${phase}-termination.json`);
      let checkpoint;
      await runOwned(binary, [], { cwd: directory, env }, 30000, path.join(directory, `restart-${phase}.log`), undefined,
        async (alive, pid) => {
          let envelope;
          try { envelope = JSON.parse(await readFile(resultPath, 'utf8')); }
          catch (error) { if (error.code === 'ENOENT' || error instanceof SyntaxError) return false; throw error; }
          if (!alive()) throw new Error('E54 process exited before checkpoint');
          checkpoint = validateRestartCheckpoint(envelope, { phase, pid, directory, resultPath, previous });
          return true;
        }, terminationPath, phase === 'A');
      const termination = JSON.parse(await readFile(terminationPath, 'utf8'));
      requireEvidence(checkpoint && termination.pid === checkpoint.processPid && termination.exited === true &&
        termination.groupGone === true && termination.checkpointCollected === true && termination.failure === null &&
        (phase !== 'A' || termination.signal === 'SIGKILL'));
      verification.processes.push({ phase, resultPath, termination, checkpoint });
      if (phase === 'A') {
        previous = checkpoint;
        // Deliberately exercise serde(skip) against stale persisted runtime metadata.
        const configPath = path.join(directory, 'stt_config.json');
        const info = await lstat(configPath);
        if (!info.isFile() || info.isSymbolicLink()) throw new Error('Unsafe restart config');
        const config = JSON.parse(await readFile(configPath, 'utf8'));
        config.continuation_target_eligible = true;
        config.language = 'e54-restart-sentinel';
        await writeFile(configPath, JSON.stringify(config));
      }
    }
    verification.passed = true;
  } catch (error) { verification.errors.push(String(error)); throw error; }
  finally { await writeFile(path.join(directory, 'restart-verification.json'), JSON.stringify(verification, null, 2), { flag: 'wx' }); }
  return verification;
}
