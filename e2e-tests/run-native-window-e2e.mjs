import { runRestartCrash } from './helpers/nativeRestartCrash.mjs';
import { closeOwnedDocument, ownedDocumentMatches } from './helpers/nativeOwnedDocument.mjs';
import { isDeepStrictEqual, promisify } from 'node:util';
import { verifyQualificationTerminals, verifyQualificationSources, verifyQualificationConnections, verifyQualificationRoute, verifyWarmProviderCanary, verifyWarmProviderTransport, verifyWarmProviderFinalFixtureAgreement, expectedWarmProviderContinues, maxProxyEvidenceEvents, liveTrials, readApprovedFixtures, validateHarnessConfig, exactInsertionEvidence } from './helpers/nativeContinuation.mjs';
import { createWriteStream } from 'node:fs';
import { spawn, execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { cp, lstat, mkdir, mkdtemp, readFile, readdir, realpath, stat, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const source = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';

const positiveSafeInteger = value => Number.isSafeInteger(value) && value > 0;
const nonnegativeSafeInteger = value => Number.isSafeInteger(value) && value >= 0;
const emptyPcmHash = 'cbf29ce484222325';

function generationMap(rows, validate) {
  if (!Array.isArray(rows) || rows.length === 0) return null;
  const generations = new Map();
  for (const row of rows) {
    if (!positiveSafeInteger(row?.captureGeneration) || !validate(row) ||
        generations.has(row.captureGeneration)) return null;
    generations.set(row.captureGeneration, row);
  }
  return generations;
}

function validateExactPcmEvidence(fixture, requireEveryCaptureDelivered, providerStopPerGeneration = true) {
  const validLedger = ledger => nonnegativeSafeInteger(ledger.chunks) &&
    nonnegativeSafeInteger(ledger.samples) && typeof ledger.hash === 'string' &&
    /^[0-9a-f]{16}$/.test(ledger.hash) &&
    (ledger.chunks === 0
      ? ledger.samples === 0 && ledger.hash === emptyPcmHash
      : ledger.samples > 0 && ledger.hash !== emptyPcmHash);
  const validCaptureMarker = row => positiveSafeInteger(row.count) &&
    positiveSafeInteger(row.firstSequence) && positiveSafeInteger(row.lastSequence) &&
    row.firstSequence <= row.lastSequence;
  const validProviderMarker = row => validCaptureMarker(row) &&
    positiveSafeInteger(row.captureRunId) && positiveSafeInteger(row.captureFenceGeneration) &&
    positiveSafeInteger(row.providerSessionId);
  const validAssociation = row => positiveSafeInteger(row.captureRunId) &&
    positiveSafeInteger(row.captureFenceGeneration);
  const captures = generationMap(fixture?.capturePcmLedgers, validLedger);
  const providers = generationMap(fixture?.providerPcmLedgers, validLedger);
  const captureMarkers = generationMap(fixture?.captureMarkers, validCaptureMarker);
  const providerMarkers = generationMap(fixture?.providerMarkers, validProviderMarker);
  const providerSessionCount = providerMarkers ?
    new Set([...providerMarkers.values()].map(row => row.providerSessionId)).size : 0;
  const associations = generationMap(fixture?.captureRunAssociations, validAssociation);
  const failureGenerations = fixture?.providerFailureCaptureGenerations;
  if (!captures || !providers || !captureMarkers || !providerMarkers || !associations ||
      captures.size !== associations.size || captures.size !== fixture.captureStarts ||
      providers.size !== providerMarkers.size ||
      !Array.isArray(failureGenerations) ||
      failureGenerations.length !== fixture.providerFailures ||
      failureGenerations.some(generation => !positiveSafeInteger(generation) || !captures.has(generation)) ||
      (providerStopPerGeneration &&
        (providerSessionCount + fixture.providerNoAudioStops < fixture.providerStops ||
         providerSessionCount + fixture.providerNoAudioStops >
           fixture.providerStops + fixture.warmTerminalCount))) return false;
  const sameGenerations = (left, right) =>
    left.size === right.size && [...left.keys()].every(generation => right.has(generation));
  const capturesWithPcm = new Map([...captures].filter(([, ledger]) => ledger.chunks > 0));
  const failed = new Set(failureGenerations);
  const undelivered = [...capturesWithPcm.keys()].filter(generation => !providers.has(generation));
  if (requireEveryCaptureDelivered ? undelivered.length !== 0 :
    undelivered.some(generation => !failed.has(generation))) return false;
  if (!sameGenerations(captures, associations) ||
      !sameGenerations(capturesWithPcm, captureMarkers) ||
      !sameGenerations(providers, providerMarkers)) return false;
  for (const [generation, provider] of providers) {
    const capture = captures.get(generation);
    const captureMarker = captureMarkers.get(generation);
    const marker = providerMarkers.get(generation);
    const association = associations.get(generation);
    if (!capture || !captureMarker || !marker || !association ||
        capture.samples !== provider.samples || capture.hash !== provider.hash ||
        marker.count !== captureMarker.count || marker.firstSequence !== captureMarker.firstSequence ||
        marker.lastSequence !== captureMarker.lastSequence ||
        marker.captureRunId !== association.captureRunId ||
        marker.captureFenceGeneration !== association.captureFenceGeneration) return false;
  }
  return true;
}

function validateTerminalFixture(fixture, requireEveryCaptureDelivered) {
  return fixture && Number.isSafeInteger(fixture.captureStarts) && fixture.captureStarts > 0 &&
    fixture.captureStarts === fixture.captureStops && fixture.activeCaptures === 0 &&
    fixture.activeProviders === 0 && fixture.maxActiveCaptures === 1 &&
    fixture.maxActiveProviders === 1 && fixture.observationOverflow === false &&
    positiveSafeInteger(fixture.providerStarts) &&
    nonnegativeSafeInteger(fixture.providerResumes) &&
    nonnegativeSafeInteger(fixture.providerFailures) &&
    nonnegativeSafeInteger(fixture.providerStops) &&
    nonnegativeSafeInteger(fixture.providerNoAudioStops) &&
    nonnegativeSafeInteger(fixture.warmTerminalCount) &&
    fixture.providerStops + fixture.warmTerminalCount ===
      fixture.providerStarts + fixture.providerResumes &&
    Array.isArray(fixture.markerViolations) && fixture.markerViolations.length === 0 &&
    validateExactPcmEvidence(fixture, requireEveryCaptureDelivered);
}

function terminalEvidenceSignature(fixture) {
  const keys = ['captureStarts', 'captureStops', 'providerStarts', 'providerResumes',
    'providerFailures', 'providerFailureCaptureGenerations',
    'providerStops', 'providerNoAudioStops', 'warmTerminalCount', 'activeCaptures',
    'activeProviders', 'maxActiveCaptures', 'maxActiveProviders', 'physicalOpenCount',
    'physicalCloseCount', 'observationOverflow',
    'markerViolations', 'captureRunAssociations', 'captureMarkers', 'providerMarkers',
    'capturePcmLedgers', 'providerPcmLedgers'];
  return JSON.stringify(Object.fromEntries(keys.map(key => [key, fixture?.[key]])));
}

export function validateMiniUxResult(envelope) {
  const report = envelope.report;
  const final = report?.final;
  const cases = report?.cases;
  const physical = report?.lifecycle?.physical;
  const validCounts = counts => Number.isSafeInteger(counts?.open) && counts.open >= 0 &&
    Number.isSafeInteger(counts?.close) && counts.close >= 0 && counts.close <= counts.open;
  const sameCounts = (left, right) => left.open === right.open && left.close === right.close;
  const oneOpen = (before, after) => after.open === before.open + 1 && after.close === before.close;
  const oneClose = (before, after) => after.open === before.open && after.close === before.close + 1;
  const physicalTransitionsValid = physical && Object.values(physical).every(validCounts) &&
    sameCounts(physical.warmReopenStart, physical.warmReopenEnd) &&
    sameCounts(physical.warmReopenEnd, physical.policyActive) &&
    oneClose(physical.policyActive, physical.policyClosed) &&
    sameCounts(physical.policyClosed, physical.policyColdActive) &&
    sameCounts(physical.policyColdActive, physical.policyColdStopped) &&
    oneOpen(physical.policyColdStopped, physical.policyResumed) &&
    oneClose(physical.policyResumed, physical.sleepClosed) &&
    oneOpen(physical.sleepClosed, physical.wakeOpened) &&
    oneClose(physical.wakeOpened, physical.terminalClosed) &&
    oneOpen(physical.terminalClosed, physical.recoveryOpened) &&
    report.warmReuseOpenCount === physical.policyResumed.open &&
    final?.fixture?.physicalOpenCount === physical.recoveryOpened.open &&
    final?.fixture?.physicalCloseCount === physical.recoveryOpened.close &&
    final.fixture.physicalOpenCount === final.fixture.physicalCloseCount + 1;
  // Mini UX is an explicit warm acceptance run; cold bypass is not evidence.
  const warmFrames = report?.warmActivationFrames;
  const visibleFrames = report?.warmVisibleFrames;
  const forbiddenStatusTexts = report?.warmForbiddenStatusTexts;
  const readyFrames = report?.warmReadyFrames;
  const nativeWindowEpochs = report?.warmWindowEpochs;
  const exactPcmEvidenceValid = validateTerminalFixture(final?.fixture, true) &&
    validateTerminalFixture(envelope.fixture, true) &&
    terminalEvidenceSignature(final?.fixture) === terminalEvidenceSignature(envelope.fixture);
  const firstVisibleFrames = Array.from({ length: 10 }, (_, index) =>
    Array.isArray(visibleFrames)
      ? visibleFrames.find(frame => frame?.attempt === index + 1)
      : undefined);
  const attemptIdentities = Array.isArray(readyFrames) ? readyFrames.map(frame => frame &&
    `${frame.runId}:${frame.revision}`) : [];
  const distinctAttemptIdentities = attemptIdentities.every(Boolean) &&
    new Set(attemptIdentities).size === attemptIdentities.length;
  const warmCaptureEvidenceValid = (() => {
    const fixture = final?.fixture;
    if (!Array.isArray(readyFrames) || !Array.isArray(fixture?.captureRunAssociations) ||
        !Array.isArray(fixture?.capturePcmLedgers) || !Array.isArray(fixture?.providerPcmLedgers)) {
      return false;
    }
    const generations = new Set();
    return readyFrames.every(ready => {
      const owned = fixture.captureRunAssociations.filter(row =>
        row?.captureRunId === ready?.runId);
      if (owned.length !== 1 || generations.has(owned[0].captureGeneration)) return false;
      generations.add(owned[0].captureGeneration);
      const capture = fixture.capturePcmLedgers.find(row =>
        row.captureGeneration === owned[0].captureGeneration);
      const provider = fixture.providerPcmLedgers.find(row =>
        row.captureGeneration === owned[0].captureGeneration);
      return positiveSafeInteger(owned[0].captureGeneration) &&
        positiveSafeInteger(owned[0].captureFenceGeneration) &&
        positiveSafeInteger(capture?.chunks) && positiveSafeInteger(capture?.samples) &&
        positiveSafeInteger(provider?.chunks) && provider.samples === capture.samples &&
        provider.hash === capture.hash;
    });
  })();
  const advancingWindowEpochs = Array.isArray(nativeWindowEpochs) &&
    nativeWindowEpochs.every((row, index) => index === 0 ||
      row.windowEpoch > nativeWindowEpochs[index - 1].windowEpoch);
  const nativeWindowEvidenceValid = Array.isArray(nativeWindowEpochs) &&
    nativeWindowEpochs.length === 10 && nativeWindowEpochs.every((row, index) => {
      const ready = readyFrames?.[index];
      return row?.attempt === index + 1 && Number.isSafeInteger(row.windowEpoch) && row.windowEpoch > 0 &&
        (row.baselineRevision === null ||
          (Number.isSafeInteger(row.baselineRevision) && row.baselineRevision > 0)) &&
        ready?.runId === row.runId && ready?.revision === row.revision &&
        visibleFrames?.some(frame => frame.attempt === index + 1 && frame.windowEpoch === row.windowEpoch) &&
        report.trace.some(sample => sample?.native?.visible === true &&
          sample.native.windowEpoch === row.windowEpoch && sample.captureRunId === row.runId &&
          sample.intentRevision === row.revision);
    });
  const captureReadyRecording = frame => frame.captureReady === true &&
    ['finalizing-previous', 'connecting-provider', 'recording'].includes(frame.readinessReason) &&
    /\brecording\b/.test(frame.phase) && !/\b(starting|processing)\b/.test(frame.phase);
  const neutralAdmissionFrame = frame => frame.captureReady === false &&
    [undefined, null, 'idle', 'activating-warm-capture'].includes(frame.readinessReason) &&
    frame.statusText === '' && !/\b(recording|starting|processing)\b/.test(frame.phase);
  const visibleFrameEvidenceValid = Array.isArray(forbiddenStatusTexts) &&
    forbiddenStatusTexts.length === 2 && forbiddenStatusTexts.every(text =>
      typeof text === 'string' && text.length > 0) && new Set(forbiddenStatusTexts).size === 2 &&
    Array.isArray(visibleFrames) && visibleFrames.length >= 10 &&
    visibleFrames.length <= 512 && visibleFrames.every(frame =>
      Number.isSafeInteger(frame?.attempt) && frame.attempt >= 1 && frame.attempt <= 10 &&
      ['render', 'shown', 'sample'].includes(frame.source) &&
      Number.isSafeInteger(frame.windowEpoch) && frame.windowEpoch > 0 &&
      ((frame.revision === null && frame.runId === null) ||
        (Number.isSafeInteger(frame.revision) && frame.revision > 0 &&
          (frame.runId === null || (Number.isSafeInteger(frame.runId) && frame.runId > 0)))) &&
      typeof frame.captureReady === 'boolean' && typeof frame.phase === 'string' &&
      typeof frame.statusText === 'string') &&
    firstVisibleFrames.every((frame, index) => {
      if (!frame) return false;
      const ready = readyFrames?.[index];
      const nativeWindow = nativeWindowEpochs?.[index];
      const attemptFrames = visibleFrames.filter(candidate => candidate.attempt === index + 1);
      const neutralActivation = neutralAdmissionFrame(frame);
      const attemptFramesValid = attemptFrames.every(candidate => {
        const candidateNeutral = neutralAdmissionFrame(candidate);
        const candidateOwnershipValid = candidateNeutral
          ? ((candidate.runId === null && candidate.revision === null) ||
            (candidate.runId === null && candidate.revision === nativeWindow?.baselineRevision) ||
            (candidate.runId === null && candidate.revision === ready?.revision) ||
            (candidate.runId === ready?.runId && candidate.revision === ready?.revision))
          : candidate.runId === ready?.runId && candidate.revision === ready?.revision;
        return (candidateNeutral || captureReadyRecording(candidate)) && candidateOwnershipValid &&
          !forbiddenStatusTexts.includes(candidate.statusText) &&
          candidate.windowEpoch === frame.windowEpoch &&
          candidate.windowEpoch === nativeWindow?.windowEpoch;
      });
      const firstOwnershipValid = neutralActivation
        ? ((frame.runId === null && frame.revision === null) ||
          (frame.runId === null && frame.revision === nativeWindow?.baselineRevision) ||
          (frame.runId === null && frame.revision === ready?.revision) ||
          (frame.runId === ready?.runId && frame.revision === ready?.revision))
        : frame.runId === ready?.runId && frame.revision === ready?.revision;
      return (neutralActivation || captureReadyRecording(frame)) && firstOwnershipValid &&
        frame.windowEpoch === nativeWindow?.windowEpoch && attemptFramesValid;
    }) && distinctAttemptIdentities && warmCaptureEvidenceValid &&
    advancingWindowEpochs && nativeWindowEvidenceValid;
  if (report?.warmMode !== true || report.warmReopens !== 10 || report.idleAcceptedDelta !== 0 ||
       !Array.isArray(report.trace) || report.trace.length === 0 ||
       !Array.isArray(readyFrames) || readyFrames.length !== 10 ||
       readyFrames.some(frame => !Number.isSafeInteger(frame.runId) || frame.runId <= 0 ||
         !Number.isSafeInteger(frame.revision) || !/\brecording\b/.test(frame.phase) || /\b(starting|processing)\b/.test(frame.phase)) ||
       !visibleFrameEvidenceValid ||
       !physicalTransitionsValid ||
       report.lifecycle?.sleepClosed !== true || report.lifecycle?.wakeOpenedOnce !== true ||
       report.lifecycle?.terminalCount !== 1 || report.lifecycle?.recoveryOpenedOnce !== true ||
       !Array.isArray(warmFrames) || warmFrames.length === 0) {
    throw new Error('Incomplete physical warm input evidence');
  }
  if (envelope.marker !== marker || envelope.passed !== true || report?.passed !== true ||
      !Array.isArray(cases) || cases.length !== 3 ||
      !Array.isArray(warmFrames) || warmFrames.length > 256 ||
      warmFrames.some(frame => !['render', 'shown', 'sample'].includes(frame.source) ||
        !Number.isSafeInteger(frame.revision) || frame.revision < 0 ||
        !Number.isSafeInteger(frame.runId) || frame.runId <= 0 ||
        frame.captureReady !== false || frame.statusText !== '' || typeof frame.phase !== 'string' ||
        /\b(recording|starting|processing)\b/.test(frame.phase)) ||
      cases[0].stop !== 'hotkey' || cases[1].stop !== 'native-close' ||
      cases[2].stop !== 'background-start-during-hide' || cases[2].backgroundStartingBeforeHide !== true ||
      cases.some(c => !Number.isFinite(c.hideMs) || c.hideMs < 0 || c.hideMs > 1000 ||
        c.bufferedBeforeStop !== true || c.oldProviderStillFinalizing !== true ||
        !Number.isSafeInteger(c.observations) || c.observations < 2 ||
        c.backgroundDidNotReopen !== true || c.markerDeliveryComplete !== true) ||
      cases[1].successorStayedVisible !== true || report.errors?.length !== 0 ||
      final?.visible !== false || final?.status !== 'Idle' || final?.preparedCaptureTokenCount !== 0 ||
      final?.fixture?.activeCaptures !== 0 || final?.fixture?.activeProviders !== 0 ||
      final?.fixture?.maxActiveProviders !== 1 || final?.fixture?.markerViolations?.length !== 0 ||
      !exactPcmEvidenceValid) {
    throw new Error('Incomplete mini UX window evidence');
  }
  return report;
}

const excluded = /^(?:\.git|\.codex|\.claude|\.ssh|\.aws|\.npmrc|node_modules|target|dist|\.env(?:\..*)?|auth\.(?:json|toml)|credentials(?:\..*)?)$/i;
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');

export async function verifyPreparationArtifact(directory, resultPath, runtimeFailure) {
  const verification = { mode: 'reader-preparation', qualificationPassed: false, passed: false,
    resultPath, errors: runtimeFailure ? [String(runtimeFailure)] : [] };
  try {
    const envelope = JSON.parse(await readFile(resultPath, 'utf8'));
    validatePreparationResult(envelope, directory);
    verification.passed = !runtimeFailure;
  } catch (error) { verification.errors.push(String(error)); }
  await writeFile(path.join(directory, 'reader-preparation-verification.json'),
    JSON.stringify(verification, null, 2), { flag: 'wx' });
  if (!verification.passed) throw new Error(verification.errors.join('; '));
  return verification;
}

export function parseArguments(args) {
  if (args.length === 0) return {};
  if (args.length === 1 && args[0] === '--mini-ux') return { miniUx: true };
  if (args.length === 1 && args[0] === '--reader-preparation') return { readerPreparation: true };
  if (args[0] === '--reuse-build' && path.isAbsolute(args[1] || '') &&
      ['--qualification-live', '--continuation-case', '--reader-preparation'].includes(args[2])) {
    return { ...parseArguments(args.slice(2)), reuseBuild: args[1] };
  }
  if (args.length === 3 && args[0] === '--qualification-live' && path.isAbsolute(args[1]) && liveTrials.some(t => t.id === args[2])) return { harnessConfig: args[1], trialId: args[2] };
  if (args.length === 2 && args[0] === '--continuation-case' && ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle', 'seal-stop', 'seal-hold', 'seal-close', 'cancel', 'stale-epoch', 'terminal-before-write', 'E04', 'E41', 'E42', 'E54'].includes(args[1])) return { continuationFake: true, continuationCase: args[1] };
  if (args.length === 1 && args[0] === '--continuation-fake') return { continuationFake: true };
  if (args.length === 1 && args[0] === '--terminal-cleanup') return { terminalCleanup: true };
  if (args.length === 2 && args[0] === '--live-elevenlabs' && path.isAbsolute(args[1])) {
    return { liveFixturePath: args[1] };
  }
  if (args.length === 2 && args[0] === '--no-build' && path.isAbsolute(args[1])) {
    return { artifactDir: args[1] };
  }
  throw new Error('Usage: node e2e-tests/run-native-window-e2e.mjs [--reuse-build /canonical/voicetext-native-e2e-XXXXXX (--reader-preparation | --qualification-live /absolute/backend.json TRIAL | --continuation-case CASE)] or existing fresh-build modes / --no-build (unpaid only). Arbitrary binary/config flags are forbidden.');
}

export async function validateArtifactDirectory(directory) {
  const canonical = await realpath(directory);
  const temporary = await realpath(os.tmpdir());
  if (canonical !== directory || path.dirname(canonical) !== temporary ||
      !/^voicetext-native-e2e-[a-zA-Z0-9]+$/.test(path.basename(canonical))) {
    throw new Error('Refusing a noncanonical or non-disposable native test directory');
  }
  const info = await stat(canonical);
  if (typeof process.getuid === 'function' && info.uid !== process.getuid()) throw new Error('Test directory belongs to another user');
  if (!info.isDirectory() || (info.mode & 0o022)) throw new Error('Untrusted writable test directory');
  return canonical;
}

export function sanitizedEnvironment(directory, inherited = process.env) {
  return {
    PATH: inherited.PATH || '/usr/bin:/bin:/usr/sbin:/sbin',
    HOME: path.join(directory, 'home'),
    TMPDIR: os.tmpdir(),
    LANG: 'en_US.UTF-8',
    VITE_NATIVE_WINDOW_E2E: '1',
    VITE_E2E: '1',
    // Both Vite's build resolver and the WebView runtime must retain loopback.
    // Otherwise a production build correctly replaces HTTP with the real API.
    TAURI_DEBUG: 'true',
    VITE_API_URL: 'http://127.0.0.1:9',
    VOICE_TO_TEXT_API_URL: 'http://127.0.0.1:9',
    VOICE_TO_TEXT_BACKEND_URL: 'ws://127.0.0.1:9',
    VOICE_TO_TEXT_CONFIG_DIR: directory,
    VOICE_TO_TEXT_NATIVE_E2E_RESULT: path.join(directory, `result-${randomUUID()}.json`),
    XDG_CONFIG_HOME: path.join(directory, 'home', '.config'),
    XDG_CACHE_HOME: path.join(directory, 'home', '.cache'),
  };
}

export function executionEnvironment(directory, options) {
  const env = sanitizedEnvironment(directory);
  if (options.miniUx) env.VOICETEXT_NATIVE_MINI_UX = 'unpaid-v1';
  if (options.readerPreparation) {
    if (options.trialId || options.continuationFake || options.liveFixturePath || options.terminalCleanup || options.harnessConfig) throw new Error('Conflicting preparation mode');
    env.VOICETEXT_NATIVE_READER_PREPARATION = 'unpaid-v1';
  }
  const trial = liveTrials.find(t => t.id === options.trialId);
  if (trial || options.continuationFake) {
    env.VOICETEXT_NATIVE_CONTINUATION = trial ? 'p4-live-v1' : 'p4-fake-v1';
    if (!trial && options.continuationCase) env.VOICETEXT_NATIVE_CONTINUATION_CASE = options.continuationCase;
    if (trial?.continuation || (!trial && options.continuationFake)) {
      env.VOICETEXT_EL_PAUSE_CONTINUE_V1 = 'true';
      env.VOICETEXT_EL_FINALIZE_OUTCOME_V1 = 'true';
    }
  }
  return env;
}

export function createQualificationCollector(trial, proxyEvents, readEnvelope, now) {
  let resultObservedMs;
  return async (isNativeAlive = () => false) => {
    let pending;
    try { pending = await readEnvelope(); }
    catch (error) { if (error.code === 'ENOENT' || error instanceof SyntaxError) return false; throw error; }
    const atMs = now();
    resultObservedMs ??= atMs;
    const reportTrialId = trial.kind === 'warm-provider-canary'
      ? pending.report?.trialId
      : pending.report?.trial?.id;
    if (pending.marker !== marker || pending.passed !== true || pending.preTeardown?.normalStopReleased !== true ||
        pending.preTeardown?.cleanupDeferredToRunner !== true || reportTrialId !== trial.id)
      throw new Error('Missing successful pre-teardown native normal-Stop evidence');
    if (!isNativeAlive()) throw new Error('Native exited before normal-Stop closure collection');
    const boundary = { event: 'qualification_pre_teardown', atMs, clock: 'runner-performance-now', nativeProcessAlive: true };
    try { verifyQualificationConnections(trial, [...proxyEvents, boundary]); }
    catch (error) { if (atMs - resultObservedMs < 5000) return false; throw error; }
    proxyEvents.push(boundary);
    return true;
  };
}

export function assertOwnedProcessGroupGone(groupGone) {
  if (groupGone !== true) throw new Error('Owned native process group did not terminate');
}

export async function runOwned(command, args, options, timeoutMs, logPath, progressPath, collectBeforeTeardown, terminationPath, crashAfterCheckpoint = false) {
  if (crashAfterCheckpoint && (!terminationPath || !collectBeforeTeardown)) throw new Error('Crash requires owned group and checkpoint collector');
  const output = createWriteStream(logPath, { flags: 'wx' });
  const child = spawn(command, args, { ...options, ...(terminationPath ? { detached: true } : {}), stdio: ['ignore', 'pipe', 'pipe'] });
  const kill = signal => {
    try { if (terminationPath && child.pid) process.kill(-child.pid, signal); else child.kill(signal); }
    catch (error) {
      if (error.code === 'ESRCH') return;
      // A macOS process group can become unsignalable while its owned leader is
      // still our direct child. Retire that leader and let the later group probe
      // keep groupGone=false unless disappearance is independently observed.
      if (error.code === 'EPERM' && terminationPath && child.pid) {
        try { child.kill(signal); }
        catch (childError) { if (childError.code !== 'ESRCH') throw childError; }
        return;
      }
      throw error;
    }
  };
  let tearingDown = false, terminationRequested = false, force;
  const requestTermination = () => {
    if (tearingDown || terminationRequested) return;
    terminationRequested = true;
    kill(crashAfterCheckpoint && collected && !collectionError ? 'SIGKILL' : 'SIGTERM');
    force = setTimeout(() => { if (!tearingDown) kill('SIGKILL'); }, 5000);
  };
  const interrupted = signal => {
    if (tearingDown) return;
    collectionError ??= new Error(`Runner interrupted: ${signal}`);
    requestTermination();
  };
  const onInt = () => interrupted('SIGINT'), onTerm = () => interrupted('SIGTERM');
  process.on('SIGINT', onInt); process.on('SIGTERM', onTerm);
  for (const stream of [child.stdout, child.stderr]) stream.on('data', (chunk) => {
    output.write(chunk);
    process.stdout.write(chunk);
  });
  let collected = false; let collectionError; let collecting = false; let primaryFailure;
  output.on('error', error => { if (!tearingDown) { collectionError ??= error; kill('SIGKILL'); } });
  const collector = collectBeforeTeardown ? setInterval(async () => {
    if (tearingDown || terminationRequested || collecting || collected || collectionError) return;
    collecting = true;
    try {
      if (child.exitCode === null && child.signalCode === null && await collectBeforeTeardown(() => child.exitCode === null && child.signalCode === null, child.pid)) {
        if (tearingDown || terminationRequested) return;
        if (child.exitCode !== null || child.signalCode !== null) throw new Error('Native exited during pre-teardown collection');
        collected = true;
        requestTermination();
      }
    } catch (error) { if (!tearingDown) { collectionError ??= error; requestTermination(); } }
    finally { collecting = false; }
  }, 25) : undefined;
  let timedOut = false;
  const started = Date.now();
  let lastProgress = started;
  const heartbeat = progressPath ? setInterval(async () => {
    try { const info = await stat(progressPath); lastProgress = Math.max(lastProgress, info.mtimeMs); } catch {}
    if (tearingDown) return;
    // The test process can be descheduled for over a minute on a loaded macOS
    // GUI host even while its native/WebView work is still making bounded
    // scenario progress. This watchdog only detects a dead harness; scenario
    // polls and latency assertions below remain the product correctness gates.
    const silenceLimit = lastProgress === started ? 120_000 : 180_000;
    if (!timedOut && Date.now() - lastProgress > silenceLimit) {
      timedOut = true;
      collectionError ??= new Error(`${path.basename(command)} failed: progress timeout`);
      requestTermination();
    }
  }, 5_000) : undefined;
  const timeout = setTimeout(() => {
    if (tearingDown) return;
    timedOut = true;
    collectionError ??= new Error(`${path.basename(command)} failed: runtime timeout`);
    requestTermination();
  }, timeoutMs);
  try {
    const code = await new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', (status, signal) => resolve({ status, signal }));
    });
    if (collectionError) throw collectionError;
    if (collectBeforeTeardown && !collected) throw new Error('Native process exited before normal-Stop closure collection');
    const expectedCollectedExit = collected && (code.status === 0 || ['SIGTERM', 'SIGKILL'].includes(code.signal));
    if (timedOut || (collected ? !expectedCollectedExit : code.status !== 0)) throw new Error(`${path.basename(command)} failed: ${JSON.stringify({ ...code, timedOut })}`);
  } catch (error) { primaryFailure = error; throw error; }
  finally {
    tearingDown = true;
    try {
    clearInterval(collector);
    clearTimeout(timeout);
    clearInterval(heartbeat);
    clearTimeout(force);
    if (child.pid && child.exitCode === null && child.signalCode === null) {
      const exited = new Promise(resolve => child.once('exit', resolve));
      kill('SIGKILL'); await exited;
    }
    if (terminationPath && child.pid) {
      // Retire the owned process group, including an interrupted osascript child.
      kill('SIGKILL');
      let groupGone = false;
      const deadline = performance.now() + 500;
      do {
        try { process.kill(-child.pid, 0); }
        catch (error) {
          if (error.code === 'ESRCH') { groupGone = true; break; }
          if (error.code === 'EPERM') break;
          throw error;
        }
        await new Promise(resolve => setTimeout(resolve, 10));
      } while (performance.now() < deadline);
      await writeFile(terminationPath, JSON.stringify({ pid: child.pid,
        signal: child.signalCode, checkpointCollected: collected, failure: primaryFailure ? String(primaryFailure) : null,
        exited: child.exitCode !== null || child.signalCode !== null, groupGone }), { flag: 'wx' });
      if (groupGone !== true) throw new Error('Owned native process group did not terminate');
    }
    } catch (error) {
      if (primaryFailure) throw new AggregateError([primaryFailure, error], `${primaryFailure.message}; process cleanup: ${error.message}`, { cause: primaryFailure });
      throw error;
    } finally {
      process.removeListener('SIGINT', onInt); process.removeListener('SIGTERM', onTerm);
      await new Promise((resolve) => output.end(resolve));
    }
  }
}

export async function snapshotDigest(directory) {
  const hash = createHash('sha256');
  async function visit(current) {
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const entry of entries) {
      if (excluded.test(entry.name) || entry.isSymbolicLink()) continue;
      const file = path.join(current, entry.name);
      if (entry.isDirectory()) await visit(file);
      else if (entry.isFile()) { hash.update(path.relative(directory, file)); hash.update(await readFile(file)); }
    }
  }
  await visit(directory);
  return hash.digest('hex');
}

