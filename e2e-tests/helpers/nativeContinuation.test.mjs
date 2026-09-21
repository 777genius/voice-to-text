import test from 'node:test';
import assert from 'node:assert/strict';
import { approvedFixtures, maxProxyEvidenceEvents, validatePcm, readApprovedFixtures, liveTrials,
  warmProviderCanaryJittersMs, warmProviderCanaryPhases, warmProviderCanaryTrial,
  validateHarnessConfig, exactInsertionEvidence } from './nativeContinuation.mjs';
import { parseArguments, sanitizedEnvironment, validateResult } from '../run-native-window-e2e.mjs';
test('qualification requires explicit opt in and inherits no feature flags', () => {
  assert.deepEqual(parseArguments(['--continuation-fake']), { continuationFake: true });
  assert.equal(sanitizedEnvironment('/tmp/example', { VOICETEXT_EL_PAUSE_CONTINUE_V1: 'true' }).VOICETEXT_EL_PAUSE_CONTINUE_V1, undefined);
});
test('fixed live budget includes all trials and full long clock', async () => {
  assert.equal(liveTrials.length, 13);
  assert.equal(liveTrials.filter(x => x.id.startsWith('warm-baseline')).length, 3);
  assert.equal(liveTrials.filter(x => x.id.startsWith('warm-continue')).length, 3);
  assert.equal(new Set(liveTrials.map(x => x.id)).size, 13);
  assert.equal(approvedFixtures['long-auto-commit.pcm'][0] / 32, 49268);
  const longestTwoEpisodeBytes = approvedFixtures['long-auto-commit.pcm'][0] + approvedFixtures['episode-b.pcm'][0];
  assert.ok(Math.ceil(longestTwoEpisodeBytes / 640) + 128 < maxProxyEvidenceEvents);
  const rows = await readApprovedFixtures(new URL('../../../qualification-fixtures', import.meta.url).pathname);
  assert.equal(rows.length, 5);
  for (const row of rows) assert.equal(validatePcm(row.name, row.pcm).sourceFrames * 2, row.bytes);
});
test('arbitrary names, changed bytes and tail truncation are rejected', () => {
  for (const name of ['../episode-a.pcm', 'toString', 'episode-a.pcm']) assert.throws(() => validatePcm(name, Buffer.alloc(42288)));
  assert.throws(() => validatePcm('long-auto-commit.pcm', Buffer.alloc(788288)));
});
test('fresh TEST provenance and no credentials are required', () => {
  const good = { schema: 'p4-test-backend-v1', testOnly: true, backendSourceSha256: 'a'.repeat(64), disposableInstanceId: 'test-instance-1', backendPid: 42, startedAtUnixMs: 1, endpoint: 'ws://127.0.0.1:52999' };
  assert.deepEqual(validateHarnessConfig(good), good);
  for (const endpoint of ['ws://127.0.0.1:51866', 'wss://api.example.com', 'ws://token@127.0.0.1:52999']) assert.throws(() => validateHarnessConfig({ ...good, endpoint }));
  assert.throws(() => validateHarnessConfig({ ...good, token: 'forbidden' }));
});
test('exact readback rejects substring and arbitrary document identity', () => {
  assert.throws(() => exactInsertionEvidence('A B', 'prefix A B', 'p4-textedit-a.txt'));
  assert.throws(() => exactInsertionEvidence('A B', 'A B', 'Untitled'));
  assert.equal(exactInsertionEvidence('A B', 'A B', 'p4-textedit-a.txt').actualPasteVerified, true);
});
test('errors cannot be overwritten by a passing continuation report', () => {
  assert.throws(() => validateResult({ marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', passed: true,
    report: { mode: 'continuation-fake', passed: true, completedCycles: 50, errors: ['late failure'] } }));
});
test('live selection is one predetermined trial with explicit trusted config, never an implicit matrix retry', () => {
  for (const trial of liveTrials) assert.deepEqual(parseArguments(['--qualification-live', '/tmp/p4-backend.json', trial.id]), { harnessConfig: '/tmp/p4-backend.json', trialId: trial.id });
  for (const args of [ ['--qualification-live', 'relative.json', 'long'], ['--qualification-live', '/tmp/config.json', 'retry'], ['--qualification-live', '/tmp/config.json', 'all'] ]) assert.throws(() => parseArguments(args));
});
test('adversarial cases require exact bounded selection and retained native evidence', () => {
  for (const selected of ['seal-stop', 'seal-hold', 'seal-close', 'cancel', 'stale-epoch', 'terminal-before-write', 'E04', 'E41', 'E42']) {
    assert.deepEqual(parseArguments(['--continuation-case', selected]), { continuationFake: true, continuationCase: selected });
  }
  assert.throws(() => parseArguments(['--continuation-case', 'arbitrary']));
  const envelope = { marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', passed: true,
    fixture: { activeCaptures: 0, activeProviders: 0, captureStarts: 2, captureStops: 2,
      providerStarts: 1, maxActiveProviders: 1, providerResumes: 0, finals: 1, markerViolations: [] },
    report: { mode: 'continuation-case', case: 'cancel', passed: true, errors: [],
      micReleasedBeforeAccepted: true, cleanup: true, restored: true, firstBWrites: 0 } };
  assert.equal(validateResult(envelope), envelope.report);
  for (const edit of [v => { v.report.firstBWrites = 1; }, v => { v.report.restored = false; },
    v => { v.report.micReleasedBeforeAccepted = false; }, v => { v.fixture.providerStarts = 2; },
    v => { v.report.errors.push('late error'); }]) {
    const invalid = structuredClone(envelope); edit(invalid); assert.throws(() => validateResult(invalid));
  }
});
test('cold capture source outlasts every Config fault at the original sample clock', () => {
  const cold = liveTrials.filter(trial => trial.id.startsWith('cold-'));
  assert.deepEqual(cold.map(trial => trial.configDelayMs), [0, 4000, 8000]);
  for (const trial of cold) {
    const bytes = approvedFixtures[trial.episodes[0]][0];
    assert.ok(bytes / 32 > trial.configDelayMs + 1000);
    assert.equal(trial.episodes[0], 'long-auto-commit.pcm');
  }
});

