import { createHash } from 'node:crypto';
import { readFile, realpath, lstat } from 'node:fs/promises';
import path from 'node:path';

export const approvedFixtures = Object.freeze({
  'episode-a.pcm': [42288, '333e191dbcf011d37d3c42968fc802cc7d4e7c1dfcd4a4ef9d7e0ca12e096ccc'],
  'episode-b.pcm': [43868, '4fc298adbedbf74ff5673500ea64c77af7fed0334e0710d9a165ff49fd2e0cb6'],
  'long-auto-commit.pcm': [1576576, 'd3a6c380e79b3d3efeda3ee7a37055987bb170984a53306cbb79f27f038c0c3d'],
  'stop-inside-word.pcm': [35200, '72603893abf7efe13a68100201b9d6c25a8e2d6002f635c41f0fd67dea30cf82'],
  'old-commit-new-tail.pcm': [214156, 'a2d00cd1f10fac3f19f5dd566779b869cb12f7325965ea3b8b76e1f16e1c2933'],
});
// Bounded above the worst case where every churn source reaches its final frame,
// followed by the complete 49.3s proof at 20ms cadence.
export const maxProxyEvidenceEvents = 32768;
export const warmProviderCanaryJittersMs = Object.freeze([0, 25, 100, 250, 500]);
export const warmProviderCanaryPhases = Object.freeze([
  'before-ready',
  'after-first-pcm',
  'during-partial',
  'after-final',
]);
const warmProviderCanaryEpisodeByPhase = Object.freeze({
  'before-ready': 'episode-a.pcm',
  'after-first-pcm': 'episode-b.pcm',
  'during-partial': 'old-commit-new-tail.pcm',
  'after-final': 'long-auto-commit.pcm',
});
export const warmProviderCanaryCycles = Object.freeze(Array.from({ length: 20 }, (_, index) => {
  const stopPhase = warmProviderCanaryPhases[Math.floor(index / warmProviderCanaryJittersMs.length)];
  return Object.freeze({
    index,
    jitterMs: warmProviderCanaryJittersMs[index % warmProviderCanaryJittersMs.length],
    stopPhase,
    episode: warmProviderCanaryEpisodeByPhase[stopPhase],
  });
}));
export const warmProviderCanaryTrial = Object.freeze({
  id: 'warm-provider-churn-20',
  kind: 'warm-provider-canary',
  continuation: true,
  configDelayMs: 4000,
  route: 'warm-provider-churn',
  cycles: warmProviderCanaryCycles,
  readyGateFromIndex: warmProviderCanaryJittersMs.length,
  finalEpisodeIndex: warmProviderCanaryCycles.length,
  episodes: Object.freeze([
    ...warmProviderCanaryCycles.map(cycle => cycle.episode),
    'long-auto-commit.pcm',
  ]),
});
export function validatePcm(name, bytes) {
  const pin = Object.hasOwn(approvedFixtures, name) && approvedFixtures[name];
  if (!pin || bytes.length !== pin[0] || createHash('sha256').update(bytes).digest('hex') !== pin[1]) {
    throw new Error('Unapproved qualification PCM identity');
  }
  return { name, bytes: pin[0], sha256: pin[1], sourceFrames: pin[0] / 2, sourceDurationMs: pin[0] / 32 };
}
export async function readApprovedFixtures(directory) {
  const root = await realpath(directory);
  const manifest = JSON.parse(await readFile(path.join(root, 'manifest.json'), 'utf8'));
  if (manifest.format !== 'PCM16 mono 16000 Hz') throw new Error('Wrong source format');
  const result = [];
  for (const name of Object.keys(approvedFixtures)) {
    const file = path.join(root, name);
    if (!(await lstat(file)).isFile() || await realpath(file) !== file) throw new Error('Fixture must be a regular owned-path file');
    const bytes = await readFile(file);
    const identity = validatePcm(name, bytes);
    if (manifest.fixtures?.[name]?.sha256 !== identity.sha256 || manifest.fixtures?.[name]?.bytes !== identity.bytes) {
      throw new Error('Manifest disagrees with pinned source identity');
    }
    result.push({ ...identity, pcm: bytes });
  }
  return result;
}
// Fixed paid budget: each entry is run at most once, failure evidence is retained.
export const liveTrials = Object.freeze([
  ...[1, 2, 3].map(attempt => ({ id: `warm-baseline-${attempt}`, continuation: false, configDelayMs: 0, route: 'baseline', episodes: ['episode-a.pcm', 'episode-b.pcm'] })),
  ...[1, 2, 3].map(attempt => ({ id: `warm-continue-${attempt}`, continuation: true, configDelayMs: 0, route: 'continued-audio', episodes: ['episode-a.pcm', 'episode-b.pcm'] })),
  ...[0, 4000, 8000].map(configDelayMs => ({ id: `cold-${configDelayMs}`, continuation: false, configDelayMs, route: 'cold-two-runs', episodes: ['long-auto-commit.pcm', 'episode-b.pcm'] })),
  { id: 'long', continuation: true, configDelayMs: 0, route: 'continued-audio', episodes: ['episode-a.pcm', 'long-auto-commit.pcm'] },
  { id: 'short-tail', continuation: true, configDelayMs: 0, route: 'cancel-unsent', episodes: ['episode-a.pcm', 'stop-inside-word.pcm'] },
  { id: 'old-tail', continuation: true, configDelayMs: 0, route: 'continued-audio', episodes: ['old-commit-new-tail.pcm', 'episode-b.pcm'] },
  warmProviderCanaryTrial,
]);
export function validateHarnessConfig(config) {
  if (config?.schema !== 'p4-test-backend-v1' || config.testOnly !== true ||
      !/^[a-f0-9]{64}$/.test(config.backendSourceSha256 ?? '') ||
      !/^[a-zA-Z0-9-]{8,80}$/.test(config.disposableInstanceId ?? '') ||
      !Number.isSafeInteger(config.backendPid) || config.backendPid <= 0 ||
      !Number.isSafeInteger(config.startedAtUnixMs) || config.startedAtUnixMs <= 0 ||
      Object.keys(config).some(k => !['schema','testOnly','backendSourceSha256','disposableInstanceId','backendPid','startedAtUnixMs','endpoint'].includes(k))) {
    throw new Error('Explicit disposable TEST backend provenance required; no credentials accepted');
  }
  const url = new URL(config.endpoint);
  if (url.protocol !== 'ws:' || url.hostname !== '127.0.0.1' || !url.port || url.port === '51866' ||
      url.username || url.password || url.search || url.hash || url.pathname !== '/') {
    throw new Error('TEST endpoint must be explicit fresh loopback port, without credentials/path');
  }
  return structuredClone(config);
}
export function exactInsertionEvidence(expected, actual, identity) {
  if (!identity || !/^p4-textedit-[a-f0-9-]+\.txt$/.test(identity) || typeof expected !== 'string' || !expected || actual !== expected) {
    throw new Error('Exact new TEST document insertion does not match stable delivery');
  }
  return { actualPasteVerified: true, documentIdentity: identity,
    utf8Bytes: Buffer.byteLength(actual), sha256: createHash('sha256').update(actual).digest('hex') };
}