export async function validateCachedBinary(directory) {
  await validateArtifactDirectory(directory);
  await trustedArtifactEntry(path.join(directory, 'native-build.json'));
  const manifest = JSON.parse(await readFile(path.join(directory, 'native-build.json'), 'utf8'));
  if (manifest.marker !== marker || manifest.binary !== 'native-window-e2e' || !/^[a-f0-9]{64}$/.test(manifest.sourceSha256 || '') ||
      !/^com\.voicetotext\.app\.native-e2e\.[a-zA-Z0-9]+$/.test(manifest.identifier || '')) throw new Error('Invalid native build manifest');
  const binary = path.join(directory, manifest.binary);
  await trustedArtifactEntry(binary);
  if (await realpath(binary) !== binary) throw new Error('Native binary cannot be a symlink');
  const bytes = await readFile(binary);
  if (digest(bytes) !== manifest.sha256 || !bytes.includes(Buffer.from(marker))) throw new Error('Native binary hash/feature marker mismatch');
  if (!bytes.includes(Buffer.from(manifest.identifier))) throw new Error('Native binary application identity mismatch');
  const snapshot = path.join(directory, 'frontend');
  await sourceInputFingerprint(snapshot, undefined, true);
  if (await realpath(snapshot) !== snapshot || await snapshotDigest(snapshot) !== manifest.sourceSha256) {
    throw new Error('Native source snapshot hash mismatch');
  }
  return binary;
}