test('paid warm provider canary fixes 20 churn cycles, four stop phases, five jitters and one full proof', async () => {
  const { qualificationExpectations, verifyQualificationConnections, verifyQualificationRoute,
    verifyWarmProviderCanary, verifyWarmProviderTransport } = await import('./nativeContinuation.mjs');
  assert.equal(warmProviderCanaryTrial.cycles.length, 20);
  assert.deepEqual([...new Set(warmProviderCanaryTrial.cycles.map(cycle => cycle.stopPhase))], warmProviderCanaryPhases);
  assert.deepEqual([...new Set(warmProviderCanaryTrial.cycles.map(cycle => cycle.jitterMs))], warmProviderCanaryJittersMs);
  assert.equal(new Set(warmProviderCanaryTrial.cycles.map(cycle => cycle.episode)).size, 2);
  assert.ok(!warmProviderCanaryTrial.cycles.some(cycle =>
    cycle.episode === warmProviderCanaryTrial.episodes[warmProviderCanaryTrial.finalEpisodeIndex]));
  const worstCaseFrames = warmProviderCanaryTrial.episodes.reduce((total, name) =>
    total + Math.ceil(approvedFixtures[name][0] / 640), 0);
  assert.ok(worstCaseFrames + 512 < maxProxyEvidenceEvents);
  const expected = qualificationExpectations(warmProviderCanaryTrial);
  assert.equal(expected.captures, 21);
  assert.deepEqual([expected.minBackendConnections, expected.maxBackendConnections], [1, 7]);
  const connected = { event: 'fault_proxy_connected', connectionId: 1 };
  const audio = { event: 'client_binary', connectionId: 1, bytes: 640 };
  const ready = { event: 'backend_control', type: 'ready', connectionId: 1, session_id: 'provider-1' };
  const controls = Array.from({ length: 15 }, (_, index) => [
    { event: 'backend_control', type: 'pause_accepted', connectionId: 1,
      provider_session_id: 'provider-1', decision: 'accepted', request_id: `pause-${index}` },
    { event: 'backend_control', type: 'continue_result', connectionId: 1,
      provider_session_id: 'provider-1', decision: 'accepted', eligible_now: true,
      request_id: `continue-${index}` },
  ]).flat();
  controls.push({ event: 'backend_control', type: 'pause_accepted', connectionId: 1,
    provider_session_id: 'provider-1', decision: 'accepted', request_id: 'pause-final' });
  const closed = { event: 'fault_proxy_close', connectionId: 1, direction: 'upstream', code: 1000 };
  const boundary = { event: 'qualification_pre_teardown', atMs: 10,
    clock: 'runner-performance-now', nativeProcessAlive: true };
  assert.equal(verifyQualificationConnections(warmProviderCanaryTrial,
    [connected, audio, closed, boundary]).backendConnections, 1);
  assert.throws(() => verifyQualificationConnections(warmProviderCanaryTrial,
    [connected, audio, { ...closed, connectionId: 999 }, boundary]), /Unmatched upstream close/);
  assert.equal(verifyQualificationRoute(warmProviderCanaryTrial,
    [connected, ready, ...controls.slice(0, -1), audio, controls.at(-1)]).maximumActiveProviderSessions, 1);
  const earlyConnected = { event: 'fault_proxy_connected', connectionId: 41 };
  const earlyReady = { event: 'backend_control', type: 'ready', connectionId: 41, session_id: 'early-provider' };
  const retainedConnected = { event: 'fault_proxy_connected', connectionId: 42 };
  const retainedReady = { ...ready, connectionId: 42 };
  const retainedControls = controls.map(event => ({ ...event, connectionId: 42 }));
  assert.equal(verifyQualificationRoute(warmProviderCanaryTrial,
    [earlyConnected, { ...audio, connectionId: 41 }, earlyReady,
      retainedConnected, retainedReady, ...retainedControls.slice(0, -1),
      { ...audio, connectionId: 42 }, retainedControls.at(-1)]).retainedProviderSessionId,
  'provider-1');
  const audioWhilePaused = [connected, ready, controls[0], audio, ...controls.slice(1)];
  assert.throws(() => verifyQualificationRoute(warmProviderCanaryTrial, audioWhilePaused),
    /audio while provider session was paused/);
  assert.throws(() => verifyQualificationRoute(warmProviderCanaryTrial, [connected]));
  assert.throws(() => verifyQualificationRoute(warmProviderCanaryTrial,
    [connected, ready, ...controls.slice(1), audio]));

  const events = [];
  const terminals = [];
  let cycleClock = 0;
  const cycles = warmProviderCanaryTrial.cycles.map((plan, index) => {
    const logicalRunId = index < warmProviderCanaryTrial.readyGateFromIndex
      ? 100 + index : 100 + warmProviderCanaryTrial.readyGateFromIndex;
    const captureRunId = 100 + index;
    const eventStart = events.length;
    const markerId = plan.episode === 'episode-a.pcm' ? 0 : 1;
    const phrase = markerId === 0 ? 'на столе лежит книга' : 'за окном растет береза';
    const triggerDeliverySeqFloor = events.filter(event => event.sessionId === logicalRunId &&
      Number.isSafeInteger(event.deliverySeq))
      .reduce((maximum, event) => Math.max(maximum, event.deliverySeq), 0);
    const providerStartSamples = index < warmProviderCanaryTrial.readyGateFromIndex
      ? null : (index - warmProviderCanaryTrial.readyGateFromIndex) * 320;
    if (plan.stopPhase === 'during-partial') events.push({ event: 'transcription:partial', text: phrase,
      cycleIndex: index, sessionId: logicalRunId, deliverySeq: null, markerIds: [markerId],
      timingKnown: true, sourceStartSeconds: providerStartSamples / 16000, sourceDurationSeconds: 0.02 });
    if (plan.stopPhase === 'after-final') events.push({ event: 'transcription:final', text: phrase,
      cycleIndex: index, sessionId: logicalRunId, deliverySeq: index + 1, markerIds: [markerId],
      timingKnown: false, sourceStartSeconds: 0, sourceDurationSeconds: 0 });
    const stopEventIndex = events.length;
    if (plan.stopPhase === 'before-ready') {
      events.push({ event: 'transcription:terminal', cycleIndex: index, sessionId: logicalRunId,
        deliverySeq: null, markerIds: [] });
      terminals.push({ sessionId: logicalRunId, cycleIndex: index, complete: true });
    }
    const startedAtMs = cycleClock;
    const previousSettleToStartMs = index === 0 ? null : warmProviderCanaryTrial.cycles[index - 1].jitterMs;
    const cycle = { ...plan, startedAtMs, previousSettleToStartMs, triggerAtMs: startedAtMs + 20,
      captureStoppedAtMs: startedAtMs + 30, settledAtMs: startedAtMs + 50,
      captureGeneration: index + 1, logicalRunId, captureRunId,
      captureFenceGeneration: index + 1,
      triggerEventStart: eventStart,
      stopEventIndex,
      callbackFenceGeneration: index > warmProviderCanaryTrial.readyGateFromIndex ? index + 1 : null,
      triggerProviderSamples: plan.stopPhase === 'before-ready' ? null : 320,
      providerStartSamples, triggerDeliverySeqFloor: plan.stopPhase === 'before-ready'
        ? null : triggerDeliverySeqFloor,
      association: plan.stopPhase === 'before-ready' ? null : {
        captureGeneration: index + 1, captureRunId, captureFenceGeneration: index + 1 },
      trigger: plan.stopPhase === 'before-ready' ? { readyBeforeStop: false,
        providerTransportBeforeStop: { serverReady: false, connectionRetained: false },
        statusBeforeStop: 'Starting', nativeBoundaryMs: index + 1 } :
        plan.stopPhase === 'during-partial' ? { event: 'transcription:partial', episode: plan.episode,
          deliverySeq: null } :
        plan.stopPhase === 'after-final' ? { event: 'transcription:final', episode: plan.episode,
          deliverySeq: index + 1 } : { emittedFrames: 320 },
      eventStart, eventEnd: events.length, activeCapturesAfterStop: 0 };
    cycleClock = cycle.settledAtMs + plan.jitterMs;
    return cycle;
  });
  const finalLogicalRunId = 100 + warmProviderCanaryTrial.readyGateFromIndex;
  const finalProviderStartSamples =
    (warmProviderCanaryTrial.cycles.length - warmProviderCanaryTrial.readyGateFromIndex) * 320;
  const finalSourceFrames = approvedFixtures['long-auto-commit.pcm'][0] / 2;
  events.push({ event: 'transcription:final', cycleIndex: 20, sessionId: finalLogicalRunId,
    deliverySeq: 99, text: 'на столе лежит книга за окном растет береза', markerIds: [0, 1],
    timingKnown: false, sourceStartSeconds: 0, sourceDurationSeconds: 0 });
  events.push({ event: 'transcription:terminal', cycleIndex: 20, sessionId: finalLogicalRunId,
    deliverySeq: null, markerIds: [] });
  terminals.push({ sessionId: finalLogicalRunId, cycleIndex: 20, complete: true });
  const sources = warmProviderCanaryTrial.episodes.map((name, index) => {
    const bytes = approvedFixtures[name][0];
    const sourceFrames = bytes / 2;
    const sourceDurationMs = bytes / 32;
    const emittedFrames = index === 20 ? sourceFrames :
      warmProviderCanaryTrial.cycles[index].stopPhase === 'before-ready' ? 0 : 320;
    const chunks = Math.ceil(emittedFrames / 320);
    const nativeSourceStartMs = emittedFrames > 0 ? 1000 + index * 100_000 : null;
    const lastSourceFrameElapsedMs = emittedFrames > 0 ? (chunks - 1) * 20 : null;
    return { name, bytes, captureGeneration: index + 1, sourceFrames, sourceDurationMs, cadenceMs: 20,
      emittedFrames, nativeSourceStartMs,
      nativeLastSourceFrameMs: emittedFrames > 0 ? nativeSourceStartMs + lastSourceFrameElapsedMs : null,
      lastSourceFrameElapsedMs, pacingIntervalsChecked: emittedFrames > 0 ? chunks - 1 : undefined,
      pacingViolations: emittedFrames > 0 ? 0 : undefined,
      nativeSourceEndMs: index === 20 ? nativeSourceStartMs + sourceDurationMs : null,
      sourceGateRequired: index >= warmProviderCanaryTrial.readyGateFromIndex,
      sourceGateReady: index >= warmProviderCanaryTrial.readyGateFromIndex ?
        { serverReady: true, status: 'Processing', nativeReadyMs: 900, emittedFrames: 0 } : null };
  });
  const captureRunAssociations = cycles.flatMap((cycle, index) => cycle.association ? [cycle.association] : [])
    .concat({ captureGeneration: 21, captureRunId: 999, captureFenceGeneration: 21 });
  const capturePcmLedgers = sources.map((source, index) => ({
    captureGeneration: index + 1,
    chunks: Math.ceil(source.emittedFrames / 320),
    samples: source.emittedFrames,
    hash: source.emittedFrames > 0 ? '0123456789abcdef' : 'cbf29ce484222325',
  }));
  const fixture = { captureStarts: 21, captureStops: 21, activeCaptures: 0, maxActiveCaptures: 1,
    observationOverflow: false, markerViolations: [], sourceEpisodes: sources,
    captureRunAssociations, capturePcmLedgers,
    providerPcmLedgers: capturePcmLedgers.filter(row => row.samples > 0).map(row => ({ ...row })),
    providerCallbackGenerations: Array.from({ length: 15 }, (_, index) => index + 7) };
  const report = { mode: 'warm-provider-canary', passed: true, trialId: warmProviderCanaryTrial.id,
    errors: [], duplicateDeliveries: [], cycles, events, finalTextBeforeProof: 'stale transcript',
    expectedInsertion: 'stable transcript',
    finalStartedAtMs: cycleClock,
    finalCallbackFence: { captureGeneration: 21, eventStart: events.length - 2 },
    finalTranscriptFence: { eventStart: events.length - 2,
      providerSamples: sources[20].sourceFrames, providerStartSamples: finalProviderStartSamples,
      deliverySeqFloor: events.slice(0, -2).filter(event => event.sessionId === finalLogicalRunId &&
        Number.isSafeInteger(event.deliverySeq))
        .reduce((maximum, event) => Math.max(maximum, event.deliverySeq), 0) },
    finalOwnership: { logicalRunId: finalLogicalRunId, captureRunId: 999, captureFenceGeneration: 21 },
    terminals, final: { status: 'Idle', preparedCaptureTokenCount: 0,
      providerTransport: { connectionRetained: false }, fixture } };
  assert.equal(verifyWarmProviderCanary(warmProviderCanaryTrial, report).churnCycles, 20);
  const ledgerBytes = fixture.providerPcmLedgers.reduce((sum, row) => sum + row.samples * 2, 0);
  const transportEvents = [{ event: 'fault_proxy_connected', connectionId: 1 },
    { event: 'backend_control', type: 'ready', connectionId: 1 }];
  fixture.providerPcmLedgers.forEach((ledger, index) => {
    if (index > 0) transportEvents.push({ event: 'backend_control', type: 'continue_result',
      decision: 'accepted', eligible_now: true, connectionId: 1 });
    transportEvents.push({ event: 'client_binary', connectionId: 1,
      bytes: ledger.samples * 2, pcmHash: ledger.hash });
    transportEvents.push({ event: 'backend_control', type: 'pause_accepted',
      decision: 'accepted', connectionId: 1 });
  });
  transportEvents.push({ event: 'fault_proxy_close', connectionId: 1, direction: 'upstream', code: 1000 });
  assert.equal(verifyWarmProviderTransport(transportEvents, fixture)
    .transmittedPcmBytes, ledgerBytes);
  const wrongBytes = structuredClone(transportEvents);
  wrongBytes.find(event => event.event === 'client_binary').bytes -= 2;
  assert.throws(() => verifyWarmProviderTransport(wrongBytes, fixture), /does not match/);
  const wrongConnection = structuredClone(transportEvents);
  wrongConnection.find(event => event.event === 'client_binary').connectionId = 2;
  assert.throws(() => verifyWarmProviderTransport(wrongConnection, fixture), /open connection/);
  const wrongInterval = structuredClone(transportEvents);
  const firstPause = wrongInterval.findIndex(event => event.type === 'pause_accepted');
  wrongInterval.splice(firstPause + 1, 0, { event: 'client_binary', connectionId: 1,
    bytes: 2, pcmHash: '0123456789abcdef' });
  const wrongHash = structuredClone(transportEvents);
  wrongHash.find(event => event.event === 'client_binary').pcmHash = 'ffffffffffffffff';
  assert.throws(() => verifyWarmProviderTransport(wrongHash, fixture), /does not match/);
  assert.throws(() => verifyWarmProviderTransport(wrongInterval, fixture), /paused interval/);
  assert.equal(verifyWarmProviderTransport([
    { event: 'fault_proxy_connected', connectionId: 1 },
    { event: 'client_binary', connectionId: 1, bytes: 4, pcmHash: '0123456789abcdef' },
    { event: 'fault_proxy_close', connectionId: 1, direction: 'upstream', code: 1000 },
    { event: 'fault_proxy_connected', connectionId: 2 },
    { event: 'backend_control', type: 'ready', connectionId: 2 },
    { event: 'client_binary', connectionId: 2, bytes: 6, pcmHash: 'fedcba9876543210' },
    { event: 'backend_control', type: 'pause_accepted', decision: 'accepted', connectionId: 2 },
    { event: 'fault_proxy_close', connectionId: 2, direction: 'upstream', code: 1000 },
  ], { providerPcmLedgers: [
    { captureGeneration: 1, chunks: 1, samples: 2, hash: '0123456789abcdef' },
    { captureGeneration: 2, chunks: 1, samples: 3, hash: 'fedcba9876543210' },
  ] }).transmittedPcmBytes, 10);
  const missingEarlyTerminal = structuredClone(report);
  missingEarlyTerminal.terminals.shift();
  assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, missingEarlyTerminal),
    /Final warm provider proof is incomplete/);
  const alreadyReadyAtEarlyStop = structuredClone(report);
  const earlyCycle = alreadyReadyAtEarlyStop.cycles.find(row => row.stopPhase === 'before-ready');
  earlyCycle.trigger.readyBeforeStop = true;
  earlyCycle.trigger.providerTransportBeforeStop.serverReady = true;
  assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, alreadyReadyAtEarlyStop),
    /before-Ready proof/);
  const missingNativeStopBoundary = structuredClone(report);
  delete missingNativeStopBoundary.cycles.find(row => row.stopPhase === 'before-ready')
    .trigger.nativeBoundaryMs;
  assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, missingNativeStopBoundary),
    /before-Ready proof/);
  for (const mutate of [
    value => value.cycles.pop(),
    value => { value.cycles[5].association.captureRunId = 9999; },
    value => { value.events.find(event => event.event === 'transcription:partial').cycleIndex = 99; },
    value => { const cycle = value.cycles.find(row => row.stopPhase === 'during-partial');
      const event = value.events.slice(cycle.triggerEventStart, cycle.eventEnd)
        .find(row => row.event === 'transcription:partial');
      event.text = cycle.episode === 'episode-a.pcm' ? 'за окном растет береза' : 'на столе лежит книга'; },
    value => { value.final.fixture.sourceEpisodes[20].emittedFrames--; },
    value => { value.final.fixture.capturePcmLedgers.pop(); },
    value => { value.final.fixture.providerPcmLedgers[0].hash = 'ffffffffffffffff'; },
    value => { value.final.fixture.capturePcmLedgers.forEach(row => { row.chunks = 0; row.samples = 0; row.hash = 'cbf29ce484222325'; });
      value.final.fixture.providerPcmLedgers = []; },
    value => { const source = value.final.fixture.sourceEpisodes[20];
      source.bytes = 640; source.sourceFrames = 320; source.emittedFrames = 320;
      source.sourceDurationMs = 20; source.nativeSourceEndMs = source.nativeSourceStartMs; },
    value => { value.final.fixture.sourceEpisodes[20].pacingIntervalsChecked = 0; },
    value => { value.final.fixture.sourceEpisodes[20].lastSourceFrameElapsedMs = 0; },
    value => { value.finalTextBeforeProof = value.expectedInsertion; },
    value => { value.final.fixture.providerCallbackGenerations.pop(); },
    value => { value.finalCallbackFence.captureGeneration = 20; },
    value => { value.finalCallbackFence.eventStart = value.events.length; },
    value => { value.finalTranscriptFence.eventStart = value.events.length - 1; },
    value => { value.finalTranscriptFence.deliverySeqFloor = 99; },
    value => { const cycle = value.cycles.find(row => row.stopPhase === 'after-final');
      cycle.triggerEventStart += 1; },
    value => { const cycle = value.cycles.find(row => row.stopPhase === 'after-final');
      const event = value.events.slice(cycle.triggerEventStart, cycle.stopEventIndex)
        .find(row => row.event === 'transcription:final');
      event.text = cycle.episode === 'episode-a.pcm' ? 'за окном растет береза' : 'на столе лежит книга';
      event.markerIds = [cycle.episode === 'episode-a.pcm' ? 1 : 0]; },
    value => { const cycle = value.cycles.find(row => row.stopPhase === 'after-final');
      const event = value.events.slice(cycle.triggerEventStart, cycle.stopEventIndex)
        .find(row => row.event === 'transcription:final');
      event.timingKnown = true;
      event.sourceStartSeconds = Math.max(0, (cycle.providerStartSamples - 320) / 16000);
      event.sourceDurationSeconds = 0.02; },
    value => { const cycle = value.cycles.find(row => row.stopPhase === 'after-final' &&
        row.providerStartSamples === 3_840);
      const event = value.events.slice(cycle.triggerEventStart, cycle.stopEventIndex)
        .find(row => row.event === 'transcription:final');
      event.timingKnown = true;
      event.sourceStartSeconds = 0.1;
      event.sourceDurationSeconds = 0.14; },
    value => { const cycle = value.cycles.find(row => row.stopPhase === 'after-final');
      const event = value.events.slice(cycle.triggerEventStart, cycle.stopEventIndex)
        .find(row => row.event === 'transcription:final');
      cycle.triggerDeliverySeqFloor = event.deliverySeq; },
    value => { value.cycles[1].previousSettleToStartMs = 5_000; },
    value => { delete value.events.find(event => event.event === 'transcription:final').deliverySeq; },
    value => { value.events.find(event => event.cycleIndex === 20 && event.event === 'transcription:final').event = 'transcription:partial'; },
    value => { value.finalOwnership.logicalRunId = value.finalOwnership.captureRunId; },
    value => value.terminals.push({ sessionId: finalLogicalRunId, cycleIndex: 20, complete: true }),
    value => { value.terminals[0].sessionId = 123; },
    value => value.duplicateDeliveries.push('1:1:final'),
    value => value.events.push({ event: 'transcription:final', cycleIndex: 19,
      sessionId: 119, deliverySeq: 1000, markerIds: [] }),
  ]) {
    const invalid = structuredClone(report); mutate(invalid);
    assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, invalid));
  }
  const insertBeforeStop = (value, cycle, event) => {
    const at = cycle.stopEventIndex;
    value.events.splice(at, 0, event);
    for (const row of value.cycles) {
      if (row.index === cycle.index) {
        row.stopEventIndex += 1;
        row.eventEnd += 1;
      } else if (row.index > cycle.index) {
        row.eventStart += 1;
        row.triggerEventStart += 1;
        row.stopEventIndex += 1;
        row.eventEnd += 1;
      }
    }
    value.finalCallbackFence.eventStart += 1;
    value.finalTranscriptFence.eventStart += 1;
  };
  const collapsedPartial = structuredClone(report);
  const partialCycle = collapsedPartial.cycles.find(row => row.stopPhase === 'during-partial');
  insertBeforeStop(collapsedPartial, partialCycle, { event: 'transcription:final',
    cycleIndex: partialCycle.index, sessionId: partialCycle.logicalRunId, deliverySeq: 700,
    text: partialCycle.episode === 'episode-a.pcm' ? 'на столе лежит книга' : 'за окном растет береза',
    markerIds: [partialCycle.episode === 'episode-a.pcm' ? 0 : 1] });
  assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, collapsedPartial),
    /stop phase collapsed/);
  const collapsedFirstPcm = structuredClone(report);
  const firstPcmCycle = collapsedFirstPcm.cycles.find(row => row.stopPhase === 'after-first-pcm' &&
    row.index > warmProviderCanaryTrial.readyGateFromIndex);
  insertBeforeStop(collapsedFirstPcm, firstPcmCycle, { event: 'transcription:partial',
    cycleIndex: firstPcmCycle.index, sessionId: firstPcmCycle.logicalRunId, deliverySeq: null,
    text: firstPcmCycle.episode === 'episode-a.pcm' ? 'на столе' : 'за окном',
    markerIds: [firstPcmCycle.episode === 'episode-a.pcm' ? 0 : 1] });
  firstPcmCycle.triggerEventStart += 1;
  assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, collapsedFirstPcm),
    /stop phase collapsed/);
  const duplicateFinal = structuredClone(report);
  const finalCycle = duplicateFinal.cycles.find(row => row.stopPhase === 'after-final');
  const firstFinal = duplicateFinal.events.slice(finalCycle.triggerEventStart, finalCycle.stopEventIndex)
    .find(row => row.event === 'transcription:final');
  insertBeforeStop(duplicateFinal, finalCycle, { ...firstFinal, deliverySeq: 701 });
  assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, duplicateFinal),
    /Final warm provider proof is incomplete/);
  for (const text of [
    'на столе лежит книга за окном растет береза',
    'на столе лежит книга на столе лежит книга',
  ]) {
    const contaminated = structuredClone(report);
    const cycle = contaminated.cycles.find(row => row.stopPhase === 'after-final' &&
      row.episode === 'episode-a.pcm');
    const event = contaminated.events.slice(cycle.triggerEventStart, cycle.stopEventIndex)
      .find(row => row.event === 'transcription:final');
    event.text = text;
    event.markerIds = text.includes('за окном') ? [0, 1] : [0];
    assert.throws(() => verifyWarmProviderCanary(warmProviderCanaryTrial, contaminated));
  }
});