export function qualificationExpectations(trial) {
  if (!liveTrials.some(row => JSON.stringify(row) === JSON.stringify(trial))) throw new Error('Unplanned qualification trial');
  if (trial.kind === 'warm-provider-canary') {
    const earlyStops = trial.cycles.filter(cycle => cycle.stopPhase === 'before-ready').length;
    return { warmProviderCanary: true, captures: trial.cycles.length + 1,
      minBackendConnections: 1, maxBackendConnections: earlyStops + 2,
      providerHandshakes: { min: 1, max: earlyStops + 2 }, maxActiveUpstream: 1,
      gateInitialSource: false, finalGateIndex: trial.finalEpisodeIndex, gapMs: null };
  }
  const baseline = trial.id.startsWith('warm-baseline-');
  return { baseline, captures: baseline ? 1 : 2, backendConnections: baseline || trial.continuation ? 1 : 2,
    providerHandshakes: baseline || trial.continuation ? 1 : 2, maxActiveUpstream: 1,
    gateInitialSource: baseline || trial.continuation, gapMs: 120 };
}
export function verifyQualificationConnections(trial, events) {
  const expected = qualificationExpectations(trial);
  let active = 0; let connections = 0; let boundarySeen = false;
  for (const event of events) {
    if (event.event === 'backend_control' && event.type === 'error') throw new Error('Retained backend error fails qualification');
    if (!trial.continuation && event.event === 'backend_control' &&
        ['pause_accepted', 'pause_rejected', 'continue_result', 'pause_restore_result'].includes(event.type)) throw new Error('Unexpected continuation control in normal recording');
    if (['fault_proxy_overflow', 'fault_proxy_transport_error', 'fault_proxy_deadline', 'fault_proxy_evidence_overflow'].includes(event.event)) throw new Error('Fault proxy gate failed');
    if (event.event === 'qualification_pre_teardown') {
      const connectionCountValid = expected.warmProviderCanary
        ? connections >= expected.minBackendConnections && connections <= expected.maxBackendConnections
        : connections === expected.backendConnections;
      if (boundarySeen || active !== 0 || !connectionCountValid || event.nativeProcessAlive !== true || event.clock !== 'runner-performance-now' || !Number.isFinite(event.atMs)) throw new Error('Normal Stop not closed before teardown');
      boundarySeen = true;
    }
    // Only upstream closes establish release; a later proxy-client close is not credited.
    if (boundarySeen && (event.event === 'fault_proxy_connected' || (event.event === 'fault_proxy_close' && event.direction === 'upstream'))) throw new Error('Connection activity after teardown boundary');
    if (event.event === 'fault_proxy_connected') {
      connections++; active++;
      if (active > 1) throw new Error('Overlapping backend connections');
    }
    if (event.event === 'fault_proxy_close' && event.direction === 'upstream') {
      if (event.code !== 1000 && event.code !== 1005) throw new Error('Unclean upstream close fails qualification');
      if (--active < 0) throw new Error('Unmatched upstream close');
    }
  }
  const connectionCountValid = expected.warmProviderCanary
    ? connections >= expected.minBackendConnections && connections <= expected.maxBackendConnections
    : connections === expected.backendConnections;
  if (!boundarySeen || !connectionCountValid || active !== 0) throw new Error('Mode-specific backend connection count/cleanup failed');
  return { backendConnections: connections, expectedProviderHandshakes: expected.providerHandshakes,
    ...(expected.warmProviderCanary ? {
      allowedBackendConnectionRange: [expected.minBackendConnections, expected.maxBackendConnections],
    } : {}),
    requiredMaxActiveUpstream: 1, normalStopConnectionReleaseVerified: true,
    preTeardownBoundary: events.find(event => event.event === 'qualification_pre_teardown'),
    providerHandshakeVerification: 'pending-parent-logs' };
}