// Reuse v1 fingerprints the same source allowlist as the isolated build, with
// length-framed paths/content. No source symlinks are silently dropped. The exact
// schema outputs below differ only for inputs; snapshot integrity retains them.
const reuseSchema = 'native-build-reuse-v1';
const tauriConfigPath = 'src-tauri/tauri.conf.json';
// Only these build outputs are omitted from INPUT comparison. snapshotDigest
// still binds their bytes, and traversal still validates their types/trust.
const generatedSchemas = new Set(['acl-manifests.json', 'capabilities.json',
  'desktop-schema.json', 'macOS-schema.json'].map(name => `src-tauri/gen/schemas/${name}`));
const generatedParents = new Set(['src-tauri/gen', 'src-tauri/gen/schemas']);
async function trustedArtifactEntry(file, directory = false) {
  const info = await lstat(file);
  if (await realpath(file) !== file || !(directory ? info.isDirectory() : info.isFile()) ||
      (typeof process.getuid === 'function' && info.uid !== process.getuid()) ||
      (info.mode & 0o022) || (!directory && info.nlink !== 1)) {
    throw new Error('Untrusted native artifact entry (symlink, ownership, permissions or hardlink)');
  }
}

async function sourceInputEntries(directory, originalTauriConfig, trusted = false) {
  if (await realpath(directory) !== directory) throw new Error('Noncanonical source input');

  async function visit(current) {
    const frames = [];
    if (trusted) await trustedArtifactEntry(current, true);
    const entries = await readdir(current, { withFileTypes: true });
    entries.sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
    for (const entry of entries) {
      if (excluded.test(entry.name)) continue;
      const file = path.join(current, entry.name);
      const relative = path.relative(directory, file).split(path.sep).join('/');
      if (entry.isDirectory()) {
        if (generatedSchemas.has(relative)) throw new Error('Generated schema must be a regular file');
        const children = await visit(file);
        if (!generatedParents.has(relative) || children.length) frames.push(`directory:${relative}`, ...children);
      }
      else {
        if (!entry.isFile()) throw new Error('Source input symlink or special file forbidden');
        if (trusted) await trustedArtifactEntry(file);
        if (generatedSchemas.has(relative)) continue;
        frames.push(`file:${relative}`);
        frames.push(relative === tauriConfigPath && originalTauriConfig !== undefined
          ? originalTauriConfig : await readFile(file));
      }
    }
    return frames;
  }
  return visit(directory);
}

export async function sourceInputFingerprint(directory, originalTauriConfig, trusted = false) {
  const hash = createHash('sha256');
  for (const bytes of await sourceInputEntries(directory, originalTauriConfig, trusted)) {
    hash.update(`${Buffer.byteLength(bytes)}:`); hash.update(bytes);
  }
  return hash.digest('hex');
}

const allowedRunnerPaths = new Set([
  'e2e-tests/helpers/nativeContinuation.mjs',
  'e2e-tests/helpers/nativeContinuation.test.mjs',
  'e2e-tests/helpers/nativeContinuationProxy.mjs',
  'e2e-tests/helpers/nativeContinuationProxy.test.mjs',
  'e2e-tests/helpers/nativeBuildReuse.test.mjs',
  'e2e-tests/helpers/nativeQualificationGuards.test.mjs',
  'e2e-tests/run-native-window-e2e.mjs',
]);
async function runnerEvidence(snapshot, checkout, originalTauriConfig) {
  const entries = async (root, config, trusted) => {
    const frames = await sourceInputEntries(root, config, trusted);
    const result = new Map();
    for (let i = 0; i < frames.length; i++) {
      const key = frames[i];
      result.set(key.slice(key.indexOf(':') + 1), key.startsWith('file:')
        ? `file:${digest(frames[++i])}` : 'directory');
    }
    return result;
  };
  const original = await entries(snapshot, originalTauriConfig, true);
  const current = await entries(checkout);
  const allowedChangedPaths = [];
  for (const name of [...new Set([...original.keys(), ...current.keys()])].sort()) {
    if (original.get(name) === current.get(name)) continue;
    if (!allowedRunnerPaths.has(name) ||
        [original.get(name), current.get(name)].some(value => value === 'directory')) {
      throw new Error('Source input fingerprint mismatch: rebuild the final integrated checkout');
    }
    allowedChangedPaths.push(name);
  }
  return { runnerSourceSha256: await sourceInputFingerprint(checkout), allowedChangedPaths };
}

function buildBinding(manifest) {
  const { sha256, sourceSha256, sourceInputSha256, identifier } = manifest;
  return { sha256, sourceSha256, sourceInputSha256, identifier };
}

export async function validateReusableBuild(directory, checkout) {
  return validateReuseCandidate(directory, checkout, false);
}

async function validateReuseCandidate(directory, checkout, preparing) {
  await validateCachedBinary(directory);
  const manifest = JSON.parse(await readFile(path.join(directory, 'native-build.json'), 'utf8'));
  if (manifest.schema !== reuseSchema || typeof manifest.originalTauriConfig !== 'string' ||
      !/^[a-f0-9]{64}$/.test(manifest.sourceInputSha256 || '') ||
      !isDeepStrictEqual(manifest.buildOrigin, buildBinding(manifest))) {
    throw new Error('Reuse requires fresh build provenance: rebuild with the final integrated runner');
  }
  if (manifest.reusedFrom !== undefined) {
    await trustedArtifactEntry(path.join(directory, 'reuse-origin.json'));
    const originBytes = await readFile(path.join(directory, 'reuse-origin.json'));
    if (digest(originBytes) !== manifest.originManifestSha256 ||
        !isDeepStrictEqual(buildBinding(JSON.parse(originBytes)), manifest.buildOrigin)) {
      throw new Error('Reuse origin provenance mismatch');
    }
  }
  const snapshot = path.join(directory, 'frontend');
  const config = JSON.parse(await readFile(path.join(snapshot, tauriConfigPath), 'utf8'));
  const expected = isolatedTauriConfig(JSON.parse(manifest.originalTauriConfig), manifest.identifier.split('.').at(-1));
  if (!isDeepStrictEqual(config, expected)) throw new Error('Isolated Tauri configuration transformation mismatch');
  if (await sourceInputFingerprint(snapshot, manifest.originalTauriConfig, true) !== manifest.sourceInputSha256) {
    throw new Error('Source input fingerprint mismatch: rebuild the final integrated checkout');
  }
  const evidence = await runnerEvidence(snapshot, checkout, manifest.originalTauriConfig);
  // Legacy artifacts can authorize preparation; launch requires fresh runner evidence
  // whenever current source differs from the immutable build inputs.
  if (!preparing && manifest.runnerSourceSha256 === undefined &&
      manifest.allowedChangedPaths === undefined && evidence.allowedChangedPaths.length) {
    throw new Error('Runner source evidence required for changed checkout');
  }
  if (manifest.runnerSourceSha256 !== undefined || manifest.allowedChangedPaths !== undefined) {
    if (manifest.runnerSourceSha256 !== evidence.runnerSourceSha256 ||
        !isDeepStrictEqual(manifest.allowedChangedPaths, evidence.allowedChangedPaths)) {
      throw new Error('Runner source evidence mismatch');
    }
  }
  return manifest;
}