test('mode counts and initial-only Ready gate preserve the legacy prescribed trials', async () => {
  const { qualificationExpectations } = await import('./nativeContinuation.mjs');
  for (const trial of liveTrials.filter(row => row.kind !== 'warm-provider-canary')) {
    const expected = qualificationExpectations(trial);
    assert.equal(expected.captures, trial.id.startsWith('warm-baseline-') ? 1 : 2);
    assert.equal(expected.backendConnections, trial.id.startsWith('cold-') ? 2 : 1);
    assert.equal(expected.providerHandshakes, expected.backendConnections);
    assert.equal(expected.gateInitialSource, !trial.id.startsWith('cold-'));
    assert.equal(expected.gapMs, 120);
  }
});
test('connection verifier rejects overlap, retry, missing closes and every retained proxy failure', async () => {
  const { verifyQualificationConnections } = await import('./nativeContinuation.mjs');
  const opened = connectionId => ({ event: 'fault_proxy_connected', connectionId });
  const closed = connectionId => ({ event: 'fault_proxy_close', connectionId, direction: 'upstream', code: 1000 });
  const boundary = { event: 'qualification_pre_teardown', atMs: 10, clock: 'runner-performance-now', nativeProcessAlive: true };
  for (const trial of liveTrials) {
    const events = trial.id.startsWith('cold-') ? [opened(1), closed(1), opened(2), closed(2)] : [opened(1), closed(1)];
    assert.equal(verifyQualificationConnections(trial, [...events, boundary]).providerHandshakeVerification, 'pending-parent-logs');
    assert.throws(() => verifyQualificationConnections(trial, [...events, opened(3), closed(3)]));
    assert.throws(() => verifyQualificationConnections(trial, events.slice(0, -1)));
    for (const event of ['fault_proxy_overflow', 'fault_proxy_transport_error', 'fault_proxy_deadline', 'fault_proxy_evidence_overflow'])
      assert.throws(() => verifyQualificationConnections(trial, [...events, { event }]));
  }
  assert.throws(() => verifyQualificationConnections(liveTrials.find(t => t.id === 'cold-0'),
    [opened(1), opened(2), closed(1), closed(2)]));
});
test('source verification requires native Ready, complete paced PCM and continuous baseline gap', async () => {
  const { verifyQualificationSources, qualificationExpectations } = await import('./nativeContinuation.mjs');
  for (const trial of liveTrials.filter(row => row.kind !== 'warm-provider-canary')) {
    const expected = qualificationExpectations(trial);
    const sourceEpisodes = trial.episodes.map((name, index) => {
      const [bytes] = approvedFixtures[name];
      const gated = index === 0 && expected.gateInitialSource;
      return { name, bytes, sourceFrames: bytes / 2, emittedFrames: bytes / 2,
        captureGeneration: expected.baseline ? 1 : index + 1, sourceGateRequired: gated,
        sourceGateReady: gated ? { serverReady: true, status: 'Recording', nativeReadyMs: 90, emittedFrames: 0 } : null,
        nativeLastSourceFrameMs: (index === 0 ? 100 : 2120) + (Math.ceil(bytes / 640) - 1) * 20,
        lastSourceFrameElapsedMs: (Math.ceil(bytes / 640) - 1) * 20,
        pacingIntervalsChecked: (Math.ceil(bytes / 640) - 1) + (expected.baseline && index === 1 ? Math.ceil(approvedFixtures[trial.episodes[0]][0] / 640) + 6 : 0), pacingViolations: 0,
        cadenceMs: 20, continuousCapture: expected.baseline, gapBeforeMs: index === 1 ? 120 : 0,
        gapFrames: 1920, nativeGapStartMs: 2000, nativeSourceStartMs: index === 0 ? 100 : 2120,
        nativeSourceEndMs: (index === 0 ? 100 : 2120) + bytes / 32 };
    });
    const fixture = { captureStarts: expected.captures, captureStops: expected.captures, activeCaptures: 0, sourceEpisodes };
    assert.deepEqual(verifyQualificationSources(trial, fixture), expected);
    const slower = structuredClone(fixture);
    slower.sourceEpisodes[1].nativeLastSourceFrameMs += 500;
    slower.sourceEpisodes[1].lastSourceFrameElapsedMs += 500;
    slower.sourceEpisodes[1].nativeSourceEndMs += 500;
    assert.deepEqual(verifyQualificationSources(trial, slower), expected);
    for (const mutate of [f => f.sourceEpisodes[1].emittedFrames--,
      f => { f.sourceEpisodes[0].nativeLastSourceFrameMs = f.sourceEpisodes[0].nativeSourceStartMs; f.sourceEpisodes[0].lastSourceFrameElapsedMs = 0; },
      f => f.sourceEpisodes[0].pacingViolations = 1,
      f => delete f.sourceEpisodes[0].pacingIntervalsChecked,
      f => f.sourceEpisodes[0].lastSourceFrameElapsedMs = Infinity,
      f => f.sourceEpisodes[0].nativeLastSourceFrameMs = f.sourceEpisodes[0].nativeSourceEndMs, f => f.sourceEpisodes[0].nativeSourceEndMs = 101,
      f => f.sourceEpisodes[1].sourceGateReady = { status: 'Recording' }, f => f.activeCaptures++, f => f.observationOverflow = true]) {
      const bad = structuredClone(fixture); mutate(bad); assert.throws(() => verifyQualificationSources(trial, bad));
    }
    if (expected.gateInitialSource) {
      const absent = structuredClone(fixture); absent.sourceEpisodes[0].sourceGateReady.serverReady = false;
      assert.throws(() => verifyQualificationSources(trial, absent));
      const bad = structuredClone(fixture); bad.sourceEpisodes[0].sourceGateReady.status = 'captureReady';
      assert.throws(() => verifyQualificationSources(trial, bad));
    }
    if (expected.baseline) {
      const bad = structuredClone(fixture); bad.sourceEpisodes[1].gapFrames = 0;
      assert.throws(() => verifyQualificationSources(trial, bad));
    }
  }
});