export function verifyQualificationRoute(trial, events) {
  const expected = qualificationExpectations(trial);
  const connections = events.filter(event => event.event === 'fault_proxy_connected');
  const binary = events.filter(event => event.event === 'client_binary');
  if (binary.some(event => !Number.isSafeInteger(event.connectionId) || event.connectionId <= 0 ||
      !Number.isSafeInteger(event.bytes) || event.bytes <= 0 || event.bytes > 9_600 || event.bytes % 2 !== 0)) {
    throw new Error('Invalid client audio frame evidence');
  }
  const connectionIds = new Set(connections.map(event => event.connectionId));
  const connectionCountValid = expected.warmProviderCanary
    ? connectionIds.size >= expected.minBackendConnections && connectionIds.size <= expected.maxBackendConnections
    : connectionIds.size === expected.backendConnections;
  if (!connectionCountValid || connections.some(event =>
      !Number.isSafeInteger(event.connectionId) || event.connectionId <= 0) ||
      binary.some(event => !connectionIds.has(event.connectionId))) {
    throw new Error('Audio evidence does not belong to the expected connections');
  }
  for (const connectionId of connectionIds) {
    if (!expected.warmProviderCanary && !binary.some(event => event.connectionId === connectionId)) throw new Error('Expected connection has no client audio');
  }
  if (expected.warmProviderCanary) {
    const lastConnectionId = connections.at(-1)?.connectionId;
    if (!lastConnectionId || !binary.some(event => event.connectionId === lastConnectionId)) {
      throw new Error('Final warm canary connection has no client audio');
    }
    const ready = events.filter(event => event.event === 'backend_control' && event.type === 'ready');
    const rejected = events.filter(event => event.event === 'backend_control' &&
      (event.type === 'pause_rejected' || (event.type === 'continue_result' && event.decision !== 'accepted')));
    const pauses = events.filter(event => event.event === 'backend_control' &&
      event.type === 'pause_accepted' && event.decision === 'accepted');
    const continues = events.filter(event => event.event === 'backend_control' &&
      event.type === 'continue_result' && event.decision === 'accepted' && event.eligible_now === true);
    const retainedCycles = trial.cycles.length - trial.readyGateFromIndex;
    const providerSessionId = ready[0]?.session_id;
    const retainedConnectionId = ready[0]?.connectionId;
    if (ready.length !== 1 || typeof providerSessionId !== 'string' || !providerSessionId ||
        !connectionIds.has(retainedConnectionId) || retainedConnectionId !== lastConnectionId ||
        rejected.length !== 0 || pauses.length !== retainedCycles + 1 || continues.length !== retainedCycles ||
        [...pauses, ...continues].some(event => event.connectionId !== retainedConnectionId ||
          event.provider_session_id !== providerSessionId)) {
      throw new Error('Warm canary did not retain exactly one provider session across churn');
    }
    const readyIndex = events.indexOf(ready[0]);
    let active = true;
    let orderedPauses = 0;
    let orderedContinues = 0;
    for (const event of events.slice(readyIndex + 1)) {
      if (event.event === 'client_binary' && event.connectionId === retainedConnectionId) {
        if (!active) throw new Error('Warm canary sent audio while provider session was paused');
        continue;
      }
      if (event.event !== 'backend_control' || event.connectionId !== retainedConnectionId ||
          event.provider_session_id !== providerSessionId) continue;
      if (event.type === 'pause_accepted' && event.decision === 'accepted') {
        if (!active) throw new Error('Warm canary Pause acceptance is out of order');
        active = false;
        orderedPauses += 1;
      } else if (event.type === 'continue_result' && event.decision === 'accepted' && event.eligible_now === true) {
        if (active) throw new Error('Warm canary Continue acceptance is out of order');
        active = true;
        orderedContinues += 1;
      }
    }
    if (active || orderedPauses !== retainedCycles + 1 || orderedContinues !== retainedCycles) {
      throw new Error('Warm canary retained session did not end in an ordered paused state');
    }
    return { clientAudioFrames: binary.length, clientAudioConnections: connectionIds.size,
      routeVerified: trial.route, maximumActiveConnections: 1, maximumActiveProviderSessions: 1,
      retainedProviderSessionId: providerSessionId, acceptedPauses: pauses.length,
      acceptedContinues: continues.length };
  }
  if (!trial.continuation) return { clientAudioFrames: binary.length, clientAudioConnections: connectionIds.size, routeVerified: trial.route };

  const pause = events.findIndex(event => event.event === 'backend_control' && event.type === 'pause_accepted' && event.decision === 'accepted');
  const continued = events.findIndex(event => event.event === 'backend_control' && event.type === 'continue_result' &&
    event.decision === 'accepted' && event.eligible_now === true);
  if (pause < 0 || continued <= pause) throw new Error('Continue route lacks ordered Pause/Continue acceptance');
  const preContinueAudio = events.slice(pause + 1, continued).filter(event => event.event === 'client_binary');
  if (preContinueAudio.length !== 0) throw new Error('Client audio preceded Continue acceptance');
  const postContinueAudio = events.slice(continued + 1).filter(event => event.event === 'client_binary');
  const restores = events.slice(continued + 1).filter(event => event.event === 'backend_control' &&
    event.type === 'pause_restore_result' && event.decision === 'accepted');
  if (trial.route === 'cancel-unsent') {
    if (restores.length !== 1 || postContinueAudio.length !== 0) throw new Error('Cancelled unsent route must Restore without a B write');
  } else if (trial.route === 'continued-audio') {
    if (restores.length !== 0 || postContinueAudio.length === 0) throw new Error('Continued route requires a B write after acceptance and no Restore');
  } else throw new Error('Unknown continuation route contract');
  return { clientAudioFrames: binary.length, clientAudioConnections: connectionIds.size,
    postContinueAudioFrames: postContinueAudio.length, routeVerified: trial.route };
}