export function assertUnpaidReplay(manifest) {
  if (manifest.mode === 'reader-preparation') throw new Error('Preparation requires explicit --reuse-build ABS --reader-preparation');
  if (['continuation-live', 'live-elevenlabs'].includes(manifest.mode)) {
    throw new Error('Paid qualification cannot be replayed with --no-build');
  }
}

export async function prepareReuseBuild(options, checkout) {
  // Revalidate the public helper's selection too; no implicit default or matrix.
  if (options.readerPreparation) executionEnvironment('/tmp', options);
  else if (options.trialId) parseArguments(['--qualification-live', options.harnessConfig, options.trialId]);
  else if (options.continuationCase) parseArguments(['--continuation-case', options.continuationCase]);
  else throw new Error('Reuse requires one explicit trial or case');
  const origin = await validateArtifactDirectory(options.reuseBuild);
  const manifest = await validateReuseCandidate(origin, checkout, true);
  const originBytes = await readFile(path.join(origin, 'native-build.json'));
  if (!isDeepStrictEqual(JSON.parse(originBytes), manifest)) throw new Error('Build manifest changed during validation');
  const directory = await realpath(await mkdtemp(path.join(os.tmpdir(), 'voicetext-native-e2e-')));
  await cp(path.join(origin, 'native-window-e2e'), path.join(directory, 'native-window-e2e'), { errorOnExist: true, force: false });
  const snapshot = path.join(origin, 'frontend');
  await cp(snapshot, path.join(directory, 'frontend'), { recursive: true, dereference: false,
    filter: entry => !path.relative(snapshot, entry).split(path.sep).some(part => excluded.test(part)),
  });
  // Whitelist build provenance only. Runtime flags, endpoints, files and previous
  // selection never cross this boundary. The embedded ID deliberately stays old.
  await writeFile(path.join(directory, 'reuse-origin.json'), originBytes, { flag: 'wx' });
  await writeFile(path.join(directory, 'native-build.json'), JSON.stringify({
    schema: reuseSchema, marker, binary: 'native-window-e2e', ...buildBinding(manifest),
    originalTauriConfig: manifest.originalTauriConfig, buildOrigin: manifest.buildOrigin,
    reusedFrom: origin, originManifestSha256: digest(originBytes),
    ...await runnerEvidence(snapshot, checkout, manifest.originalTauriConfig),
    mode: options.readerPreparation ? 'reader-preparation' : options.trialId ? 'continuation-live' : 'continuation-fake',
    trialId: options.trialId ?? null, continuationCase: options.continuationCase ?? null,
  }), { flag: 'wx' });
  // Validate both sides after copying and again in main immediately before effects.
  const after = await validateReuseCandidate(origin, checkout, true);
  if (!isDeepStrictEqual(after, manifest)) throw new Error('Build manifest changed during copy');
  await validateReusableBuild(directory, checkout);
  return directory;
}

export function validatePreparationResult(envelope, directory) {
  const r = envelope?.report, f = envelope?.fixture, reader = envelope?.nativeReadback;
  if (envelope?.marker !== marker || envelope.readerPreparation !== true || envelope.qualificationPassed !== false ||
      envelope.passed !== true || r?.mode !== 'reader-preparation' || r.qualificationPassed !== false || r.passed !== true ||
      r.preflightComplete !== true || r.unexpectedEvents !== 0 || envelope.diagnosticEffectRefused !== false ||
      !Array.isArray(r.errors) || r.errors.length || r.error || r.observationMs < 5000 || !Number.isFinite(r.observationMs) ||
      r.clockExchanges?.length !== 3 || r.clockExchanges.some(p => p.phase !== 'pre') ||
      ['captureStarts', 'livePcmBytes', 'providerPcmBytes', 'providerStarts', 'providerResumes',
       'activeCaptures', 'activeProviders', 'audioChunks', 'providerAudioChunks'].some(k => f?.[k] !== 0) ||
      !Array.isArray(f?.intentObservations) || f.intentObservations.length || f.observationOverflow !== false ||
      envelope.readerWorkerJoined !== true || reader?.stopped !== true || reader.armed !== true || reader.valid !== true ||
      reader.error || reader.shutdownError || reader.records?.[0]?.text !== '' ||
      reader.records.some(row => row.identityValid !== true || row.text !== '') ||
      reader.identity?.path !== path.join(directory, 'p4-textedit-a.txt') || reader.identity?.bundle !== 'com.apple.TextEdit' ||
      !Number.isSafeInteger(reader.identity?.pid) || reader.identity.pid <= 0 ||
      envelope.ownedPath !== path.join(directory, 'p4-textedit-a.txt') ||
      r.targetDocument !== 'p4-textedit-a.txt') throw new Error('Unpaid reader preparation readiness failed');
  return r;
}

function markerFor(s) {
  const rows = s.fixture.providerMarkers.filter(m => m.captureRunId === s.captureEpisode?.runId &&
    m.captureFenceGeneration === s.captureEpisode?.generation);
  return rows.length === 1 ? rows[0] : undefined;
}
function sameMarkerOwner(a, b) {
  const x = markerFor(a), y = markerFor(b);
  return !!x && !!y && x.captureGeneration === y.captureGeneration && x.captureRunId === y.captureRunId &&
    x.captureFenceGeneration === y.captureFenceGeneration && x.providerSessionId === y.providerSessionId &&
    x.firstSequence === y.firstSequence;
}
function markerAdvances(before, after) {
  const a = markerFor(before), b = markerFor(after);
  return !!a && !!b && a.captureGeneration === b.captureGeneration && a.captureRunId === b.captureRunId &&
    a.captureFenceGeneration === b.captureFenceGeneration && a.providerSessionId === b.providerSessionId &&
    a.firstSequence === b.firstSequence && b.count > a.count && b.lastSequence > a.lastSequence &&
    b.count - a.count === b.lastSequence - a.lastSequence;
}
function currentStopApplied(before, after, source) {
  const last = before.coordinatorTrace.slice(-1)[0]?.sequence ?? 0;
  const gesture = before.coordinatorTrace.filter(t => t.source === 'Some(HoldHotkey)' && t.phase === 'IntentApplied' && t.desiredAfter.startsWith('On') && t.gesture != null).slice(-1)[0]?.gesture;
  // The service retains the sealed episode while the logical provider is paused.
  // Its identity is historical ownership, not proof that the microphone is active.
  const episode = before.captureEpisode, marker = markerFor(before);
  if (!episode || !marker || before.fixture.activeCaptures !== 1 ||
      (after.captureEpisode != null && (after.captureEpisode.runId !== episode.runId ||
        after.captureEpisode.generation !== episode.generation))) return false;
  const events = after.fixture.captureEvents.filter(e => e.generation === marker.captureGeneration);
  const released = events.filter(e => e.kind === 'capture-off');
  const joined = events.filter(e => e.kind === 'capture-joined');
  if (!Number.isFinite(before.nativeClockMs) || !Number.isFinite(after.nativeClockMs) ||
      released.length !== 1 || joined.length !== 1 ||
      !Number.isFinite(released[0].atMs) || !Number.isFinite(joined[0].atMs) ||
      released[0].atMs < before.nativeClockMs || joined[0].atMs < released[0].atMs ||
      joined[0].atMs > after.nativeClockMs) return false;
  return after.fixture.activeCaptures === 0 && after.preparedCaptureTokenCount === 0 &&
    after.coordinatorTrace.some(t => t.sequence > last &&
      t.phase === 'IntentApplied' && t.source === source && t.desiredAfter === 'Off' &&
      (source !== 'Some(HoldHotkey)' || (gesture != null && t.gesture === gesture)) &&
      after.coordinatorTrace.some(e => e.sequence === t.sequence && e.phase === 'CaptureStopEnqueued' &&
        e.runId === episode.runId) &&
      after.coordinatorTrace.some(e => e.sequence > t.sequence && e.phase === 'CaptureStopped' &&
        e.runId === episode.runId && e.desiredAfter === 'Off' && e.captureAfter === 'Idle'));
}

