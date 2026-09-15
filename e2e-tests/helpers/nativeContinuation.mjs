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
export const maxProxyEvidenceEvents = 4096;
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
      if (boundarySeen || active !== 0 || connections !== expected.backendConnections || event.nativeProcessAlive !== true || event.clock !== 'runner-performance-now' || !Number.isFinite(event.atMs)) throw new Error('Normal Stop not closed before teardown');
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
  if (!boundarySeen || connections !== expected.backendConnections || active !== 0) throw new Error('Mode-specific backend connection count/cleanup failed');
  return { backendConnections: connections, expectedProviderHandshakes: expected.providerHandshakes,
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
  if (connectionIds.size !== expected.backendConnections || connections.some(event =>
      !Number.isSafeInteger(event.connectionId) || event.connectionId <= 0) ||
      binary.some(event => !connectionIds.has(event.connectionId))) {
    throw new Error('Audio evidence does not belong to the expected connections');
  }
  for (const connectionId of connectionIds) {
    if (!binary.some(event => event.connectionId === connectionId)) throw new Error('Expected connection has no client audio');
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