export function verifyQualificationTerminals(trial, episodes, terminals) {
  const expected = qualificationExpectations(trial);
  const owners = episodes?.map(row => row.logicalRunId) ?? [];
  const distinct = new Set(owners);
  if (owners.length !== 2 || owners.some(id => !Number.isSafeInteger(id) || id <= 0) ||
      distinct.size !== expected.backendConnections || !Array.isArray(terminals) || terminals.length !== distinct.size ||
      terminals.some(t => t.complete !== true || !distinct.has(t.sessionId)) ||
      [...distinct].some(id => terminals.filter(t => t.sessionId === id).length !== 1))
    throw new Error('Exactly one complete terminal per expected distinct logical run required');
}

export function verifyQualificationSources(trial, fixture) {
  const expected = qualificationExpectations(trial);
  if (fixture?.observationOverflow || fixture?.captureStarts !== expected.captures || fixture?.captureStops !== expected.captures ||
      fixture?.activeCaptures !== 0 || fixture?.sourceEpisodes?.length !== 2) throw new Error('Mode-specific capture/source counts failed');
  for (const [index, row] of fixture.sourceEpisodes.entries()) {
    const [bytes] = approvedFixtures[trial.episodes[index]];
    const gate = index === 0 && expected.gateInitialSource;
    if (row.name !== trial.episodes[index] || row.bytes !== bytes || row.sourceFrames !== bytes / 2 || row.emittedFrames !== bytes / 2 ||
        row.captureGeneration !== (expected.baseline ? 1 : index + 1) || row.sourceGateRequired !== gate || row.cadenceMs !== 20 ||
        !Number.isFinite(row.nativeSourceStartMs) || !Number.isFinite(row.nativeSourceEndMs) ||
        row.nativeSourceEndMs - row.nativeSourceStartMs < bytes / 32 || row.sourceGateError) throw new Error('Invalid native source evidence');
    const chunks = Math.ceil(row.sourceFrames / 320);
    const lastMinimumMs = (chunks - 1) * 20;
    const lastPartialMs = (row.sourceFrames - (chunks - 1) * 320) / 16;
    const intervalCount = expected.baseline && index === 1 ? Math.ceil(fixture.sourceEpisodes[0].sourceFrames / 320) + 6 + chunks - 1 : chunks - 1;
    if (!Number.isFinite(row.nativeLastSourceFrameMs) || !Number.isFinite(row.lastSourceFrameElapsedMs) ||
        row.nativeLastSourceFrameMs - row.nativeSourceStartMs < lastMinimumMs || row.lastSourceFrameElapsedMs < lastMinimumMs ||
        row.nativeLastSourceFrameMs + lastPartialMs > row.nativeSourceEndMs ||
        row.lastSourceFrameElapsedMs > row.nativeSourceEndMs - row.nativeSourceStartMs ||
        row.pacingIntervalsChecked !== intervalCount || row.pacingViolations !== 0) throw new Error('Contradictory native paced source evidence');
    if (gate && (row.sourceGateReady?.serverReady !== true || row.sourceGateReady?.status !== 'Recording' || row.sourceGateReady?.emittedFrames !== 0 ||
        !Number.isFinite(row.sourceGateReady?.nativeReadyMs) || row.sourceGateReady.nativeReadyMs > row.nativeSourceStartMs)) throw new Error('Missing initial provider Ready gate');
    if (!gate && row.sourceGateReady != null) throw new Error('Cold/B source must not be gated');
    if (expected.baseline && (!row.continuousCapture || (index === 1 && (row.gapBeforeMs !== 120 || row.gapFrames !== 1920 ||
        !Number.isFinite(row.nativeGapStartMs) || row.nativeGapStartMs < fixture.sourceEpisodes[0].nativeSourceEndMs ||
        row.nativeSourceStartMs - row.nativeGapStartMs < 120)))) throw new Error('Missing continuous baseline gap');
  }
  return expected;
}