function requireAfterWrite(ok, message) { if (!ok) throw new Error(message); }
function serviceTerminal(a, b, final) {
  const s = final.afterWriteService, r = s?.completedReport, d = r?.audio, p = r?.provider;
  const terminal = s?.terminal?.filter(t => t.runId === a.logicalProviderRunId);
  return s?.owner === a.logicalProviderRunId && s.status === 'Idle' && s.logicalProviderRunId === 0 &&
    s.pausedContinuation === null && s.coordinatorIdle === true && s.pendingStart === false &&
    s.processingJobs === 0 && s.continuationPending === false &&
    terminal?.length === 1 && terminal[0].sequence > (b.coordinatorTrace.at(-1)?.sequence ?? 0) &&
    terminal[0].outcome === 'Some(FinalizeCommitted)' && terminal[0].error === null &&
    r?.run_id === a.logicalProviderRunId && r.provider_release === 'released' && r.error === null &&
    r.shared_failure === false && r.continuation_not_started === null &&
    p?.reason === 'drained' && p.provider_release === 'released' && p.error == null &&
    d?.reason === 'drained' && d.remaining_bytes === 0 && d.unknown_bytes === 0 &&
    (d.unacknowledged_bytes === null || d.unacknowledged_bytes === 0) &&
    Number.isSafeInteger(d.accepted_bytes) && d.accepted_bytes > 0 &&
    d.accepted_bytes === d.read_bytes && d.read_bytes === d.submitted_bytes &&
    d.submitted_bytes === final.fixture.fullPcm?.filter(r => r.seam === 'provider').reduce((n, r) => n + r.samples * 2, 0);
}
function verifyAfterWrite(a, b, final, selected) {
  const x = markerFor(a), y = markerFor(b);
  requireAfterWrite(x && y && x.captureGeneration === 1 && y.captureGeneration === 2 &&
    b.fixture.activeCaptures === 1 && b.fixture.activeProviders === 1 && b.fixture.captureStops === 1 &&
    x.providerSessionId > 0 && x.providerSessionId === y.providerSessionId &&
    x.firstSequence === 1 && y.firstSequence === 1 && x.count > 0 && y.count > 0 &&
    x.count === x.lastSequence && y.count === y.lastSequence && a.logicalProviderRunId > 0 &&
    a.logicalProviderRunId === b.logicalProviderRunId && a.captureEpisode && b.captureEpisode &&
    a.captureEpisode.generation !== b.captureEpisode.generation, 'B must continue A with distinct capture ownership');
  const source = selected === 'after-write-hold' ? 'Some(HoldHotkey)' :
    selected === 'after-write-toggle' ? 'Some(CarbonHotkey)' : 'Some(Frontend)';
  requireAfterWrite(currentStopApplied(b, final, source), 'Current B event did not stop its epoch');
  for (const s of [b, final]) {
    const f = s.fixture;
    requireAfterWrite(f.firstBWrites.length === 1 && f.firstBWrites[0].captureGeneration === y.captureGeneration &&
      f.firstBWrites[0].logicalRunId === a.logicalProviderRunId && f.firstBWrites[0].pauseEpoch > 0 &&
      f.controlResults.some(c => c.operation === 'continue' && c.delivered && c.result.decision === 'accepted' &&
        c.result.pause_epoch === f.firstBWrites[0].pauseEpoch) &&
      !f.controlResults.some(c => c.operation === 'restore'), 'Missing first B write or unexpected Restore');
    requireAfterWrite(!f.observationOverflow && f.markerViolations.length === 0 && f.providerStarts === 1 &&
      f.providerResumes === 0 && f.maxActiveProviders === 1 && f.maxActiveCaptures === 1 &&
      f.captureStarts === 2, 'Invalid capture/provider evidence');
  }
  requireAfterWrite(serviceTerminal(a, b, final), 'Service terminal incomplete before finish');
  const f = final.fixture;
  requireAfterWrite(f.firstBWrites[0].pauseEpoch === b.fixture.firstBWrites[0].pauseEpoch, 'B write epoch changed');
  for (const generation of [1, 2]) {
    for (const kind of ['capture-start', 'capture-off', 'capture-joined']) {
      requireAfterWrite(f.captureEvents.filter(e => e.generation === generation && e.kind === kind).length === 1,
        'Missing or duplicate capture lifecycle');
    }
  }
  requireAfterWrite(f.captureStops === 2 && f.activeCaptures === 0 && f.activeProviders === 0 &&
    final.preparedCaptureTokenCount === 0 && f.finals === 1 &&
    !f.captureEvents.some(e => e.kind === 'stop-timeout'), 'Terminal cleanup incomplete');
  requireAfterWrite(f.fullPcm?.length === 4, 'Missing full PCM validation');
  for (const generation of [1, 2]) for (const seam of ['capture', 'provider']) {
    const rows = f.fullPcm.filter(r => r.captureGeneration === generation && r.seam === seam);
    const range = (seam === 'capture' ? f.captureMarkers : f.providerMarkers).find(r => r.captureGeneration === generation);
    requireAfterWrite(rows.length === 1 && rows[0].valid === true && rows[0].chunks === range?.count &&
      rows[0].samples === rows[0].chunks * 320, 'Full PCM length, format or waveform mismatch');
  }
  requireAfterWrite(f.captureMarkers.length === 2 && f.providerMarkers.length === 2, 'Missing per-capture PCM evidence');
  for (const owner of [x, y]) {
    const emitted = f.captureMarkers.filter(m => m.captureGeneration === owner.captureGeneration);
    const received = f.providerMarkers.filter(m => m.captureGeneration === owner.captureGeneration);
    requireAfterWrite(emitted.length === 1 && received.length === 1, 'Ambiguous PCM generation');
    const e = emitted[0], r = received[0];
    requireAfterWrite(e.firstSequence === 1 && r.firstSequence === 1 && Number.isSafeInteger(e.count) &&
      Number.isSafeInteger(r.count) && e.count > 0 &&
      e.count === e.lastSequence && r.count === r.lastSequence && e.count === r.count &&
      r.count >= owner.count && r.providerSessionId === owner.providerSessionId &&
      r.captureRunId === owner.captureRunId && r.captureFenceGeneration === owner.captureFenceGeneration,
      'Source PCM lost, replayed, discarded or reassigned');
  }
}
export function validE42PhysicalEvidence(s) {
  const p = s.physicalKeyboard;
  if (!p || p.source !== 'fake' || p.realKeyboardReads !== 0 || p.overflow ||
    p.observations.length > 256 || p.events.length > 128 ||
    p.observations.some(o => o.source !== 'fake' || o.key !== 7 || o.modifiers !== 3 ||
      o.observation !== (o.downKeys.includes(7) && (o.downKeys.includes(55) || o.downKeys.includes(54)) &&
        (o.downKeys.includes(56) || o.downKeys.includes(60)) ? 'Down' : 'Up'))) return false;
  if (p.events.some(e => e.kind === 'watcher' ? e.watcherFinished !== true : e.watcherFinished !== null)) return false;
  if (p.events.some(e => e.observation === 'NotRead' ? e.sample !== null :
    !Number.isInteger(e.sample) || e.sample == null || e.sample < 0 ||
    p.observations[e.sample]?.observation !== e.observation)) return false;
  const accepted = p.events.filter(e => e.kind === 'pressed' && e.result === 'Accepted');
  if (accepted.length !== 2 || accepted.some(e => e.observation !== 'Down' || !e.handle ||
    !Number.isSafeInteger(e.handle.gesture) || e.handle.gesture <= 0 ||
    !Number.isSafeInteger(e.handle.watcher) || e.handle.watcher <= 0)) return false;
  const [a, b] = accepted;
  if (!s.coordinatorTrace.some(t => t.source === 'Some(HoldHotkey)' &&
    t.gesture === b.handle.gesture && t.phase === 'IntentApplied' && t.desiredAfter === 'Off')) return false;
  if (a.handle.gesture === b.handle.gesture || a.handle.watcher === b.handle.watcher) return false;
  const matches = (e, owner, kind, observation, result) =>
    e.kind === kind && e.observation === observation && e.result === result &&
    e.handle?.gesture === owner.handle.gesture && e.handle?.watcher === owner.handle.watcher;
  const rearm = p.events.findIndex(e => matches(e, a, 'watcher', 'Up', 'Rearmed'));
  const duplicate = p.events.findIndex(e => matches(e, a, 'pressed', 'Down', 'Duplicate'));
  const ignored = p.events.findIndex(e => matches(e, b, 'released', 'Down', 'Stale'));
  const ended = p.events.findIndex(e => matches(e, b, 'watcher', 'Up', 'HoldEnded'));
  const samples = p.events.flatMap(e => e.sample == null ? [] : [e.sample]);
  if (samples.some((sample, i) => i > 0 && sample < samples[i - 1]) ||
    duplicate <= p.events.indexOf(a) || duplicate >= rearm ||
    ignored <= p.events.indexOf(b) || ended <= ignored) return false;
  return rearm > p.events.indexOf(a) && rearm < p.events.indexOf(b) &&
    p.events.some(e => matches(e, a, 'pressed', 'Down', 'Duplicate')) &&
    p.events.slice(p.events.indexOf(b) + 1).some(e => matches(e, a, 'watcher', 'NotRead', 'Stale')) &&
    p.events.slice(p.events.indexOf(b) + 1).some(e => matches(e, b, 'released', 'Down', 'Stale')) &&
    p.events.slice(p.events.indexOf(b) + 1).some(e => matches(e, b, 'watcher', 'Up', 'HoldEnded')) &&
    p.observations.filter(o => o.observation === 'Down').length >= 2 &&
    p.observations.filter(o => o.observation === 'Up').length >= 2;
}