test('seal-close TEST command invokes the product native close Stop path with isolated guards and debug registration', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = await readFile(new URL('../../src-tauri/src/presentation/native_e2e.rs', import.meta.url), 'utf8');
  const lib = await readFile(new URL('../../src-tauri/src/lib.rs', import.meta.url), 'utf8');
  const cases = await readFile(new URL('../../src/e2e/nativeContinuationCases.ts', import.meta.url), 'utf8');
  const command = native.slice(native.indexOf('pub fn native_e2e_close_recording'), native.indexOf('pub struct FixtureConfig'));
  assert.match(command, /RESULT_PATH.get\(\).is_none\(\)/);
  assert.match(command, /mini_ux_mode\(\)/);
  assert.match(command, /continuation_mode\(\)/);
  assert.match(command, /Some\("seal-close"\)/);
  assert.match(command, /super::commands::stop_recording_on_native_close\(&app_handle\)/);
  assert.match(lib, /#\[cfg\(all\(debug_assertions, feature = "native-window-e2e"\)\)\]\s*presentation::native_e2e::native_e2e_close_recording/);
  assert.match(lib, /commands::stop_recording_on_native_close\(window_clone.app_handle\(\)\)/);
  assert.match(cases, /selected === 'seal-close'\) await invoke\('native_e2e_close_recording'\)/);
  assert.doesNotMatch(cases, /getCurrentWindow|hide_recording_window_if_current/);
});

test('warm canary Stop samples transport and dispatches without a JS or async readiness gap', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = await readFile(new URL('../../src-tauri/src/presentation/native_e2e.rs', import.meta.url), 'utf8');
  const lib = await readFile(new URL('../../src-tauri/src/lib.rs', import.meta.url), 'utf8');
  const start = native.indexOf('pub async fn native_e2e_stop_with_transport_boundary');
  const command = native.slice(start, native.indexOf('\nfn dispatch_hotkey', start));
  assert.ok(start > 0);
  assert.match(command, /trial\["kind"\] != "warm-provider-canary"/);
  const observation = command.indexOf('.native_e2e_transport_observation()');
  const press = command.indexOf('dispatch_hotkey(&app, true)');
  assert.ok(observation > 0 && press > observation);
  assert.doesNotMatch(command.slice(observation, press), /\.await[\s\S]*\.await/);
  assert.match(lib, /#\[cfg\(all\(debug_assertions, feature = "native-window-e2e"\)\)\]\s*presentation::native_e2e::native_e2e_stop_with_transport_boundary/);
});

test('normal baseline/cold reject continuation controls', async () => {
  const { verifyQualificationConnections } = await import('./nativeContinuation.mjs');
  for (const trial of liveTrials.filter(t => !t.continuation)) {
    for (const type of ['pause_accepted', 'pause_rejected', 'continue_result', 'pause_restore_result']) {
      assert.throws(() => verifyQualificationConnections(trial, [{ event: 'backend_control', type }]), /Unexpected continuation control/);
    }
  }
});

 test('terminal verifier rejects reviewer missing/duplicate/unexpected/incomplete terminals and collapsed cold owners', async () => {
  const { verifyQualificationTerminals, qualificationExpectations } = await import('./nativeContinuation.mjs');
  for (const trial of liveTrials.filter(row => row.kind !== 'warm-provider-canary')) {
    const count = qualificationExpectations(trial).backendConnections;
    const episodes = [{ logicalRunId: 41 }, { logicalRunId: count === 1 ? 41 : 42 }];
    const terminals = [...new Set(episodes.map(e => e.logicalRunId))].map(sessionId => ({ sessionId, complete: true }));
    verifyQualificationTerminals(trial, episodes, terminals);
    for (const bad of [[], [...terminals, terminals[0]], [{ sessionId: 999, complete: true }], terminals.map(t => ({ ...t, complete: false }))])
      assert.throws(() => verifyQualificationTerminals(trial, episodes, bad));
    if (count === 2) assert.throws(() => verifyQualificationTerminals(trial, [{ logicalRunId: 41 }, { logicalRunId: 41 }], terminals));
  }
});
test('normal Stop closure must precede explicit live-process collector boundary, never finish/exit cleanup', async () => {
  const { verifyQualificationConnections } = await import('./nativeContinuation.mjs');
  const trial = liveTrials.find(t => t.id === 'warm-baseline-1');
  const open = { event: 'fault_proxy_connected', connectionId: 1 };
  const close = { event: 'fault_proxy_close', connectionId: 1, direction: 'upstream', code: 1000 };
  const boundary = { event: 'qualification_pre_teardown', atMs: 10, clock: 'runner-performance-now', nativeProcessAlive: true };
  verifyQualificationConnections(trial, [open, close, boundary]);
  assert.throws(() => verifyQualificationConnections(trial, [open, boundary, close]));
  assert.throws(() => verifyQualificationConnections(trial, [open, close]));
  assert.throws(() => verifyQualificationConnections(trial, [open, close, { ...boundary, nativeProcessAlive: false }]));
});

test('live route verifier rejects A-only false positives and distinguishes sealed cancellation', async () => {
  const { verifyQualificationRoute } = await import('./nativeContinuation.mjs');
  const connected = { event: 'fault_proxy_connected', connectionId: 1 };
  const audio = (bytes = 640) => ({ event: 'client_binary', connectionId: 1, bytes });
  const pause = { event: 'backend_control', type: 'pause_accepted', decision: 'accepted' };
  const continued = { event: 'backend_control', type: 'continue_result', decision: 'accepted', eligible_now: true };
  const restore = { event: 'backend_control', type: 'pause_restore_result', decision: 'accepted' };
  const continuedTrial = liveTrials.find(t => t.id === 'warm-continue-1');
  const cancelledTrial = liveTrials.find(t => t.id === 'short-tail');
  verifyQualificationRoute(continuedTrial, [connected, audio(), pause, continued, audio()]);
  verifyQualificationRoute(continuedTrial, [connected, audio(9600), pause, continued, audio(9600)]);
  for (const bytes of [1, 9601, 9602]) {
    assert.throws(() => verifyQualificationRoute(continuedTrial, [connected, audio(bytes), pause, continued, audio()]), /Invalid client audio/);
  }
  assert.throws(() => verifyQualificationRoute(continuedTrial, [connected, audio(), pause, continued]), /requires a B write/);
  assert.throws(() => verifyQualificationRoute(continuedTrial, [connected, audio(), pause, audio(), continued, audio()]), /preceded Continue/);
  assert.throws(() => verifyQualificationRoute(continuedTrial, [connected, audio(), pause, continued, restore, audio()]), /no Restore/);
  verifyQualificationRoute(cancelledTrial, [connected, audio(), pause, continued, restore]);
  assert.throws(() => verifyQualificationRoute(cancelledTrial, [connected, audio(), pause, audio(), continued, restore]), /preceded Continue/);
  assert.throws(() => verifyQualificationRoute(cancelledTrial, [connected, audio(), pause, continued, audio(), restore]), /without a B write/);
  assert.throws(() => verifyQualificationRoute(cancelledTrial, [connected, audio(), continued, restore]), /ordered Pause\/Continue/);
});