export function verifyWarmProviderCanary(trial, report) {
  const expected = qualificationExpectations(trial);
  const cycles = report?.cycles;
  const events = report?.events;
  const fixture = report?.final?.fixture;
  if (!expected.warmProviderCanary || report?.mode !== 'warm-provider-canary' ||
      report?.passed !== true || report.trialId !== trial.id ||
      !Array.isArray(report.errors) || report.errors.length ||
      !Array.isArray(report.duplicateDeliveries) || report.duplicateDeliveries.length ||
      !Array.isArray(cycles) || cycles.length !== trial.cycles.length ||
      !Array.isArray(events) || events.length > 2048 || !fixture) {
    throw new Error('Incomplete warm provider canary report');
  }
  const associations = new Map((fixture.captureRunAssociations ?? []).map(row =>
    [row.captureGeneration, row]));
  const captureLedgers = fixture.capturePcmLedgers ?? [];
  const providerLedgers = fixture.providerPcmLedgers ?? [];
  const ledgerIsValid = row => Number.isSafeInteger(row?.captureGeneration) && row.captureGeneration > 0 &&
    Number.isSafeInteger(row.chunks) && row.chunks >= 0 && Number.isSafeInteger(row.samples) && row.samples >= 0 &&
    typeof row.hash === 'string' && /^[a-f0-9]{16}$/.test(row.hash);
  if (fixture.captureStarts !== expected.captures || fixture.captureStops !== expected.captures ||
      fixture.activeCaptures !== 0 || fixture.maxActiveCaptures !== 1 ||
      fixture.observationOverflow !== false || fixture.markerViolations?.length !== 0 ||
      fixture.sourceEpisodes?.length !== expected.captures ||
      captureLedgers.length !== expected.captures || captureLedgers.some(row => !ledgerIsValid(row)) ||
      new Set(captureLedgers.map(row => row.captureGeneration)).size !== expected.captures ||
      providerLedgers.some(row => !ledgerIsValid(row)) ||
      new Set(providerLedgers.map(row => row.captureGeneration)).size !== providerLedgers.length) {
    throw new Error('Warm provider canary capture lifecycle is incomplete');
  }
  const providerByGeneration = new Map(providerLedgers.map(row => [row.captureGeneration, row]));
  for (const [index, capture] of captureLedgers.entries()) {
    const source = fixture.sourceEpisodes[index];
    const provider = providerByGeneration.get(capture.captureGeneration);
    const expectedChunks = Math.ceil(source.emittedFrames / 320);
    if (capture.captureGeneration !== index + 1 || capture.samples !== source.emittedFrames ||
        capture.chunks !== expectedChunks ||
        (capture.samples === 0 && (capture.hash !== 'cbf29ce484222325' || provider != null)) ||
        (capture.samples > 0 && (!provider || provider.chunks !== capture.chunks ||
          provider.samples !== capture.samples || provider.hash !== capture.hash))) {
      throw new Error(`Warm provider canary PCM generation ${index + 1} is incomplete`);
    }
  }
  if (providerLedgers.length !== captureLedgers.filter(row => row.samples > 0).length) {
    throw new Error('Warm provider canary contains an unexpected provider PCM generation');
  }
  for (const [index, cycle] of cycles.entries()) {
    const plan = trial.cycles[index];
    const source = fixture.sourceEpisodes[index];
    const cycleEvents = events.slice(cycle.eventStart, cycle.eventEnd);
    const gated = index >= trial.readyGateFromIndex;
    if (cycle.index !== index || cycle.stopPhase !== plan.stopPhase || cycle.jitterMs !== plan.jitterMs ||
        cycle.episode !== plan.episode || cycle.captureGeneration !== index + 1 ||
        !Number.isFinite(cycle.startedAtMs) || !Number.isFinite(cycle.triggerAtMs) ||
        !Number.isFinite(cycle.captureStoppedAtMs) || !Number.isFinite(cycle.idleAtMs) ||
        cycle.triggerAtMs < cycle.startedAtMs || cycle.triggerAtMs - cycle.startedAtMs > 75_000 ||
        cycle.captureStoppedAtMs < cycle.triggerAtMs || cycle.captureStoppedAtMs - cycle.triggerAtMs > 5_500 ||
        cycle.idleAtMs < cycle.captureStoppedAtMs || cycle.idleAtMs - cycle.captureStoppedAtMs > 45_500 ||
        cycle.activeCapturesAfterStop !== 0 || source?.name !== plan.episode ||
        source.captureGeneration !== index + 1 || !Number.isSafeInteger(source.emittedFrames) ||
        source.emittedFrames < 0 || source.emittedFrames > source.sourceFrames ||
        source.sourceGateRequired !== gated ||
        (gated && (source.sourceGateReady?.serverReady !== true || source.sourceGateReady.emittedFrames !== 0)) ||
        (!gated && source.sourceGateReady != null) ||
        !Number.isSafeInteger(cycle.eventStart) || !Number.isSafeInteger(cycle.eventEnd) ||
        cycle.eventStart < 0 || cycle.eventEnd < cycle.eventStart || cycle.eventEnd > events.length ||
        (index === 0 ? cycle.eventStart !== 0 : cycle.eventStart !== cycles[index - 1].eventEnd) ||
        cycleEvents.some(event => event.cycleIndex !== index || !Number.isSafeInteger(event.sessionId) || event.sessionId <= 0)) {
      throw new Error(`Warm provider canary cycle ${index} evidence is contradictory`);
    }
    if (plan.stopPhase === 'before-ready') {
      if (cycle.trigger?.readyBeforeStop !== false ||
          cycleEvents.some(event => ['transcription:partial', 'transcription:final'].includes(event.event)) ||
          (cycle.association != null && (cycle.association.captureGeneration !== index + 1 ||
            cycle.association.captureRunId !== cycle.logicalRunId))) {
        throw new Error(`Cycle ${index} missed before-Ready proof`);
      }
    } else {
      const association = associations.get(index + 1);
      if (!association || cycle.association?.captureGeneration !== index + 1 ||
          cycle.association.captureRunId !== cycle.logicalRunId ||
          association.captureRunId !== cycle.logicalRunId || source.emittedFrames <= 0 ||
          cycleEvents.some(event => ['transcription:partial', 'transcription:final'].includes(event.event) &&
            event.sessionId !== cycle.logicalRunId)) {
        throw new Error(`Cycle ${index} lost capture/provider ownership`);
      }
      const expectedEvent = plan.stopPhase === 'during-partial' ? 'transcription:partial' :
        plan.stopPhase === 'after-final' ? 'transcription:final' : null;
      if (expectedEvent && !cycleEvents.some(event => event.event === expectedEvent)) {
        throw new Error(`Cycle ${index} missed ${expectedEvent} evidence`);
      }
    }
  }
  const finalIndex = trial.finalEpisodeIndex;
  const finalSource = fixture.sourceEpisodes[finalIndex];
  const finalAssociation = associations.get(finalIndex + 1);
  const finalEvents = events.filter(event => event.cycleIndex === finalIndex);
  const terminalEvents = events.filter(event => event.event === 'transcription:terminal');
  const terminals = report.terminals;
  if (finalSource?.name !== trial.episodes[finalIndex] ||
      finalSource.captureGeneration !== finalIndex + 1 ||
      finalSource.emittedFrames !== finalSource.sourceFrames ||
      !Number.isFinite(finalSource.nativeSourceStartMs) || !Number.isFinite(finalSource.nativeSourceEndMs) ||
      finalSource.nativeSourceEndMs < finalSource.nativeSourceStartMs ||
      finalSource.sourceGateRequired !== true || finalSource.sourceGateReady?.serverReady !== true ||
      finalSource.sourceGateReady.emittedFrames !== 0 || !finalAssociation ||
      typeof report.expectedInsertion !== 'string' || !report.expectedInsertion.trim() ||
      new Set(finalEvents.flatMap(event => event.markerIds)).size < 2 ||
      finalEvents.some(event => ['transcription:partial', 'transcription:final'].includes(event.event) &&
        event.sessionId !== finalAssociation.captureRunId) ||
      events.some((event, eventIndex) => Number.isSafeInteger(event.cycleIndex) &&
        event.cycleIndex >= 0 && event.cycleIndex < finalIndex &&
        (eventIndex < cycles[event.cycleIndex].eventStart || eventIndex >= cycles[event.cycleIndex].eventEnd)) ||
      !Array.isArray(terminals) || terminals.length !== 1 || terminals[0]?.complete !== true ||
      terminals[0].sessionId !== finalAssociation.captureRunId || terminals[0].cycleIndex !== finalIndex ||
      terminalEvents.length !== 1 || terminalEvents[0].sessionId !== finalAssociation.captureRunId ||
      terminalEvents[0].cycleIndex !== finalIndex ||
      report.final.status !== 'Idle' || report.final.preparedCaptureTokenCount !== 0 ||
      report.final.providerTransport?.connectionRetained !== false) {
    throw new Error('Final warm provider proof is incomplete');
  }
  return { churnCycles: cycles.length, finalCaptureGeneration: finalIndex + 1,
    stopPhases: [...new Set(cycles.map(cycle => cycle.stopPhase))],
    jitterMs: [...new Set(cycles.map(cycle => cycle.jitterMs))] };
}