export function validateResult(envelope) {
  if (envelope.report?.mode === 'mini-ux') return validateMiniUxResult(envelope);
  if (envelope?.readerPreparation || envelope?.report?.mode === 'reader-preparation') throw new Error('Preparation is not qualification');
  const report = envelope?.report;
  const fixture = envelope?.fixture;
  if (report?.error || (Array.isArray(report?.errors) && report.errors.length)) throw new Error("Native report retains failure evidence");
  if (report?.mode === 'after-write-case') {
    requireAfterWrite(envelope.marker === marker && envelope.passed === true && report.passed === true &&
      ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle'].includes(report.case) &&
      Array.isArray(report.errors) && report.errors.length === 0 &&
      isDeepStrictEqual(fixture, report.final?.fixture) &&
      isDeepStrictEqual(envelope.afterWriteServiceBefore, report.final?.afterWriteService) &&
      isDeepStrictEqual(envelope.afterWriteServiceAfter, envelope.afterWriteServiceBefore), 'Invalid after-write envelope');
    verifyAfterWrite(report.a, report.b, report.final, report.case);
    return report;
  }
  if (report?.mode === 'native-event-case') {
    const expected = { E04: 2, E41: 3, E42: 2 }[report.case];
    if (!expected || envelope.marker !== marker || envelope.passed !== true || report.passed !== true ||
      !Array.isArray(report.errors) || report.errors.length || !report.final ||
      fixture?.captureStarts !== expected || fixture?.captureStops !== expected ||
      fixture?.maxActiveCaptures !== 1 || fixture?.activeCaptures !== 0 || fixture?.activeProviders !== 0 ||
      fixture?.observationOverflow !== false || fixture?.markerViolations?.length !== 0 ||
      report.final.preparedCaptureTokenCount !== 0 ||
      !Array.isArray(report.checkpoints) || report.checkpoints.length < 6 ||
      (report.case === 'E04' && (!Number.isFinite(report.releaseToFirstPcmMs) || report.releaseToFirstPcmMs < 0 || report.releaseToFirstPcmMs > 250))) {
      throw new Error('Incomplete native event evidence');
    }
    const point = label => {
      const found = report.checkpoints.filter(p => p.label === label);
      if (found.length !== 1 || !found[0].state?.fixture) throw new Error(`Missing native checkpoint: ${label}`);
      return found[0].state;
    };
    const requireEvidence = (ok) => { if (!ok) throw new Error('Native event trace/effects mismatch'); };
    const off = s => s.fixture.activeCaptures === 0 && s.preparedCaptureTokenCount === 0;
    const recording = (s, n) => s.fixture.activeCaptures === 1 && s.fixture.captureStarts === n &&
      s.fixture.providerMarkers?.some(m => m.captureGeneration === n && m.count > 0);
    requireEvidence(recording(point('A recording'), 1));
    requireEvidence(off(report.final) && report.final.fixture.activeProviders === 0 &&
      report.final.fixture.captureStarts === expected && report.final.fixture.captureStops === expected &&
      report.final.fixture.maxActiveCaptures === 1 && report.final.fixture.markerViolations?.length === 0);
    requireEvidence(Array.isArray(fixture.captureEvents) && !fixture.captureEvents.some(e => e.kind === 'stop-timeout'));
    if (report.case === 'E04') {
      const held = point('B queued behind actual A');
      requireEvidence(held.fixture.activeCaptures === 1 && held.fixture.captureStarts === 1 &&
        held.fixture.captureEvents.some(e => e.kind === 'stop-entered') &&
        !held.fixture.captureEvents.some(e => e.kind === 'capture-off' || e.generation === 2));
      const b = point('B recording');
      const started = b.fixture.captureEvents.find(e => e.kind === 'capture-start' && e.generation === 2);
      const released = b.fixture.captureEvents.filter(e => e.kind === 'capture-off' && e.generation === 1);
      const joined = b.fixture.captureEvents.filter(e => e.kind === 'capture-joined' && e.generation === 1);
      const first = b.fixture.captureEvents.filter(e => e.kind === 'first-pcm' && e.generation === 2);
      requireEvidence(recording(b, 2) && released.length === 1 && first.length === 1 && joined.length === 1 &&
        started && released[0].atMs <= joined[0].atMs && joined[0].atMs <= started.atMs && started.atMs <= first[0].atMs &&
        Number.isFinite(released[0].atMs) && Number.isFinite(first[0].atMs) &&
        first[0].atMs - released[0].atMs === report.releaseToFirstPcmMs &&
        b.fixture.providerMarkers.some(m => m.captureGeneration === 2 && m.firstSequence === 1 && m.count > 0));
    } else if (report.case === 'E41') {
      let previous = point('B recording');
      requireEvidence(recording(previous, 2) && previous.captureEpisode != null);
      for (const [label, source] of [['stale-key-release', 'Some(HoldHotkey)'], ['stale-vad', 'Some(Vad)']]) {
        const before = point('before ' + label);
        requireEvidence(recording(before, 2) && sameMarkerOwner(previous, before) && JSON.stringify(before.captureEpisode) === JSON.stringify(previous.captureEpisode));
        previous = before;
        const next = point(label);
        const sequence = previous.coordinatorTrace.slice(-1)[0]?.sequence ?? 0;
        requireEvidence(recording(next, 2) && JSON.stringify(next.captureEpisode) === JSON.stringify(previous.captureEpisode) &&
          next.fixture.captureStops === previous.fixture.captureStops && markerAdvances(previous, next) &&
          next.coordinatorTrace.some(t => t.sequence > sequence && t.phase === 'IntentRejected' && t.source === source));
        previous = next;
      }
      const keyStopped = point('current key release stops B');
      const vadStopped = point('current VAD stops C');
      requireEvidence(off(keyStopped) && recording(point('C recording'), 3) && off(vadStopped) &&
        currentStopApplied(point('before current key release'), keyStopped, 'Some(HoldHotkey)') &&
        currentStopApplied(point('before current VAD'), vadStopped, 'Some(Vad)'));
    } else {
      const a = point('A recording'), b = point('next gesture recording');
      const rearmed = point('A physical Up rearms'), held = point('delayed A release ignored');
      const beforeUp = point('before B physical Up'), stopped = point('next gesture release off');
      requireEvidence(rearmed.physicalKeyboard?.events.some(e =>
        e.kind === 'watcher' && e.observation === 'Up' && e.result === 'Rearmed'));
      requireEvidence(off(rearmed) && rearmed.fixture.captureStarts === 1 &&
        validE42PhysicalEvidence(stopped) && sameMarkerOwner(b, held) && markerAdvances(b, held) &&
        held.fixture.captureStops === b.fixture.captureStops &&
        currentStopApplied(beforeUp, stopped, 'Some(HoldHotkey)'));
      requireEvidence(fixture.providerResumes === 0 && report.final.fixture.providerResumes === 0 &&
        report.checkpoints.every(p => p.state.fixture.providerResumes === 0) &&
        a.fixture.providerStarts === 1 && b.fixture.providerStarts === 2 && markerFor(a) && markerFor(b) &&
        markerFor(a).providerSessionId !== markerFor(b).providerSessionId);
      const slept = point('sleep resources off');
      requireEvidence(off(slept) && slept.fixture.activeProviders === 0 &&
        slept.coordinatorTrace.some(t => t.reason === 'Some(SystemSleep)'));
      for (const label of ['wake without phantom', 'wake remains off without key-up']) {
        const s = point(label); requireEvidence(off(s) && s.fixture.captureStarts === 1 && s.fixture.activeProviders === 0);
      }
      requireEvidence(recording(point('next gesture recording'), 2) && off(point('next gesture release off')) &&
        fixture.providerStarts === 2 && !fixture.controlResults.some(c => c.operation === 'continue') &&
        report.final.coordinatorTrace.some(t => t.reason === 'Some(SystemSleep)'));
    }
    return report;
  }
  if (report?.mode === 'continuation-case') {
    const fallback = ['stale-epoch', 'terminal-before-write'].includes(report.case);
    if (envelope.marker !== marker || envelope.passed !== true || report.passed !== true ||
      !['seal-stop', 'seal-hold', 'seal-close', 'cancel', 'stale-epoch', 'terminal-before-write'].includes(report.case) ||
      (fallback ? report.fallbackAfterRefusal !== true : report.micReleasedBeforeAccepted !== true) || report.cleanup !== true ||
      !Array.isArray(report.errors) || report.errors.length || fixture?.activeCaptures !== 0 ||
      fixture?.activeProviders !== 0 || fixture?.captureStarts !== 2 || fixture?.captureStops !== 2 ||
      fixture?.providerStarts !== (fallback ? 2 : 1) || fixture?.maxActiveProviders !== 1 || fixture?.providerResumes !== 0 || fixture?.finals !== (fallback ? 2 : 1) ||
      fixture?.markerViolations?.length !== 0 ||
      (fallback ? report.firstBWrites !== 0 : report.case === 'cancel' ? report.restored !== true || report.firstBWrites !== 0 : report.firstBWrites !== 1)) {
      throw new Error('Incomplete native adversarial continuation evidence');
    }
    return report;
  }
  if (report?.mode === 'continuation-fake') {
    const cycles = report.cycles;
    const latencies = fixture?.firstPcmLatenciesMs;
    const continuationGenerations = Array.from({ length: 51 }, (_, index) => index + 1);
    const authoritativeControls = fixture?.controlResults;
    const retainedLogicalRunId = report.logicalRunId;
    const validCycles = Array.isArray(cycles) && cycles.length === 50 && cycles.every((cycle, index) =>
      cycle?.cycle === index && cycle.captureGeneration === index + 2 &&
      positiveSafeInteger(cycle.captureRunId) && positiveSafeInteger(cycle.captureFenceGeneration) &&
      Number.isSafeInteger(cycle.windowEpoch) && cycle.windowEpoch > 0 &&
      (index === 0 || cycle.windowEpoch > cycles[index - 1].windowEpoch) &&
      cycle.providerStarts === 1 && cycle.micOffOnStop === true &&
      Array.isArray(cycle.controls) && cycle.controls.length === 2 &&
      cycle.controls[0]?.operation === 'pause' && cycle.controls[1]?.operation === 'continue' &&
      cycle.controls[0].logicalRunId === cycle.controls[1].logicalRunId &&
      cycle.controls[0].logicalRunId === retainedLogicalRunId &&
      Number.isSafeInteger(retainedLogicalRunId) && retainedLogicalRunId > 0 &&
      cycle.controls[0].delivered === true && cycle.controls[1].delivered === true &&
      cycle.controls[0].result?.decision === 'accepted' && cycle.controls[1].result?.decision === 'accepted' &&
      cycle.controls[1].result.eligible_now === true &&
      cycle.controls[0].result.pause_epoch === cycle.controls[1].result.pause_epoch &&
      cycle.controls[0].result.pause_epoch === index + 1);
    const flattenedControls = Array.isArray(cycles) ? cycles.flatMap(cycle => cycle.controls ?? []) : [];
    const terminalControl = Array.isArray(authoritativeControls) ? authoritativeControls.at(-1) : null;
    const controlRequestIds = Array.isArray(authoritativeControls)
      ? authoritativeControls.map(control => control?.result?.request_id) : [];
    const validAuthoritativeControls = Array.isArray(authoritativeControls) &&
      authoritativeControls.length === 101 && flattenedControls.length === 100 &&
      flattenedControls.every((control, index) => isDeepStrictEqual(control, authoritativeControls[index])) &&
      controlRequestIds.every(requestId => typeof requestId === 'string' && requestId.length > 0) &&
      new Set(controlRequestIds).size === authoritativeControls.length &&
      authoritativeControls.filter(control => control.operation === 'continue')
        .every(control => control.result?.eligible_now === true) &&
      terminalControl?.operation === 'pause' && terminalControl.logicalRunId === retainedLogicalRunId &&
      terminalControl.delivered === true && terminalControl.result?.decision === 'accepted' &&
      terminalControl.result.pause_epoch === 51;
    const latencyValues = Array.isArray(latencies) ? latencies.map(row => row?.elapsedMs) : [];
    const sortedLatencies = latencyValues.every(value => Number.isFinite(value) && value >= 0)
      ? [...latencyValues].sort((left, right) => left - right) : [];
    const measuredP95 = sortedLatencies.length === 51
      ? sortedLatencies[Math.ceil(sortedLatencies.length * .95) - 1] : null;
    const validLatencies = Array.isArray(latencies) && latencies.length === 51 &&
      latencies.every((row, index) => row?.captureGeneration === index + 1) &&
      measuredP95 !== null && measuredP95 <= 250 && report.p95FirstPcmMs === measuredP95;
    const captureLedgers = generationMap(fixture?.capturePcmLedgers, ledger =>
      positiveSafeInteger(ledger.chunks) && positiveSafeInteger(ledger.samples) &&
      typeof ledger.hash === 'string' && /^[0-9a-f]{16}$/.test(ledger.hash) && ledger.hash !== emptyPcmHash);
    const providerLedgers = generationMap(fixture?.providerPcmLedgers, ledger =>
      positiveSafeInteger(ledger.chunks) && positiveSafeInteger(ledger.samples) &&
      typeof ledger.hash === 'string' && /^[0-9a-f]{16}$/.test(ledger.hash) && ledger.hash !== emptyPcmHash);
    const providerMarkers = generationMap(fixture?.providerMarkers, row =>
      positiveSafeInteger(row.count) && positiveSafeInteger(row.firstSequence) &&
      positiveSafeInteger(row.lastSequence) && row.firstSequence <= row.lastSequence &&
      positiveSafeInteger(row.captureRunId) && positiveSafeInteger(row.captureFenceGeneration) &&
      positiveSafeInteger(row.providerSessionId));
    const captureAssociations = generationMap(fixture?.captureRunAssociations, row =>
      positiveSafeInteger(row.captureRunId) && positiveSafeInteger(row.captureFenceGeneration));
    const orderedCaptureOwnership = captureAssociations?.size === 51 &&
      continuationGenerations.every((generation, index) => {
        const association = captureAssociations.get(generation);
        const previous = index === 0 ? null : captureAssociations.get(generation - 1);
        const cycle = index === 0 ? null : cycles[index - 1];
        return association && (!previous || (association.captureRunId > previous.captureRunId &&
          association.captureFenceGeneration > previous.captureFenceGeneration)) &&
          (!cycle || (cycle.captureGeneration === generation &&
            cycle.captureRunId === association.captureRunId &&
            cycle.captureFenceGeneration === association.captureFenceGeneration)) &&
          providerMarkers?.get(generation)?.captureRunId === association.captureRunId &&
          providerMarkers?.get(generation)?.captureFenceGeneration ===
            association.captureFenceGeneration;
      });
    const validContinuationPcm = captureLedgers?.size === 51 && providerLedgers?.size === 51 &&
      providerMarkers?.size === 51 && continuationGenerations.every(generation =>
        captureLedgers.has(generation) && providerLedgers.has(generation) && providerMarkers.has(generation)) &&
      new Set([...providerMarkers.values()].map(row => row.providerSessionId)).size === 1 &&
      orderedCaptureOwnership;
    const expectedProviderSessionId = validContinuationPcm
      ? [...providerMarkers.values()][0].providerSessionId : null;
    const expectedTranscript = expectedProviderSessionId === null
      ? null : `Native fixture session ${expectedProviderSessionId}`;
    const validControlProviderOwner = expectedProviderSessionId !== null &&
      authoritativeControls?.every(control =>
        control.result?.provider_session_id === `p4-${expectedProviderSessionId}`);
    const stableDelivery = Array.isArray(report.stableDeliveries) && report.stableDeliveries.length === 1
      ? report.stableDeliveries[0] : null;
    const terminal = Array.isArray(report.terminals) && report.terminals.length === 1
      ? report.terminals[0] : null;
    const transcriptEvents = report.transcriptEvents;
    const validTranscriptEvidence = stableDelivery?.sessionId === expectedProviderSessionId &&
      positiveSafeInteger(stableDelivery?.deliverySeq) && stableDelivery?.text === expectedTranscript &&
      terminal?.sessionId === expectedProviderSessionId && terminal?.complete === true &&
      terminal?.stableSnapshot === expectedTranscript && Array.isArray(transcriptEvents) &&
      transcriptEvents.length === 2 && transcriptEvents[0]?.event === 'final' &&
      transcriptEvents[0]?.sessionId === expectedProviderSessionId &&
      transcriptEvents[0]?.deliverySeq === stableDelivery.deliverySeq &&
      transcriptEvents[0]?.text === expectedTranscript && transcriptEvents[0]?.complete === null &&
      transcriptEvents[1]?.event === 'terminal' &&
      transcriptEvents[1]?.sessionId === expectedProviderSessionId &&
      transcriptEvents[1]?.deliverySeq === null && transcriptEvents[1]?.text === expectedTranscript &&
      transcriptEvents[1]?.complete === true;
    if (envelope.marker !== marker || envelope.passed !== true || report.passed !== true ||
        report.terminalCount !== 1 || !validTranscriptEvidence || report.final?.historyEntryCount !== 1 ||
        report.completedCycles !== 50 || !validCycles || !validAuthoritativeControls ||
        !validControlProviderOwner ||
        !validLatencies || !validContinuationPcm ||
        !validateTerminalFixture(fixture, true) || fixture?.observationOverflow !== false ||
        !Array.isArray(report.errors) || report.errors.length ||
        !Number.isFinite(report.p95FirstPcmMs) || report.p95FirstPcmMs < 0 || report.p95FirstPcmMs > 250 ||
        !isDeepStrictEqual(fixture, report.final?.fixture) || report.final?.status !== 'Idle' ||
        fixture?.activeCaptures !== 0 || fixture?.activeProviders !== 0 ||
        fixture?.maxActiveCaptures !== 1 || fixture?.maxActiveProviders !== 1 ||
        fixture?.captureStarts !== 51 || fixture?.captureStops !== 51 || fixture?.providerStarts !== 1 ||
        fixture?.providerStops !== 1 || fixture?.providerResumes !== 0 || fixture?.finals !== 1 ||
        fixture?.providerFailures !== 0 || fixture?.providerNoAudioStops !== 0 ||
        fixture?.warmTerminalCount !== 0 ||
        fixture?.markerViolations?.length !== 0 ||
        report.final?.preparedCaptureTokenCount !== 0) throw new Error('Incomplete native continuation qualification');
    return report;
  }
  if (report?.mode === 'terminal-cleanup') {
    if (envelope.marker !== marker || envelope.passed !== true || report.passed !== true ||
        report.sleepReleased !== true || report.wakeDidNotRestart !== true || report.explicitRestart !== true ||
        report.holdSleepReleased !== true || report.retiredHoldReleaseIgnored !== true || report.holdRestartReleased !== true ||
        report.deviceErrorObserved !== true || report.deviceReleased !== true ||
        fixture?.captureStarts !== 5 || fixture?.captureStops !== 5 ||
        fixture?.activeCaptures !== 0 || fixture?.activeProviders !== 0 || report.skipped) {
      throw new Error('Native terminal cleanup evidence is incomplete');
    }
    return report;
  }
  if (report?.mode === 'live-elevenlabs') {
    if (envelope.marker !== marker || envelope.passed !== true || report.passed !== true ||
        report.pcmBytes !== 788288 || !report.targetDocument || !report.finalText ||
        report.actualPasteVerified !== false || fixture?.captureStarts !== 1 ||
        fixture?.captureStops !== 1 || fixture?.activeCaptures !== 0) {
      throw new Error('Live native pipeline failed; actual OS content still requires external verification');
    }
    return report;
  }
  if (envelope?.marker !== marker || envelope.passed !== true || !report || !fixture ||
      !Number.isFinite(report.elapsedMs) || report.elapsedMs < 180_000 || report.elapsedMs > 900_000 ||
      report.elapsedMs < report.hiddenIdleMs ||
      !Number.isSafeInteger(fixture.captureStarts) || fixture.captureStarts <= 0 ||
      fixture.activeCaptures !== 0 || fixture.activeProviders !== 0 || fixture.captureStarts !== fixture.captureStops ||
      fixture.maxActiveCaptures !== 1 || fixture.maxActiveProviders !== 1 ||
      fixture.observationOverflow !== false || !Array.isArray(fixture.markerViolations) ||
      fixture.markerViolations.length !== 0 || !validateTerminalFixture(fixture, false) ||
      !Array.isArray(report.scenarios) || new Set(report.scenarios).size !== report.scenarios.length ||
      report.scenarios.some((name) => typeof name !== 'string' || !name) || report.scenarios.length < 12 ||
      report.passed !== true || report.completedCycles !== 50 ||
      !Number.isSafeInteger(fixture.finals) || fixture.finals < 50 ||
      report.final?.fixture?.activeCaptures !== 0 || report.final?.fixture?.activeProviders !== 0 ||
      !isDeepStrictEqual(fixture, report.final?.fixture) ||
      report.final?.preparedCaptureTokenCount !== 0 ||
      !Number.isFinite(report.hiddenIdleMs) || report.hiddenIdleMs < 180_000 || report.skipped) {
    throw new Error(`Native result is incomplete: ${JSON.stringify(envelope)}`);
  }
  const cycles = report.cycleEvidence;
  const cycleFinalDeliveries = report.cycleFinalDeliveries;
  const allFinalDeliveries = report.allFinalDeliveries;
  const expectedFinalSessionIds = report.expectedFinalSessionIds;
  const independentlyExpectedFinalSessionIds = Array.isArray(cycles)
    ? [...cycles.map(row => row?.sessionId), report.hiddenIdleEvidence?.wakeSessionId] : [];
  const expectedTranscriptForSession = sessionId => {
    const providerSessionIds = [...new Set((fixture.providerMarkers ?? [])
      .filter(markerRow => markerRow.captureRunId === sessionId)
      .map(markerRow => markerRow.providerSessionId))];
    return providerSessionIds.length === 1 && positiveSafeInteger(providerSessionIds[0])
      ? `Native fixture session ${providerSessionIds[0]}` : null;
  };
  const requiredScenarios = ['50-audio-transcript-stop-hide-reopen-cycles',
    'real-hidden-idle-180s-and-fresh-audio'];
  if (!Array.isArray(cycles) || cycles.length !== 50 ||
    !Array.isArray(cycleFinalDeliveries) || cycleFinalDeliveries.length !== cycles.length ||
    !Array.isArray(allFinalDeliveries) || !Array.isArray(expectedFinalSessionIds) ||
    allFinalDeliveries.length !== expectedFinalSessionIds.length ||
    allFinalDeliveries.length !== fixture.finals ||
    new Set(expectedFinalSessionIds).size !== expectedFinalSessionIds.length ||
    !isDeepStrictEqual(expectedFinalSessionIds, independentlyExpectedFinalSessionIds) ||
    expectedFinalSessionIds.some((sessionId, index) =>
      allFinalDeliveries[index]?.sessionId !== sessionId) ||
    expectedFinalSessionIds.some(sessionId => !positiveSafeInteger(sessionId) ||
      allFinalDeliveries.filter(delivery => delivery.sessionId === sessionId).length !== 1) ||
    allFinalDeliveries.some(delivery => !expectedFinalSessionIds.includes(delivery.sessionId) ||
      typeof delivery.text !== 'string' || !delivery.text.trim() ||
      delivery.text !== expectedTranscriptForSession(delivery.sessionId) ||
      (delivery.deliverySeq !== null && !positiveSafeInteger(delivery.deliverySeq))) ||
    cycleFinalDeliveries.some((delivery, index) =>
      delivery.sessionId !== cycles[index]?.sessionId ||
      delivery.text !== cycles[index]?.expectedTranscript ||
      delivery.deliverySeq !== cycles[index]?.finalDeliverySeq ||
      !allFinalDeliveries.some(candidate => candidate.sessionId === delivery.sessionId &&
        candidate.text === delivery.text && candidate.deliverySeq === delivery.deliverySeq)) ||
    cycles.some((row, index) =>
    row?.index !== index || !Number.isSafeInteger(row.captureStartsBefore) ||
    !Number.isSafeInteger(row.captureStartsAfter) || row.captureStartsAfter <= row.captureStartsBefore ||
    !Number.isSafeInteger(row.captureStopsBefore) ||
    row.captureStopsAfter - row.captureStopsBefore !== row.captureStartsAfter - row.captureStartsBefore ||
    !Number.isSafeInteger(row.sessionId) || row.sessionId <= 0 ||
    !Number.isSafeInteger(row.windowEpoch) || row.windowEpoch <= 0 ||
    !Number.isSafeInteger(row.captureGeneration) || row.captureGeneration <= 0 ||
    !Array.isArray(row.captureGenerations) ||
    row.captureGenerations.length !== row.captureStartsAfter - row.captureStartsBefore ||
    row.captureGenerations.at(-1) !== row.captureGeneration ||
    row.captureGenerations.some((generation, generationIndex) =>
      !positiveSafeInteger(generation) ||
      (generationIndex > 0 && generation <= row.captureGenerations[generationIndex - 1])) ||
    typeof row.expectedTranscript !== 'string' || !row.expectedTranscript.trim() ||
    row.expectedTranscript !== expectedTranscriptForSession(row.sessionId) ||
    row.finalSessionId !== row.sessionId || row.finalText !== row.expectedTranscript ||
    (row.finalDeliverySeq !== null && (!Number.isSafeInteger(row.finalDeliverySeq) || row.finalDeliverySeq <= 0)) ||
    (index > 0 && (row.captureStartsBefore !== cycles[index - 1].captureStartsAfter ||
      row.captureStopsBefore !== cycles[index - 1].captureStopsAfter ||
      row.sessionId <= cycles[index - 1].sessionId || row.windowEpoch <= cycles[index - 1].windowEpoch ||
      row.captureGenerations[0] <= cycles[index - 1].captureGeneration ||
      row.finalSessionId <= cycles[index - 1].finalSessionId))) ||
    cycles.some(row => {
      const deliveries = cycleFinalDeliveries.filter(delivery => delivery.sessionId === row.sessionId);
      return deliveries.length !== 1 || deliveries[0].text !== row.expectedTranscript ||
        deliveries[0].deliverySeq !== row.finalDeliverySeq;
    }) ||
    cycles[49].captureStartsAfter > fixture.captureStarts || cycles[49].captureStopsAfter > fixture.captureStops ||
    cycles.some(row => {
      const capture = fixture.capturePcmLedgers.find(ledger => ledger.captureGeneration === row.captureGeneration);
      const provider = fixture.providerPcmLedgers.find(ledger => ledger.captureGeneration === row.captureGeneration);
      const ownershipValid = row.captureGenerations.every(generation =>
        fixture.captureRunAssociations.some(association =>
          association.captureGeneration === generation && association.captureRunId === row.sessionId));
      return !ownershipValid || !capture || capture.samples <= 0 || !provider || provider.chunks <= 0 ||
        provider.samples !== capture.samples || provider.hash !== capture.hash;
    }) || requiredScenarios.some(name => !report.scenarios.includes(name))) {
    throw new Error(`Native result cycle evidence is incomplete: ${JSON.stringify(envelope)}`);
  }
  const idle = report.hiddenIdleEvidence;
  if (!idle || idle.nativeHiddenIdleMs !== report.hiddenIdleMs ||
      !Number.isFinite(idle.webviewElapsedMs) || idle.webviewElapsedMs < 180_000 ||
      !Number.isSafeInteger(idle.baselineCaptureStarts) ||
      idle.baselineCaptureStarts !== idle.baselineCaptureStops ||
      idle.baselineActiveCaptures !== 0 || idle.baselineActiveProviders !== 0 ||
      !Number.isSafeInteger(idle.baselineCaptureGeneration) || idle.baselineCaptureGeneration < 0 ||
      !Number.isSafeInteger(idle.wakeCaptureGeneration) ||
      idle.wakeCaptureGeneration <= idle.baselineCaptureGeneration ||
      idle.wakeCaptureGeneration <= cycles[49].captureGeneration ||
      !Number.isSafeInteger(idle.wakeSessionId) || idle.wakeSessionId <= cycles[49].sessionId ||
      allFinalDeliveries.filter(delivery => delivery.sessionId === idle.wakeSessionId).length !== 1 ||
      !allFinalDeliveries.some(delivery => delivery.sessionId === idle.wakeSessionId &&
        delivery.text === idle.wakeTranscript) ||
      !Number.isSafeInteger(idle.wakeWindowEpoch) || idle.wakeWindowEpoch <= cycles[49].windowEpoch ||
      typeof idle.wakeTranscript !== 'string' || !idle.wakeTranscript.trim() ||
      !fixture.captureRunAssociations.some(association =>
        association.captureGeneration === idle.wakeCaptureGeneration &&
        association.captureRunId === idle.wakeSessionId) ||
      !fixture.capturePcmLedgers.some(capture => {
        const provider = fixture.providerPcmLedgers.find(ledger =>
          ledger.captureGeneration === idle.wakeCaptureGeneration);
        return capture.captureGeneration === idle.wakeCaptureGeneration && capture.samples > 0 &&
          provider?.chunks > 0 && provider.samples === capture.samples && provider.hash === capture.hash;
      }) ||
      !Number.isFinite(idle.firstVisibleMs) || idle.firstVisibleMs < 0 ||
      !Number.isSafeInteger(idle.wakeSampleCount) || idle.wakeSampleCount < 2 ||
      !Number.isFinite(idle.lastVisibleElapsedMs) ||
      idle.lastVisibleElapsedMs - idle.firstVisibleMs < 1200 ||
      !Number.isSafeInteger(idle.visibilityTransitionCount) || idle.visibilityTransitionCount < 1) {
    throw new Error(`Native result hidden-idle evidence is incomplete: ${JSON.stringify(envelope)}`);
  }
  return report;
}