test('cold route verifier requires client audio on both connection owners', async () => {
  const { verifyQualificationRoute } = await import('./nativeContinuation.mjs');
  const trial = liveTrials.find(t => t.id === 'cold-4000');
  const connected = connectionId => ({ event: 'fault_proxy_connected', connectionId });
  const audio = connectionId => ({ event: 'client_binary', connectionId, bytes: 640 });
  verifyQualificationRoute(trial, [connected(1), audio(1), connected(2), audio(2)]);
  assert.throws(() => verifyQualificationRoute(trial, [connected(1), audio(1), connected(2)]), /has no client audio/);
  assert.throws(() => verifyQualificationRoute(trial, [connected(1), audio(2), connected(2), audio(2)]), /has no client audio/);
});

// Execute the actual TypeScript orchestration against an ordered IPC evidence tape.
// This tests orchestration/rejection boundaries; it is deliberately not native proof.
async function eventOrchestration(selected, mutate = () => {}) {
  const { readFile } = await import('node:fs/promises');
  const { stripTypeScriptTypes } = await import('node:module');
  const { compileFunction, createContext } = await import('node:vm');
  let clock = 0; let report; const calls = [];
  const event = (kind, generation, atMs) => ({ kind, generation, atMs });
  const trace = (sequence, source, phase = 'IntentRejected', reason = 'None') => ({ sequence, source, phase, reason, gesture: 1, desiredAfter: 'Off' });
  const snapshot = (starts, stops, chunks, extra = {}) => ({ nativeClockMs: clock, preparedCaptureTokenCount: 0,
    captureEpisode: starts > stops ? { runId: starts, generation: starts } : null,
    coordinatorTrace: [{ ...trace(1, 'Some(HoldHotkey)', 'IntentApplied'), gesture: starts, desiredAfter: 'On { revision: 1 }' }],
    fixture: { activeCaptures: starts - stops, activeProviders: 1, captureStarts: starts, captureStops: stops,
      maxActiveCaptures: 1, providerStarts: 1, providerResumes: 0, providerAudioChunks: chunks, markerViolations: [], observationOverflow: false,
      captureEvents: [], controlResults: [], providerMarkers: Array.from({ length: starts }, (_, i) => ({ captureGeneration: i + 1, firstSequence: 1, lastSequence: chunks, count: chunks, captureRunId: i + 1, captureFenceGeneration: i + 1, providerSessionId: i + 1 })) }, ...extra });
  const a = snapshot(1, 0, 1); let tape; let final;
  if (selected === 'E04') {
    const held = snapshot(1, 0, 2); held.fixture.captureEvents = [event('stop-entered', 1, 10)];
    const b = snapshot(2, 1, 4); b.fixture.captureEvents = [...held.fixture.captureEvents,
      event('stop-released', 1, 160), event('capture-off', 1, 161), event('capture-joined', 1, 161), event('capture-start', 2, 161), event('first-pcm', 2, 200)];
    const off = snapshot(2, 2, 5); off.fixture.captureEvents = b.fixture.captureEvents;
    final = structuredClone(off); final.fixture.activeProviders = 0;
    tape = [a, held, held, b, off, final];
  } else if (selected === 'E41') {
    const offA = snapshot(1, 1, 2); const b = snapshot(2, 1, 3);
    const key = snapshot(2, 1, 5, { coordinatorTrace: [...b.coordinatorTrace, trace(2, 'Some(HoldHotkey)')] });
    const vad = snapshot(2, 1, 7, { coordinatorTrace: [...b.coordinatorTrace, trace(2, 'Some(HoldHotkey)'), trace(3, 'Some(Vad)')] });
    const offB = snapshot(2, 2, 8); const c = snapshot(3, 2, 9); const offC = snapshot(3, 3, 10);
    offB.coordinatorTrace = [{ ...trace(4, 'Some(HoldHotkey)', 'IntentApplied'), gesture: 2 }, { ...trace(4, 'None', 'CaptureStopEnqueued'), runId: 2 }];
    offC.coordinatorTrace = [trace(5, 'Some(Vad)', 'IntentApplied'), { ...trace(5, 'None', 'CaptureStopEnqueued'), runId: 3 }];
    for (const [stopped, runId, sequence] of [[offB, 2, 4], [offC, 3, 5]]) {
      stopped.captureEpisode = { runId, generation: runId };
      stopped.coordinatorTrace.push({ sequence: sequence + 1, phase: 'CaptureStopped', runId,
        desiredAfter: 'Off', captureAfter: 'Idle' });
      stopped.fixture.captureEvents = ['capture-off', 'capture-joined'].map(kind => event(kind, runId, 0));
    }
    final = structuredClone(offC); final.fixture.activeProviders = 0;
    tape = [a, offA, b, b, key, key, vad, vad, offB, c, c, offC, offC, final];
  } else {
    const offA = snapshot(1, 1, 2); offA.fixture.activeProviders = 0;
    offA.coordinatorTrace = [trace(2, 'Some(System)', 'ForceOff', 'Some(SystemSleep)')];
    const b = snapshot(2, 1, 3); b.fixture.providerStarts = 2;
    const offB = snapshot(2, 2, 4); offB.fixture.providerStarts = 2;
    final = structuredClone(offB); final.fixture.activeProviders = 0;
    final.coordinatorTrace = [trace(2, 'Some(System)', 'IntentAccepted', 'Some(SystemSleep)')];
    const ev = (kind, id, observation, result) => ({kind, watcherFinished: kind === 'watcher' ? true : null, sample: observation === 'NotRead' ? null : (id - 1) * 2 + (observation === 'Up' ? 1 : 0), handle:{gesture:id,watcher:id},observation,result});
    const physical = {source:'fake',realKeyboardReads:0,overflow:false,
      observations: ['Down','Up','Down','Up'].map(observation => ({key:7,modifiers:3,source:'fake',observation,
        downKeys:observation === 'Down' ? [7,55,56] : []})),
      events:[ev('pressed',1,'Down','Accepted'),ev('pressed',1,'Down','Duplicate'),ev('watcher',1,'Up','Rearmed'),
        ev('pressed',2,'Down','Accepted'),ev('watcher',1,'NotRead','Stale'),ev('released',2,'Down','Stale'),ev('watcher',2,'Up','HoldEnded')]};
    const rearmed = structuredClone(offA); rearmed.physicalKeyboard = physical;
    const held = snapshot(2,1,5); held.fixture.providerStarts = 2;
    offB.captureEpisode = {runId:2,generation:2};
    offB.coordinatorTrace = [{...trace(4,'Some(HoldHotkey)','IntentApplied'),gesture:2},
      {...trace(4,'None','CaptureStopEnqueued'),runId:2},
      {sequence:5,phase:'CaptureStopped',runId:2,desiredAfter:'Off',captureAfter:'Idle'}];
    offB.fixture.captureEvents = ['capture-off','capture-joined'].map(kind => event(kind,2,0));
    offB.physicalKeyboard = physical;
    final = structuredClone(offB); final.fixture.activeProviders = 0;
    final.coordinatorTrace.push(trace(6,'Some(System)','ForceOff','Some(SystemSleep)'));
    tape = [a, offA, offA, offA, rearmed, b, held, held, offB, offB, final];
  }
  // Keep checkpoints independent, like deserialized IPC values.
  tape = structuredClone(tape); mutate(tape);
  let index = 0;
  const invoke = async (command, args) => {
    calls.push([command, args]);
    if (command === 'native_e2e_state') return structuredClone(tape[Math.min(index++, tape.length - 1)]);
    if (command === 'native_e2e_delay') { assert.ok(args.durationMs > 0 && args.durationMs <= 150); clock += args.durationMs; }
    if (command === 'native_e2e_finish') report = args.report;
  };
  const context = createContext({ performance: { now: () => clock } });
  const source = await readFile(new URL('../../src/e2e/nativeContinuationCases.ts', import.meta.url), 'utf8');
  const stripped = stripTypeScriptTypes(source);
  // Only module linkage is replaced; every function body and assertion is the real TS.
  const imports = stripped.match(/^import .*;$/gm);
  assert.deepEqual(imports, [
    "import { invoke } from '@tauri-apps/api/core';",
    "import { listen } from '@tauri-apps/api/event';",
    "import { useAppConfigStore } from '@/stores/appConfig';",
  ]);
  const executable = stripped.replace(/^import .*;\n/gm, '').replace(/^export /gm, '');
  const load = compileFunction(executable + '\nreturn runNativeContinuationCase;',
    ['invoke', 'listen', 'useAppConfigStore'], { parsingContext: context });
  const actual = load(invoke, async () => () => {}, () => ({ startSync: async () => {}, refresh: async () => {} }));
  await actual({}, selected);
  return { report: JSON.parse(JSON.stringify(report)), calls, envelope: {
    marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', passed: true, report: JSON.parse(JSON.stringify(report)), fixture: tape[tape.length - 1].fixture } };
}

for (const selected of ['E04', 'E41', 'E42']) {
  test(`${selected} actual TS uses bounded native IPC and runner checks evidence`, async () => {
    const { report, calls, envelope } = await eventOrchestration(selected);
    assert.equal(report.passed, true, JSON.stringify(report.errors));
    assert.equal(validateResult(envelope).case, selected);
    const actions = calls.filter(([name]) => name === 'native_e2e_hotkey').map(([, args]) => args.action);
    if (selected === 'E04') {
      assert.deepEqual(actions, ['press', 'release', 'press', 'release-capture-stop', 'release', 'sleep', 'wake']);
      const releaseIndex = calls.findIndex(([name, args]) => name === 'native_e2e_hotkey' && args.action === 'release');
      assert.equal(calls[releaseIndex + 1][1].action, 'press');
    } else if (selected === 'E41') {
      assert.deepEqual(actions, ['press', 'save-capture-events', 'release', 'press', 'stale-key-release', 'stale-vad', 'release', 'press', 'current-vad', 'release', 'sleep', 'wake']);
    } else {
      assert.deepEqual(JSON.parse(JSON.stringify(calls.filter(([name]) => name === 'native_e2e_hotkey').map(([,args]) => args.physical ?? args.action))), [
        {kind:'set',available:true,downKeys:[7,55,56]}, {kind:'callback',state:'pressed'}, 'sleep','wake',
        {kind:'callback',state:'pressed'}, {kind:'set',available:true,downKeys:[]},
        {kind:'set',available:true,downKeys:[7,55,56]}, {kind:'callback',state:'pressed'},
        {kind:'watcher-step',savedHandle:'A'}, {kind:'callback',state:'released'}, {kind:'callback',state:'released'},
        {kind:'set',available:true,downKeys:[]}, 'sleep','wake']);
    }
    for (const mutate of [e => e.fixture.maxActiveCaptures = 2,
      e => e.report.final.preparedCaptureTokenCount = 1,
      e => e.report.checkpoints.splice(0, 1), e => e.fixture.captureStops--,
      e => e.fixture.captureEvents.push({ kind: 'stop-timeout' })]) {
      const bad = structuredClone(envelope); mutate(bad); assert.throws(() => validateResult(bad));
    }
  });
}

test('actual E04 TS rejects early B loss, overlap, timeout and release latency boundaries', async () => {
  for (const latency of [-1, 0, 250, 251]) {
    const { report } = await eventOrchestration('E04', tape => {
      tape[3].fixture.captureEvents.find(e => e.kind === 'first-pcm').atMs = 161 + latency;
    });
    assert.equal(report.passed, latency >= 0 && latency <= 250, `latency ${latency}`);
  }
  for (const mutate of [t => t[3].fixture.providerMarkers[1].firstSequence = 2,
    t => t[5].fixture.maxActiveCaptures = 2,
    t => t[5].fixture.captureEvents.push({ kind: 'stop-timeout' }),
    t => { for (let i = 3; i < t.length; i++) t[i].fixture.captureStarts = 1; }]) {
    assert.equal((await eventOrchestration('E04', mutate)).report.passed, false);
  }
});

test('actual E41 TS requires stale rejection, continued PCM and both current stops', async () => {
  for (const mutate of [t => t[4].coordinatorTrace = [],
    t => t[6].coordinatorTrace = [], t => t[4].captureEpisode.generation++,
    t => t[4].fixture.providerMarkers[1] = structuredClone(t[3].fixture.providerMarkers[1]),
    t => { for (let i = 9; i < t.length; i++) t[i].fixture.activeCaptures = 1; }]) {
    assert.equal((await eventOrchestration('E41', mutate)).report.passed, false);
  }
});

test('actual E42 TS rejects stale Continue, missing sleep dispatch trace and phantom capture', async () => {
  for (const mutate of [t => t[10].fixture.controlResults.push({ operation: 'continue' }),
    t => t[10].coordinatorTrace = [], t => t[1].coordinatorTrace = [],
    t => { for (let i = 2; i < t.length; i++) t[i].fixture.captureStarts = 3; }]) {
    assert.equal((await eventOrchestration('E42', mutate)).report.passed, false);
  }
});

test('bounded fixture hooks dispatch real captured events and lifecycle helpers; E04 gates capture task', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = await readFile(new URL('../../src-tauri/src/presentation/native_e2e.rs', import.meta.url), 'utf8');
  const commands = await readFile(new URL('../../src-tauri/src/presentation/commands.rs', import.meta.url), 'utf8');
  const capture = native.slice(native.indexOf('impl AudioCapture for FixtureCapture'), native.indexOf('pub struct FixtureFactory'));
  const stop = capture.slice(capture.indexOf('async fn stop_capture'));
  assert.ok(stop.indexOf('capture_stop_release.notified()') < stop.indexOf('task.abort()'));
  assert.match(stop, /Duration::from_secs\(3\)/);
  assert.ok(stop.indexOf('task.await') < stop.indexOf('"capture-joined"'));
  assert.match(native.slice(native.indexOf('impl Drop for CaptureLease'), native.indexOf('impl AudioCapture for FixtureCapture')), /"kind": "capture-off", "generation": self.1, "atMs": released_at/);
  const hotkey = native.slice(native.indexOf('pub async fn native_e2e_hotkey'), native.indexOf('fn dispatch_hotkey'));
  assert.match(hotkey, /!event_case\("E41"\)/);
  assert.match(hotkey, /current_capture_stop\(IntentSource::Vad\)/);
  assert.match(hotkey, /RecordingIntent::stop\(\s*IntentSource::HoldHotkey,\s*Some\(gesture\),?\s*\)/);
  assert.match(hotkey, /dispatch_recording_coordinator_event\(app, event\)/);
  assert.doesNotMatch(hotkey, /reduce\(|reduce_at\(/);
  assert.match(hotkey, /force_off_recording_for_system_sleep\(app\)/);
  assert.match(hotkey, /reset_recording_gestures_after_system_wake\(&app\)/);
  const sleep = commands.slice(commands.indexOf('pub(super) fn force_off_recording_for_system_sleep'), commands.indexOf('fn execute_recording_coordinator_effect'));
  assert.match(sleep, /submit_normalized_recording_gesture\(app_handle.clone\(\), intent\);[\s\S]*drop\(gestures\);[\s\S]*submitted\(\)/);
  assert.match(sleep, /\.force_off\(super::recording_hotkey_gestures::ForceOffReason::Sleep\)/);
});

test('runner independently rejects corrupted per-case native checkpoint evidence', async () => {
  const mutations = {
    E04: [e => e.report.checkpoints.find(p => p.label === 'B recording').state.fixture.providerMarkers[1].firstSequence = 2,
      e => e.report.checkpoints.find(p => p.label === 'B queued behind actual A').state.fixture.captureStarts = 2,
      e => e.report.releaseToFirstPcmMs++,
      e => e.report.checkpoints.find(p => p.label === 'B recording').state.fixture.captureEvents = []],
    E41: [e => e.report.checkpoints.find(p => p.label === 'stale-vad').state.coordinatorTrace = [],
      e => e.report.checkpoints.find(p => p.label === 'stale-key-release').state.captureEpisode.generation++,
      e => e.report.checkpoints.find(p => p.label === 'current VAD stops C').state.fixture.activeCaptures = 1],
    E42: [e => e.fixture.controlResults.push({ operation: 'continue' }),
      e => e.report.checkpoints.find(p => p.label === 'sleep resources off').state.coordinatorTrace = [],
      e => e.report.checkpoints.find(p => p.label === 'wake without phantom').state.fixture.activeCaptures = 1],
  };
  for (const [selected, edits] of Object.entries(mutations)) {
    const { envelope } = await eventOrchestration(selected);
    for (const edit of edits) {
      const bad = structuredClone(envelope); edit(bad); assert.throws(() => validateResult(bad));
    }
  }
});

// Each critic counterexample runs through both the actual TS and the independent runner.
// These are assertion tests with synthetic IPC evidence, never native execution evidence.
test('critic counterexamples fail actual orchestration and independently fail runner validation', async () => {
  const cases = [
    ['E04', 3, 'B recording', s => s.fixture.providerMarkers[1].firstSequence = 2],
    ['E04', 3, 'B recording', s => s.fixture.captureEvents.find(e => e.kind === 'capture-joined').atMs = 201],
    ['E04', 3, 'B recording', s => s.fixture.captureEvents.find(e => e.kind === 'capture-off').atMs = -100],
    ...[[8, 'current key release stops B'], [11, 'current VAD stops C']].flatMap(([index, label]) => [
      ['E41', index, label, s => s.coordinatorTrace.forEach(t => t.sequence = 1)],
      ['E41', index, label, s => s.coordinatorTrace.find(t => t.phase === 'CaptureStopEnqueued').runId = 99],
      ['E41', index, label, s => s.coordinatorTrace.find(t => t.phase === 'IntentApplied').desiredAfter = 'On'],
    ]),
    ['E41', 8, 'current key release stops B', s => s.coordinatorTrace[0].gesture = 99],
    ...[[4, 'stale-key-release', 3], [6, 'stale-vad', 5]].flatMap(([index, label, count]) => [
      ['E41', index, label, s => { s.fixture.providerMarkers[1].count = count; s.fixture.providerMarkers[1].lastSequence = count; }],
      ['E41', index, label, s => s.fixture.providerMarkers[1].providerSessionId = 99],
      ['E41', index, label, s => s.fixture.providerMarkers[1].captureRunId = 99],
      ['E41', index, label, s => s.fixture.providerMarkers[1].captureFenceGeneration = 99],
      ['E41', index, label, s => s.fixture.providerMarkers[1].lastSequence = count],
    ]),
    ...[[0, 'A recording'], [1, 'sleep resources off'], [2, 'wake without phantom'], [3, 'wake remains off without key-up'], [5, 'next gesture recording'], [10, 'terminal cleanup']].map(([index, label]) =>
      ['E42', index, label, s => s.fixture.providerResumes = 1]),
    ['E42', 0, 'A recording', s => s.fixture.providerStarts = 2],
    ['E42', 5, 'next gesture recording', s => s.fixture.providerStarts = 1],
    ['E42', 5, 'next gesture recording', s => s.fixture.providerMarkers[1].providerSessionId = 1],
  ];
  for (const [selected, index, label, edit] of cases) {
    const rejected = await eventOrchestration(selected, tape => {
      edit(tape[index]);
      // A delayed valid completion is now intentionally accepted by bounded polling.
      if (selected === 'E41' && index === 11) { edit(tape[12]); edit(tape[13]); }
    });
    assert.equal(rejected.report.passed, false, `${selected}: ${label}: ${edit}`);
    const { envelope } = await eventOrchestration(selected);
    edit(envelope.report.checkpoints.find(p => p.label === label).state);
    assert.throws(() => validateResult(envelope), `${selected}: ${label}: ${edit}`);
  }
});

function afterWriteEnvelope(selected) {
  const marker = (generation, count) => ({ captureGeneration: generation, firstSequence: 1,
    lastSequence: count, count, captureRunId: generation, captureFenceGeneration: generation, providerSessionId: 7 });
  const control = operation => ({ operation, delivered: true, result: { decision: 'accepted', pause_epoch: 1 } });
  const source = selected === 'after-write-hold' ? 'Some(HoldHotkey)' : selected === 'after-write-toggle' ? 'Some(CarbonHotkey)' : 'Some(Frontend)';
  const a = { nativeClockMs: 0, logicalProviderRunId: 9, captureEpisode: { runId: 1, generation: 1 }, preparedCaptureTokenCount: 0,
    coordinatorTrace: [], fixture: { providerMarkers: [marker(1, 2)] } };
  const b = { nativeClockMs: 0, logicalProviderRunId: 9, captureEpisode: { runId: 2, generation: 2 }, preparedCaptureTokenCount: 0,
    visible: true, windowEpoch: 2, coordinatorShownEpoch: 2,
    coordinatorCapture: { recording: true, runId: 2 },
    coordinatorTrace: [{ sequence: 3, phase: 'IntentApplied', source: 'Some(HoldHotkey)', gesture: 2, desiredAfter: 'On' }],
    fixture: { captureStarts: 2, captureStops: 1, activeCaptures: 1, activeProviders: 1,
      maxActiveCaptures: 1, maxActiveProviders: 1, providerStarts: 1, providerResumes: 0,
      observationOverflow: false, markerViolations: [], captureEvents: [],
      controlResults: [control('pause'), control('continue')],
      firstBWrites: [{ captureGeneration: 2, logicalRunId: 9, pauseEpoch: 1 }],
      providerMarkers: [marker(1, 3), marker(2, 2)] } };
  const final = structuredClone(b);
  final.afterWriteService = { owner: 9, status: 'Idle', logicalProviderRunId: 0,
    captureEpisode: structuredClone(b.captureEpisode), pausedContinuation: null, coordinatorIdle: true,
    pendingStart: false, processingJobs: 0, continuationPending: false,
    terminal: [{ runId: 9, sequence: 6, outcome: 'Some(FinalizeCommitted)', error: null }],
    completedReport: { run_id: 9, provider_release: 'released', error: null, shared_failure: false,
      continuation_not_started: null, audio: { accepted_bytes: 4480, read_bytes: 4480, submitted_bytes: 4480,
        acknowledged_bytes: null, unacknowledged_bytes: null, remaining_bytes: 0, unknown_bytes: 0, reason: 'drained' },
      provider: { reason: 'drained', tail_evidence: 'segment_observed', provider_release: 'released',
        last_delivery_seq: 1, stable_snapshot: 'Native fixture session 7' } } };
  final.coordinatorTrace.push({ sequence: 4, phase: 'IntentApplied', source, gesture: 2, desiredAfter: 'Off' },
    { sequence: 4, phase: 'CaptureStopEnqueued', runId: 2 },
    { sequence: 5, phase: 'CaptureStopped', runId: 2, desiredAfter: 'Off', captureAfter: 'Idle' });
  Object.assign(final.fixture, { activeCaptures: 0, activeProviders: 0, captureStops: 2, finals: 1,
    fullPcm: [1, 2].flatMap(captureGeneration => ['capture', 'provider'].map(seam => ({ seam, captureGeneration,
      chunks: captureGeneration === 1 ? 3 : 4, samples: (captureGeneration === 1 ? 3 : 4) * 320, valid: true }))),
    providerMarkers: [marker(1, 3), marker(2, 4)], captureMarkers: [marker(1, 3), marker(2, 4)],
    captureEvents: [1, 2].flatMap(generation => ['capture-start', 'capture-off', 'capture-joined'].map(kind => ({ generation, kind, atMs: 0 }))) });
  return { marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', passed: true, fixture: final.fixture,
    afterWriteServiceBefore: structuredClone(final.afterWriteService), afterWriteServiceAfter: structuredClone(final.afterWriteService),
    report: { mode: 'after-write-case', case: selected, passed: true, errors: [], a, b, final } };
}
async function actualAfterWrite(selected, mutate = () => {}, intercept = () => undefined) {
  const { readFile } = await import('node:fs/promises');
  const { stripTypeScriptTypes } = await import('node:module');
  const { compileFunction } = await import('node:vm');
  const envelope = afterWriteEnvelope(selected); const { a, b, final } = envelope.report;
  const paused = structuredClone(b); paused.fixture.activeCaptures = 0;
  const pending = structuredClone(b); pending.fixture.firstBWrites = []; pending.fixture.providerMarkers.pop();
  const tape = [a, paused, pending, b, final]; mutate(tape);
  let index = 0, clock = 0, report, handoff; const calls = [];
  const invoke = async (name, args) => {
    calls.push([name, args, index]);
    if (name === 'native_e2e_finish') report = args.report;
    if (name === 'native_e2e_terminal_handoff') handoff = structuredClone(args);
    const override = intercept(name, args, index);
    if (override !== undefined) return override;
    if (name === 'native_e2e_state') return structuredClone(tape[Math.min(index++, tape.length - 1)]);
    if (name === 'native_e2e_delay') clock += args.durationMs;
    if (name === 'native_e2e_finish') report = args.report;
  };
  const source = stripTypeScriptTypes(await readFile(new URL('../../src/e2e/nativeContinuationCases.ts', import.meta.url), 'utf8'));
  const actual = compileFunction(source.replace(/^import .*;\n/gm, '').replace(/^export /gm, '') + '\nreturn { runNativeContinuationCase, verifyAfterWrite };',
    ['invoke', 'listen', 'useAppConfigStore', 'performance'])(invoke, async () => () => {},
    () => ({ startSync: async () => {}, refresh: async () => {} }), { now: () => clock });
  await actual.runNativeContinuationCase({}, selected);
  // Post-handoff verification is invoked explicitly by this unit test. This is
  // the actual exported TS validator, NOT an emulation of native orchestration.
  if (handoff) {
    report = { ...handoff.report, final: structuredClone(tape.at(-1)), errors: [...handoff.report.errors] };
    try {
      actual.verifyAfterWrite(report.a, report.b, report.final, selected);
      report.passed = true;
    } catch (error) { report.errors.push(String(error)); report.passed = false; }
  }
  return { report, handoff, calls, envelope: { ...envelope, report } };
}
for (const selected of ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle']) {
  test(`${selected} actual TS hands off only after native B write; both validators accept the complete sample`, async () => {
    assert.equal(parseArguments(['--continuation-case', selected]).continuationCase, selected);
    const { report, calls, envelope } = await actualAfterWrite(selected);
    assert.equal(report.passed, true, JSON.stringify(report.errors));
    validateResult(envelope);
    const last = calls.filter(([name]) => name === 'native_e2e_terminal_handoff').at(-1);
    assert.equal(last[2], 4, 'stop must follow the actual B writer checkpoint');
    assert.equal(last[0], 'native_e2e_terminal_handoff');
    assert.equal(calls.some(([name]) => ['native_e2e_close_recording', 'stop_recording', 'native_e2e_finish'].includes(name)), false);
  });
  test(`${selected} independent runner rejects corrupt ownership, source loss and historical stop`, () => {
    for (const corrupt of [
      e => e.report.b.logicalProviderRunId++, e => e.report.b.captureEpisode.generation = 1,
      e => e.report.b.fixture.firstBWrites = [], e => e.report.b.fixture.firstBWrites.push({}),
      e => e.report.b.fixture.firstBWrites[0].logicalRunId++,
      e => e.report.b.fixture.firstBWrites[0].pauseEpoch++,
      e => { const r = e.report.b.fixture.controlResults.find(c => c.operation === 'continue').result; r.pauseEpoch = r.pause_epoch; delete r.pause_epoch; },
      e => e.report.final.fixture.captureEvents.pop(),
      e => e.report.b.fixture.providerMarkers[1].providerSessionId++,
      e => e.report.final.fixture.captureMarkers[1].count++,
      e => e.report.final.fixture.providerMarkers[1].captureFenceGeneration++,
      e => e.report.final.fixture.providerMarkers[1].firstSequence++,
      e => e.report.final.fixture.providerMarkers[1].lastSequence++,
      e => e.report.final.fixture.controlResults.push({ operation: 'restore' }),
      e => e.report.final.fixture.captureStops--, e => e.report.final.fixture.maxActiveCaptures++,
      e => e.report.final.fixture.providerStarts++, e => e.report.final.fixture.activeProviders++,
      e => e.report.final.preparedCaptureTokenCount++, e => e.report.errors.push('transcription error'),
      e => e.report.final.coordinatorTrace[1].sequence = 1,
      e => e.report.final.coordinatorTrace[2].runId = 1,
      e => e.report.final.coordinatorTrace[1].source = 'Some(Vad)',
    ]) { const e = afterWriteEnvelope(selected); corrupt(e); assert.throws(() => validateResult(e)); }
  });
  test(`${selected} exported TS verifier rejects PCM loss and stale stop`, async () => {
    for (const mutate of [t => t[4].fixture.captureMarkers[1].count++,
      t => t[4].coordinatorTrace[2].runId = 1,
      t => t[4].fixture.controlResults.push({ operation: 'restore' })]) {
      const { report } = await actualAfterWrite(selected, mutate);
      assert.equal(report.passed, false);
    }
  });
}


test('after-write close invokes the actual close helper and all four opt-ins expose native event observations', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = await readFile(new URL('../../src-tauri/src/presentation/native_e2e.rs', import.meta.url), 'utf8');
  const close = native.slice(native.indexOf('pub fn native_e2e_close_recording'), native.indexOf('pub struct FixtureConfig'));
  assert.match(close, /Some\("seal-close"\) \| Some\("after-write-close"\)/);
  assert.match(close, /super::commands::stop_recording_on_native_close\(&app_handle\)/);
  for (const selected of ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle']) {
    assert.ok(native.slice(native.indexOf('fn capture_event'), native.indexOf('fn continuation_mode')).includes('"' + selected + '"'));
    assert.ok(native.slice(native.indexOf('result["preparedCaptureTokenCount"]')).includes('"' + selected + '"'));
  }
});

// Use the exact failed native report, without turning its failed envelope into a
// passing qualification. Both actual implementations independently assess release.
test('E41 exact native release retains sealed run2/fence3 and requires physical completion', async () => {
  const { readFile } = await import('node:fs/promises');
  const { stripTypeScriptTypes } = await import('node:module');
  const { compileFunction } = await import('node:vm');
  const evidence = JSON.parse(await readFile(new URL('./fixtures/e41-native-release-failure.json', import.meta.url), 'utf8'));
  const points = evidence.report.checkpoints;
  const before = points.find(p => p.label === 'before current key release').state;
  const after = points.find(p => p.label === 'current key release stops B').state;
  assert.deepEqual(after.captureEpisode, { runId: 2, generation: 3 });
  assert.equal(evidence.report.passed, false);
  assert.throws(() => validateResult(evidence));
  for (const file of ['../../src/e2e/nativeContinuationCases.ts', '../run-native-window-e2e.mjs']) {
    let source = await readFile(new URL(file, import.meta.url), 'utf8');
    if (file.endsWith('.ts')) source = stripTypeScriptTypes(source);
    const start = source.indexOf('function markerFor(');
    const stop = source.indexOf('\n}', source.indexOf('function currentStopApplied(')) + 2;
    const verify = compileFunction(source.slice(start, stop) + '\nreturn currentStopApplied;')();
    assert.equal(verify(before, after, 'Some(HoldHotkey)'), true, file);
    const removed = structuredClone(after); removed.captureEpisode = null;
    assert.equal(verify(before, removed, 'Some(HoldHotkey)'), true);
    for (const edit of [
      s => s.captureEpisode.runId++, s => s.captureEpisode.generation++,
      s => s.fixture.activeCaptures = 1, s => s.preparedCaptureTokenCount = 1,
      s => s.coordinatorTrace = s.coordinatorTrace.filter(t => t.phase !== 'CaptureStopped'),
      s => s.coordinatorTrace.find(t => t.phase === 'CaptureStopped' && t.runId === 2).runId = 1,
      s => s.coordinatorTrace.find(t => t.phase === 'CaptureStopped' && t.runId === 2).sequence = 26,
      s => s.coordinatorTrace.find(t => t.phase === 'CaptureStopped' && t.runId === 2).captureAfter = 'Stopping',
      s => s.coordinatorTrace.find(t => t.phase === 'IntentApplied' && t.sequence === 26).gesture = 1,
      s => s.coordinatorTrace.find(t => t.phase === 'CaptureStopEnqueued' && t.sequence === 26).runId = 1,
      s => s.fixture.captureEvents = s.fixture.captureEvents.filter(e => !(e.kind === 'capture-joined' && e.generation === 2)),
      s => s.fixture.captureEvents = s.fixture.captureEvents.filter(e => !(e.kind === 'capture-off' && e.generation === 2)),
      s => s.fixture.captureEvents.find(e => e.kind === 'capture-off' && e.generation === 2).atMs = before.nativeClockMs - 1,
      s => s.fixture.captureEvents.find(e => e.kind === 'capture-joined' && e.generation === 2).atMs = after.nativeClockMs + 1,
    ]) {
      const bad = structuredClone(after); edit(bad);
      assert.equal(verify(before, bad, 'Some(HoldHotkey)'), false, `${file}: ${edit}`);
    }
  }
});

test('E41 polls resources-off until matching coordinator completion and preserves failure during cleanup', async () => {
  const delayed = await eventOrchestration('E41', tape => {
    const pending = structuredClone(tape[8]);
    pending.coordinatorTrace = pending.coordinatorTrace.filter(t => t.phase !== 'CaptureStopped');
    tape.splice(8, 0, pending);
  });
  assert.equal(delayed.report.passed, true);
  validateResult(delayed.envelope);
  const failed = await eventOrchestration('E41', tape => {
    for (const s of tape.slice(8)) s.coordinatorTrace = s.coordinatorTrace.filter(t => t.phase !== 'CaptureStopped');
  });
  assert.equal(failed.report.passed, false);
  assert.match(failed.report.errors[0], /current key release stops B timed out/);
  assert.ok(failed.report.checkpoints.some(p => p.label === 'failure cleanup'));
  assert.equal(failed.report.final, null);
  assert.throws(() => validateResult(failed.envelope));
});

test('E41 retained provider bounds failure cleanup without masking primary assertion', async () => {
  const failed = await eventOrchestration('E41', tape => {
    for (const s of tape.slice(8)) {
      s.coordinatorTrace = s.coordinatorTrace.filter(t => t.phase !== 'CaptureStopped');
      s.fixture.activeProviders = 1;
    }
  });
  assert.equal(failed.report.passed, false);
  assert.match(failed.report.errors[0], /current key release stops B timed out/);
  assert.match(failed.report.errors[1], /Failure cleanup: Error: failure cleanup timed out/);
  assert.equal(failed.calls.at(-1)[0], 'native_e2e_finish');
});

// These mutate native DTO observations, not the live-only normalStopReleased flag.
for (const selected of ['after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle']) {
  test(`${selected} rejects full-payload and pre-finish service counterexamples in both verifiers`, async () => {
    for (const edit of [
      s => s.fixture.fullPcm[3].samples -= 224, // marker header survives truncation
      s => s.fixture.fullPcm[3].valid = false, // same length, corrupted tail or format
      s => s.fixture.fullPcm[0].valid = false, // capture seam must also validate
      s => s.fixture.fullPcm.pop(),
      s => s.afterWriteService.completedReport = null,
      s => s.afterWriteService.completedReport.run_id = 2, // UI B is not logical A
      s => s.afterWriteService.completedReport.provider_release = 'unconfirmed',
      s => s.afterWriteService.completedReport.audio.reason = 'deadline',
      s => s.afterWriteService.completedReport.audio.unknown_bytes = 2,
      s => s.afterWriteService.completedReport.audio.submitted_bytes -= 2,
      s => s.afterWriteService.completedReport.error = 'drain failed',
      s => s.afterWriteService.completedReport.shared_failure = true,
      s => s.afterWriteService.completedReport.provider.reason = 'provider_error',
      s => s.afterWriteService.pausedContinuation = 9,
      s => s.afterWriteService.continuationPending = true,
      s => s.afterWriteService.pendingStart = true,
      s => s.afterWriteService.processingJobs = 1,
      s => s.afterWriteService.status = 'Processing',
      s => s.afterWriteService.terminal[0].runId = 2,
      s => s.afterWriteService.terminal[0].sequence = 3,
      s => s.afterWriteService.terminal[0].outcome = 'Some(FinalizeFailed)',
    ]) {
      const e = afterWriteEnvelope(selected); edit(e.report.final);
      // Keep the envelope equal so rejection must come from the evidence predicate.
      e.afterWriteServiceBefore = structuredClone(e.report.final.afterWriteService);
      e.afterWriteServiceAfter = structuredClone(e.afterWriteServiceBefore);
      assert.throws(() => validateResult(e), String(edit));
      const actual = await actualAfterWrite(selected, tape => edit(tape[4]));
      assert.equal(actual.report.passed, false, String(edit));
    }
    const rescue = afterWriteEnvelope(selected);
    rescue.afterWriteServiceBefore.completedReport = null;
    assert.throws(() => validateResult(rescue), 'finish must not publish missing terminal');
    const repaired = afterWriteEnvelope(selected);
    repaired.afterWriteServiceAfter.continuationPending = true;
    assert.throws(() => validateResult(repaired), 'fixture counters cannot hide service changes');
  });
}

test('E63 JS handoff stops polling while the independent validator rejects callback-only evidence', async () => {
  const actual = await actualAfterWrite('after-write-stop');
  assert.equal(actual.calls.filter(([name]) => name === 'native_e2e_state').length, 4);
  assert.ok(actual.handoff);
  validateResult(actual.envelope);
  const incomplete = structuredClone(actual.envelope);
  incomplete.report.final.afterWriteService.completedReport = null;
  incomplete.report.final.afterWriteService.terminal = [];
  assert.throws(() => validateResult(incomplete));
});

test('E42 both verifiers reject dropped physical Down, Up, rearm, token and real-reader corruption', async () => {
  const mutations = [
    p => p.events = p.events.filter(e => e.result !== 'Rearmed'),
    p => p.events = p.events.filter(e => e.result !== 'HoldEnded'),
    p => p.events = p.events.filter(e => e.result !== 'Accepted'),
    p => p.observations = p.observations.filter(e => e.observation !== 'Down'),
    p => p.observations = p.observations.filter(e => e.observation !== 'Up'),
    p => p.events.at(-1).handle.watcher = 99,
    p => p.events.at(-1).sample = 0,
    p => p.events.at(-1).watcherFinished = false,
    p => { for (const e of p.events) if (e.handle?.gesture === 2) e.handle.gesture = 99; },
    p => p.realKeyboardReads = 1, p => p.overflow = true,
    p => p.observations[0].modifiers = 0,
  ];
  for (const mutate of mutations) {
    const bad = await eventOrchestration('E42', tape => mutate(tape[8].physicalKeyboard));
    assert.equal(bad.report.passed, false);
    const { envelope } = await eventOrchestration('E42');
    mutate(envelope.report.checkpoints.find(p => p.label === 'next gesture release off').state.physicalKeyboard);
    assert.throws(() => validateResult(envelope));
  }
});

test('E42 runner requires rearm evidence at its pre-B checkpoint', async () => {
  const { envelope } = await eventOrchestration('E42');
  delete envelope.report.checkpoints.find(p => p.label === 'A physical Up rearms').state.physicalKeyboard;
  assert.throws(() => validateResult(envelope));
});

for (const outcome of ['lost', 'pending']) {
  test(`E63 ${outcome} handoff acknowledgement never repeats Stop or starts JS cleanup`, async () => {
    const { calls, handoff } = await actualAfterWrite('after-write-stop', () => {}, (name) => {
      if (name === 'native_e2e_terminal_handoff') {
        return outcome === 'lost' ? Promise.reject(new Error('lost acknowledgement')) : new Promise(() => {});
      }
    });
    assert.ok(handoff);
    assert.equal(calls.filter(([name]) => name === 'native_e2e_terminal_handoff').length, 1);
    assert.equal(calls.some(([name]) => ['stop_recording', 'native_e2e_finish'].includes(name)), false);
    const arm = calls.findIndex(([, args]) => args?.diagnosticPhase === 'arm');
    const transfer = calls.findIndex(([name]) => name === 'native_e2e_terminal_handoff');
    assert.ok(arm >= 0 && arm < transfer);
    assert.equal(calls.slice(transfer).filter(([name]) => name === 'native_e2e_state').length, 0);
  });
}
test('E63 original assertion remains primary when finish rejects', async () => {
  let submitted;
  const { report } = await actualAfterWrite('after-write-stop', tape => {
    for (const state of tape) state.fixture.firstBWrites = [];
  }, (name, args) => {
    if (name === 'native_e2e_finish') {
      submitted = structuredClone(args.report);
      return Promise.reject(new Error('cleanup failed'));
    }
  });
  assert.equal(submitted.passed, false);
  assert.equal(report.errors[0], submitted.errors[0]);
  assert.match(report.errors.at(-1), /Finish: Error: cleanup failed/);
  assert.equal(report.passed, false);
});
test('E63 native submission precedes teardown and runner keeps the original independent gates', async () => {
  const { readFile } = await import('node:fs/promises');
  const native = await readFile(new URL('../../src-tauri/src/presentation/native_e2e.rs', import.meta.url), 'utf8');
  const finish = native.slice(native.indexOf('pub async fn native_e2e_finish'), native.indexOf('mod marker_tests'));
  assert.ok(finish.indexOf('diagnostic::submit') < finish.indexOf('synthetic_readback::stop'));
  assert.ok(finish.indexOf('diagnostic::submit') < finish.indexOf('recording_lifecycle_guard.lock'));
  assert.match(finish, /"preFinish":true/);
  assert.match(finish, /diagnostic::healthy\(\)/);
  const runner = await readFile(new URL('../run-native-window-e2e.mjs', import.meta.url), 'utf8');
  assert.match(runner, /options\.readerPreparation \|\| \['E04', 'E41', 'E42', 'after-write-stop', 'after-write-hold', 'after-write-close', 'after-write-toggle'\]\.includes\(options\.continuationCase\) \? 30_000\s*: 480_000/);
  assert.ok(runner.indexOf('E63 native diagnostic failed') < runner.lastIndexOf('validateResult(envelope)'));
  const envelope = afterWriteEnvelope('after-write-stop');
  envelope.afterWriteServiceAfter.pausedContinuation = 9;
  assert.throws(() => validateResult(envelope));
});