export function isolatedTauriConfig(original, suffix = randomUUID().replaceAll('-', '')) {
  if (!/^[a-zA-Z0-9]+$/.test(suffix)) throw new Error('Unsafe native identifier suffix');
  const config = structuredClone(original);
  const updaterKey = original?.plugins?.updater?.pubkey;
  // The installed updater plugin requires a string public key during initialization,
  // even in a debug build which never schedules update checks. Keep its required shape
  // while removing every production endpoint and deep-link scheme from this fixture.
  if (typeof updaterKey !== 'string' || !updaterKey) throw new Error('Native bootstrap requires configured updater public key');
  config.identifier = `com.voicetotext.app.native-e2e.${suffix}`;
  config.productName = 'VoicetextAI Native E2E';
  for (const window of config.app.windows) window.incognito = true;
  config.build.devUrl = null;
  config.build.frontendDist = '../dist';
  config.build.beforeBuildCommand = '';
  config.build.beforeBundleCommand = '';
  config.bundle.active = false;
  config.plugins = { updater: { pubkey: updaterKey, endpoints: [] }, 'deep-link': { desktop: { schemes: [] } } };
  return config;
}

export async function main(args = process.argv.slice(2)) {
  const options = parseArguments(args);
  if (process.platform !== 'darwin') throw new Error('Native macOS E2E requires macOS; unsupported platforms are failures, never passing skips');
  const directory = options.reuseBuild ? await prepareReuseBuild(options, source) : options.artifactDir
    ? await validateArtifactDirectory(options.artifactDir)
    : await realpath(await mkdtemp(path.join(os.tmpdir(), 'voicetext-native-e2e-')));
  let primaryFailure, interruption, completionMessage;
  const onInterrupt = () => { interruption ??= new Error('Runner interrupted'); };
  process.on('SIGINT', onInterrupt); process.on('SIGTERM', onInterrupt);
  try {
  const env = executionEnvironment(directory, options);
  await mkdir(env.HOME, { recursive: true });
  const liveSha = '46b449e09435d1694fd118c2c78725fae3be0af414648b16ff005e3bb76cc472';
  const trial = options.trialId ? liveTrials.find(t => t.id === options.trialId) : null;
  let provenance;
  if (trial) {
    const configFile = await realpath(options.harnessConfig);
    const info = await lstat(options.harnessConfig);
    if (configFile !== options.harnessConfig || !info.isFile() || (typeof process.getuid === 'function' && info.uid !== process.getuid()) || (info.mode & 0o022)) throw new Error('Trusted harness config must be owned, canonical and not writable by others');
    provenance = validateHarnessConfig(JSON.parse(await readFile(configFile, 'utf8')));
    for (const fixture of await readApprovedFixtures(path.resolve(source, '../qualification-fixtures'))) await writeFile(path.join(directory, fixture.name), fixture.pcm, { flag: 'wx' });
    await writeFile(path.join(directory, 'qualification-trial.json'), JSON.stringify(trial), { flag: 'wx' });
    await writeFile(path.join(directory, 'backend-provenance.json'), JSON.stringify(provenance), { flag: 'wx' });
  }
  let liveMode = Boolean(options.liveFixturePath);
  let terminalCleanup = Boolean(options.terminalCleanup);
  let continuationFake = Boolean(options.continuationFake);
  if (options.artifactDir) {
    const cached = JSON.parse(await readFile(path.join(directory, 'native-build.json'), 'utf8'));
    assertUnpaidReplay(cached);
    liveMode = cached.mode === 'live-elevenlabs';
    terminalCleanup = cached.mode === 'terminal-cleanup';
    continuationFake = cached.mode === 'continuation-fake';
    options.continuationCase = cached.continuationCase;
    options.miniUx = cached.mode === 'mini-ux';
    if (options.miniUx) env.VOICETEXT_NATIVE_MINI_UX = 'unpaid-v1';
  }
  if (options.artifactDir && continuationFake) Object.assign(env, executionEnvironment(directory, { continuationFake, continuationCase: options.continuationCase }));
  if (terminalCleanup) env.VOICETEXT_NATIVE_TERMINAL = 'test-elevenlabs-stability-20260906';
  if (liveMode) {
    const pcmPath = path.join(directory, 'synthetic.pcm');
    const pcm = await readFile(options.liveFixturePath ?? pcmPath);
    if (digest(pcm) !== liveSha) throw new Error('Live mode requires the exact approved synthetic PCM');
    if (options.liveFixturePath) await writeFile(pcmPath, pcm, { flag: 'wx' });
    env.VOICETEXT_NATIVE_LIVE = 'test-elevenlabs-stability-20260906';
  }
  console.log(`[native-e2e] artifacts: ${directory}`);
  if (!options.artifactDir && !options.reuseBuild) {
    const sourceInputSha256 = await sourceInputFingerprint(source);
    const originalTauriConfig = await readFile(path.join(source, tauriConfigPath), 'utf8');
    const snapshot = path.join(directory, 'frontend');
    await cp(source, snapshot, { recursive: true, dereference: false,
      filter: async (entry) => !path.relative(source, entry).split(path.sep).some((part) => excluded.test(part)) && !(await lstat(entry)).isSymbolicLink(),
    });
    // Only shared, already-installed dependencies are linked; source and build output stay disposable.
    await symlink(path.join(source, 'node_modules'), path.join(snapshot, 'node_modules'), 'dir');
    const config = isolatedTauriConfig(JSON.parse(await readFile(path.join(snapshot, 'src-tauri', 'tauri.conf.json'), 'utf8')));
    await writeFile(path.join(snapshot, 'src-tauri', 'tauri.conf.json'), JSON.stringify(config, null, 2));
    await runOwned(process.execPath, [path.join(snapshot, 'node_modules/vite/bin/vite.js'), 'build', '--mode', 'native-window-e2e'],
      { cwd: snapshot, env }, 180_000, path.join(directory, 'frontend-build.log'));
    const rustEnv = { ...env,
      RUSTUP_HOME: path.join(os.homedir(), '.rustup'),
      CARGO_HOME: path.join(os.homedir(), '.cargo'),
      CARGO_TARGET_DIR: '/tmp/voicetext-rust-target',
      CARGO_BUILD_JOBS: '2',
    };
    await runOwned('cargo', ['build', '--locked', '--features', 'native-window-e2e,tauri/custom-protocol', '--bin', 'voice-to-text'],
      { cwd: path.join(snapshot, 'src-tauri'), env: rustEnv }, 1_800_000, path.join(directory, 'rust-build.log'));
    const sourceSha256 = await snapshotDigest(snapshot);
    const binary = path.join(directory, 'native-window-e2e');
    await cp(path.join(rustEnv.CARGO_TARGET_DIR, 'debug', 'voice-to-text'), binary);
    const binaryBytes = await readFile(binary);
    // A concurrent build sharing the Cargo cache must never be silently bound to
    // this snapshot. Each isolated config has a unique embedded application ID.
    if (!binaryBytes.includes(Buffer.from(config.identifier))) throw new Error('Native binary application identity mismatch');
    const binding = { sha256: digest(binaryBytes), identifier: config.identifier, sourceSha256, sourceInputSha256 };
    await writeFile(path.join(directory, 'native-build.json'), JSON.stringify({ schema: reuseSchema, marker,
      binary: path.basename(binary), ...binding, buildOrigin: binding, originalTauriConfig,
      trialId: trial?.id ?? null, continuationCase: options.continuationCase ?? null,
      mode: options.miniUx ? 'mini-ux' : options.readerPreparation ? 'reader-preparation' : trial ? 'continuation-live' : continuationFake ? 'continuation-fake' : liveMode ? 'live-elevenlabs' : terminalCleanup ? 'terminal-cleanup' : 'fixture' }));
  }
  await validateReusableBuild(directory, source);
  const binary = await validateCachedBinary(directory);
  if (options.continuationCase === 'E54') {
    await runRestartCrash(binary, directory, env, runOwned);
    completionMessage = `[native-e2e] E54 restart/crash verified ${directory}`;
    return;
  }
  let runtimeFailure;
  const proxyEvents = [];
  const proxyStarted = performance.now();
  const { startConfigDelayProxy } = trial ? await import('./helpers/nativeContinuationProxy.mjs') : {};
  const proxy = trial ? await startConfigDelayProxy(provenance.endpoint, trial.configDelayMs, (event, details) => {
    // The canary includes bounded churn plus one complete 49.3 s source. Keep
    // even the all-sources-complete worst case for ownership and cleanup proof.
    if (proxyEvents.length < maxProxyEvidenceEvents) proxyEvents.push({ event, ...details, atMs: performance.now() - proxyStarted });
    else if (proxyEvents.length === maxProxyEvidenceEvents) proxyEvents.push({ event: 'fault_proxy_evidence_overflow' });
  }) : null;
  if (proxy) env.VOICETEXT_QUALIFICATION_ENDPOINT = proxy.url;
  const collectBeforeTeardown = trial ? createQualificationCollector(trial, proxyEvents,
    async () => JSON.parse(await readFile(env.VOICE_TO_TEXT_NATIVE_E2E_RESULT, 'utf8')),
    () => performance.now() - proxyStarted) : options.miniUx ? async () => {
      try {
        // Read a complete report before retiring the test-owned process. UX and
        // resource assertions are validated below, independently of termination.
        JSON.parse(await readFile(env.VOICE_TO_TEXT_NATIVE_E2E_RESULT, 'utf8'));
        return true;
      } catch (error) {
        if (error.code === 'ENOENT' || error instanceof SyntaxError) return false;
        throw error;
      }
    } : undefined;
  try {
    if (interruption) throw interruption;
    // Event/preparation timeout: 30 seconds, then up to 5 seconds SIGTERM grace before SIGKILL.
    const runtimeTimeoutMs = options.miniUx ? 480_000
      : trial?.kind === 'warm-provider-canary' ? 900_000
      : options.readerPreparation || ['E04', 'E41', 'E42'].includes(options.continuationCase) ? 30_000
      : 900_000;
    await runOwned(binary, [], { cwd: directory, env }, runtimeTimeoutMs,
      path.join(directory, `native-runtime-${randomUUID()}.log`), path.join(directory, 'native-progress.jsonl'),
      collectBeforeTeardown, path.join(directory, 'native-process-termination.json'));
  } catch (error) { runtimeFailure = error; }
  finally {
    if (proxy) {
      try { await proxy.close(); }
      catch (error) { runtimeFailure = runtimeFailure ? new AggregateError([runtimeFailure, error], `${runtimeFailure.message}; proxy cleanup: ${error.message}`, { cause: runtimeFailure }) : error; }
      try { await writeFile(path.join(directory, 'proxy-evidence.json'), JSON.stringify({ clock: 'runner-performance-now', events: proxyEvents }, null, 2)); }
      catch (error) { runtimeFailure = runtimeFailure ? new AggregateError([runtimeFailure, error], `${runtimeFailure.message}; proxy evidence: ${error.message}`, { cause: runtimeFailure }) : error; }
    }
  }
  if (options.readerPreparation) {
    await verifyPreparationArtifact(directory, env.VOICE_TO_TEXT_NATIVE_E2E_RESULT, runtimeFailure);
    completionMessage = `[native-e2e] PREPARATION READY (unpaid; inconclusive nonreproduction; qualificationPassed=false) ${env.VOICE_TO_TEXT_NATIVE_E2E_RESULT}`;
    return;
  }
  if (runtimeFailure && options.continuationCase?.startsWith('after-write-')) {
    await writeFile(path.join(directory, 'runner-diagnostic-failure.json'), JSON.stringify({
      passed: false, qualificationPassed: false, reason: 'owned-runtime-failed', overallBudgetMs: 30000,
    }), { flag: 'wx' });
  }
  if (options.continuationCase?.startsWith('after-write-')) {
    // A late full report cannot override an independent native failure.
    try {
      const failure = await readFile(path.join(directory, 'native-diagnostic-failure.json'), 'utf8');
      throw new Error(`E63 native diagnostic failed: ${failure}`);
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  let envelope;
  try { envelope = JSON.parse(await readFile(env.VOICE_TO_TEXT_NATIVE_E2E_RESULT, 'utf8')); }
  catch (error) { throw runtimeFailure || error; }
  if (runtimeFailure) throw runtimeFailure;
  if (trial) {
    const verification = { passed: false, qualificationPassed: false, trialId: trial.id, actualPasteVerified: false };
    try {
      const report = envelope.report;
      const reportTrialId = trial.kind === 'warm-provider-canary' ? report?.trialId : report?.trial?.id;
      if (envelope.marker !== marker || envelope.passed !== true || report?.passed !== true || report.errors?.length || reportTrialId !== trial.id) throw new Error('Native live qualification failed');
      const target = path.join(directory, 'p4-textedit-a.txt');
      const script = `tell application "TextEdit"\nset matches to ${ownedDocumentMatches(target)}\nif (count matches) is not 1 then error "TEST document identity missing or ambiguous"\nreturn text of item 1 of matches\nend tell`;
      const readbackStartMs = performance.now() - proxyStarted;
      const { stdout } = await promisify(execFile)('/usr/bin/osascript', ['-e', script], { timeout: 5000, maxBuffer: 1024 * 1024 });
      verification.readback = { clock: 'runner-performance-now', startMs: readbackStartMs, endMs: performance.now() - proxyStarted };
      Object.assign(verification, verifyQualificationConnections(trial, proxyEvents));
      Object.assign(verification, verifyQualificationRoute(trial, proxyEvents));
      if (trial.kind === 'warm-provider-canary') {
        if (stdout.replace(/\n$/, '') !== '') {
          throw new Error('Warm provider canary changed immutable delivery policy or pasted unexpectedly');
        }
        verification.noUnexpectedPaste = true;
        Object.assign(verification,
          verifyWarmProviderFinalFixtureAgreement(report.final.fixture, envelope.fixture));
        Object.assign(verification, verifyWarmProviderCanary(trial, report));
        Object.assign(verification, verifyWarmProviderTransport(proxyEvents, report.final.fixture, report));
        if (verification.clientAudioBytes !== verification.transmittedPcmBytes) {
          throw new Error('Warm provider route and transport byte evidence disagree');
        }
        const accepted = proxyEvents.filter(e => e.event === 'backend_control' &&
          e.type === 'continue_result' && e.decision === 'accepted' && e.eligible_now === true);
        const expectedContinues = expectedWarmProviderContinues(trial);
        if (accepted.length !== expectedContinues) {
          throw new Error('Warm provider canary did not retain every expected continuation');
        }
      } else {
        Object.assign(verification, exactInsertionEvidence(report.expectedInsertion, stdout.replace(/\n$/, ''), report.targetDocument));
        verifyQualificationSources(trial, report.final?.fixture);
        verifyQualificationTerminals(trial, report.episodes, report.terminals);
        const accepted = proxyEvents.filter(e => e.event === 'backend_control' && e.type === 'continue_result' && e.decision === 'accepted' && e.eligible_now === true);
        if (trial.continuation && accepted.length !== 1) throw new Error('Exactly one eligible Continue acceptance required');
      }
      verification.continueOutcomes = proxyEvents.filter(e => e.event === 'backend_control');
      // Upstream provider connect count must come from backend/native instrumentation, not socket inference.
      verification.limitations = ['Provider handshake/Continue eligibility and native insertion timing require parent instrumentation; this is pipeline evidence only.'];
      verification.passed = true;
    } catch (error) { verification.error = String(error); throw error; }
    finally { await writeFile(path.join(directory, 'qualification-verification.json'), JSON.stringify(verification, null, 2)); }
  } else {
    validateResult(envelope);
    if (liveMode) {
      const verification = { passed: false, qualificationPassed: false, actualPasteVerified: false };
      let verificationFailure;
      try {
        const report = envelope.report;
        const target = path.join(directory, 'p4-textedit-a.txt');
        if (report.mode !== 'live-elevenlabs' || report.targetDocument !== path.basename(target)) throw new Error('Legacy live TEST document identity mismatch');
        const script = `tell application "TextEdit"\nset matches to ${ownedDocumentMatches(target)}\nif (count matches) is not 1 then error "TEST document identity missing or ambiguous"\nreturn text of item 1 of matches\nend tell`;
        const { stdout } = await promisify(execFile)('/usr/bin/osascript', ['-e', script], { timeout: 5000, maxBuffer: 1024 * 1024 });
        Object.assign(verification, exactInsertionEvidence(report.finalText, stdout.replace(/\n$/, ''), report.targetDocument));
        verification.passed = true;
      } catch (error) { verificationFailure = error; verification.error = String(error); throw error; }
      finally {
        try { await writeFile(path.join(directory, 'live-verification.json'), JSON.stringify(verification, null, 2)); }
        catch (error) {
          if (verificationFailure) throw new AggregateError([verificationFailure, error], `${verificationFailure.message}; verification evidence: ${error.message}`, { cause: verificationFailure });
          throw error;
        }
      }
    }
  }
  completionMessage = `[native-e2e] ${trial ? 'PIPELINE PASS, provider/latency qualification pending' : liveMode ? 'PIPELINE PASS, exact OS insertion verified; provider/latency qualification pending' : 'PASS'} ${env.VOICE_TO_TEXT_NATIVE_E2E_RESULT}`;
  } catch (error) { primaryFailure = error; throw error; }
  finally {
    try { await closeOwnedDocument(directory); }
    catch (cleanupError) {
      if (primaryFailure) throw new AggregateError([primaryFailure, cleanupError], `${primaryFailure.message}; cleanup failed: ${cleanupError.message}`, { cause: primaryFailure });
      throw cleanupError;
    } finally { process.removeListener('SIGINT', onInterrupt); process.removeListener('SIGTERM', onInterrupt); }
    if (!primaryFailure && interruption) throw interruption;
    if (!primaryFailure && completionMessage) console.log(completionMessage);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(`[native-e2e] FAIL ${error.stack || error}`); process.exitCode = 1; });
}
