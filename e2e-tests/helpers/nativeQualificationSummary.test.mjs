import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createNativeQualificationBinding, summarizeNativeQualification,
  writeNativeQualificationBinding } from './nativeQualificationSummary.mjs';
import { approvedFixtures } from './nativeContinuation.mjs';

const ids = ['warm-baseline-1', 'warm-baseline-2', 'warm-baseline-3',
  'warm-continue-1', 'warm-continue-2', 'warm-continue-3'];
const labels = ['empirical B prefix', 'empirical B full sentence marker', 'exact cumulative stable text'];
const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';
function rebind(row) {
  const files = { result: 'result-attempt.json', verification: 'qualification-verification.json',
    build: 'native-build.json', provenance: 'backend-provenance.json' };
  row.artifactBytes = Object.fromEntries(Object.keys(files).map(k =>
    [k, Buffer.from(JSON.stringify(k === 'result' ? row.envelope : row[k]))]));
  row.binding = createNativeQualificationBinding(row.trialId, files, row.artifactBytes);
  return row;
}
function attempt(trialId, latency = 100) {
  const baseline = trialId.startsWith('warm-baseline-');
  const present = 100 + latency;
  const bracket = [present - 10, present];
  const text = 'на столе лежит книга за окном растет береза';
  const records = [
    { sequence: 0, text: '', identityValid: true, samples: 1,
      readStartMs: 0, readEndMs: 0, lastReadStartMs: 0, lastReadEndMs: 0, maxSamplingGapMs: 0 },
    { sequence: 1, text: '', identityValid: true, samples: 1,
      readStartMs: present - 10, readEndMs: present - 10,
      lastReadStartMs: present - 10, lastReadEndMs: present - 10, maxSamplingGapMs: 0 },
    { sequence: 2, text, identityValid: true, samples: 1,
      readStartMs: present, readEndMs: present,
      lastReadStartMs: present, lastReadEndMs: present, maxSamplingGapMs: 0 },
  ];
  const probes = [0, 1, 2, 3, 4, 5].map(i => ({
    phase: i < 3 ? 'pre' : 'post', n: i < 3 ? i : 2000 + i,
    w0: (i < 3 ? i : 2000 + i) + 10, w1: (i < 3 ? i : 2000 + i) + 10,
  }));
  // Keep all receipts between the third pre probe and first post probe.
  const report = {
    passed: true, actualPasteVerified: false, trial: { id: trialId, route: baseline ? 'baseline' : 'continued-audio',
      continuation: !baseline, configDelayMs: 0, episodes: ['episode-a.pcm', 'episode-b.pcm'] },
    errors: [], eventOverflow: false, expectedInsertion: text, targetDocument: 'p4-textedit-a.txt',
    clockExchanges: probes, calibration: { offset: [10, 10], error: null },
    episodes: [{ logicalRunId: 1, source: {} },
      { logicalRunId: baseline ? 1 : 2, source: { nativeSourceStartMs: 100, nativeSourceEndMs: 120 } }],
    events: [
      { event: 'transcription:partial', atMs: present + 10, sessionId: baseline ? 1 : 2,
        deliverySeq: 2, stable: false, rawSyntheticText: text,
        syntheticTextValid: true, syntheticTextTruncated: false },
      { event: 'transcription:final', atMs: present + 20, sessionId: baseline ? 1 : 2,
        deliverySeq: 3, stable: true, rawSyntheticText: text,
        syntheticTextValid: true, syntheticTextTruncated: false },
    ],
    final: { nativeInsertionTrace: { overflow: false }, fixture: { observationOverflow: false },
      nativeReadback: { armed: true, stopped: true, valid: true, error: null,
        maxSamplingGapMs: 0, records } },
    nativeInsertions: { valid: true },
    osReadback: { observations: labels.map(label => ({ label, bracket,
      sourceComparisons: [{}, { fromSourceStartMs: bracket.map(v => v - 100),
        residualFromSourceEndMs: bracket.map(v => v - 120) }] })) },
  };
  return rebind({ trialId, envelope: { marker, passed: true, report },
    verification: { passed: true, actualPasteVerified: true, trialId,
      documentIdentity: 'p4-textedit-a.txt',
      sha256: createHash('sha256').update(text).digest('hex') },
    build: { marker, trialId, sourceInputSha256: 'a', sourceSha256: 'b', sha256: 'c' },
    provenance: { testOnly: true, backendSourceSha256: 'd' },
    fixtureHashes: { 'episode-a.pcm': approvedFixtures['episode-a.pcm'][1],
      'episode-b.pcm': approvedFixtures['episode-b.pcm'][1] } });
}
const six = (baseline = 100, continued = 90) => ids.map(id =>
  attempt(id, id.startsWith('warm-baseline-') ? baseline : continued));

test('retains six metrics and passes only when both receipt and AX intervals pass', () => {
  const result = summarizeNativeQualification(six());
  assert.equal(result.decision, 'pass');
  assert.equal(result.eligibleAttempts.length, 6);
  assert.deepEqual(result.comparisons.receiptPrefix.differenceMs, [-10, -10]);
  assert.deepEqual(result.comparisons.axPrefix.differenceMs, [-20, 0]);
  assert.ok(result.attempts.every(r => r.metrics.receiptFull && r.metrics.axFull && r.metrics.axExact));
  assert.ok(result.attempts.every(r => r.metrics.receiptStablePrefix && r.metrics.receiptStableFull));
  assert.equal(Object.hasOwn(result, 'qualificationPassed'), false);
});

test('slow AX appearance prevents a false green despite fast receipt', () => {
  const rows = six();
  for (const row of rows.slice(3)) {
    const report = row.envelope.report;
    const delta = 700;
    for (const r of report.final.nativeReadback.records.slice(1)) {
      r.readStartMs += delta; r.readEndMs += delta; r.lastReadStartMs += delta; r.lastReadEndMs += delta;
    }
    for (const obs of report.osReadback.observations) {
      obs.bracket = obs.bracket.map(v => v + delta);
      obs.sourceComparisons[1].fromSourceStartMs = obs.sourceComparisons[1].fromSourceStartMs.map(v => v + delta);
      obs.sourceComparisons[1].residualFromSourceEndMs = obs.sourceComparisons[1].residualFromSourceEndMs.map(v => v + delta);
    }
    rebind(row);
  }
  assert.equal(summarizeNativeQualification(rows).decision, 'fail');
});

test('missing or slow stable B receipt cannot pass on early partial alone', () => {
  const missing = six();
  missing[3].envelope.report.events.pop();
  rebind(missing[3]);
  assert.equal(summarizeNativeQualification(missing).decision, 'inconclusive');
  assert.equal(summarizeNativeQualification(missing).comparisons.receiptStablePrefix, null);
  const slow = six();
  for (const row of slow.slice(3)) { row.envelope.report.events[1].atMs += 700; rebind(row); }
  const result = summarizeNativeQualification(slow);
  assert.equal(result.decision, 'fail');
  assert.ok(result.comparisons.receiptPrefix.differenceMs[1] <= 500);
  assert.ok(result.comparisons.receiptStablePrefix.differenceMs[0] > 500);
});

test('late full B appearance in AX fails even when prefix appears promptly', () => {
  const rows = six();
  for (const row of rows.slice(3)) {
    const report = row.envelope.report;
    const native = report.final.nativeReadback;
    const prefixRecord = native.records[2];
    const oldTime = prefixRecord.readEndMs;
    prefixRecord.text = 'за окном';
    native.records.push({ ...prefixRecord, sequence: 3,
      readStartMs: oldTime + 650, readEndMs: oldTime + 650,
      lastReadStartMs: oldTime + 650, lastReadEndMs: oldTime + 650 });
    native.records.push({ ...prefixRecord, sequence: 4,
      text: report.expectedInsertion, readStartMs: oldTime + 700,
      readEndMs: oldTime + 700, lastReadStartMs: oldTime + 700,
      lastReadEndMs: oldTime + 700 });
    for (const label of ['empirical B full sentence marker', 'exact cumulative stable text']) {
      const obs = report.osReadback.observations.find(v => v.label === label);
      obs.bracket = [oldTime + 650, oldTime + 700];
      obs.sourceComparisons[1].fromSourceStartMs = obs.bracket.map(v => v - 100);
      obs.sourceComparisons[1].residualFromSourceEndMs = obs.bracket.map(v => v - 120);
    }
    rebind(row);
  }
  const result = summarizeNativeQualification(rows);
  assert.equal(result.decision, 'fail');
  assert.ok(result.comparisons.axPrefix.differenceMs[1] <= 500);
  assert.ok(result.comparisons.axFull.differenceMs[0] > 500);
});

test('missing and failed attempts remain visible and block comparison', () => {
  const missing = summarizeNativeQualification(six().slice(0, 5));
  assert.deepEqual(missing.missingTrialIds, ['warm-continue-3']);
  assert.equal(missing.decision, 'inconclusive');
  const failed = six(); failed[5].verification.passed = false;
  rebind(failed[5]);
  const result = summarizeNativeQualification(failed);
  assert.equal(result.decision, 'inconclusive');
  assert.equal(result.attempts[5].outcome.verificationPassed, false);
  assert.equal(result.attempts[5].eligible, false);
});

test('rejects duplicate/unplanned IDs and untrusted timing or provenance', () => {
  assert.throws(() => summarizeNativeQualification([...six(), attempt('warm-continue-3')]));
  assert.throws(() => summarizeNativeQualification([attempt('cold-0')]));
  for (const edit of [
    r => { r.envelope.report.calibration.offset = [9, 10]; },
    r => { r.envelope.report.clockExchanges[0].w0 = NaN; },
    r => { r.envelope.report.events[0].atMs = Infinity; },
    r => { r.envelope.report.osReadback.observations[0].bracket = [1, 2]; },
    r => { r.build.sha256 = 'different'; },
    r => { r.envelope.report.eventOverflow = true; },
    r => { r.envelope.report.trial.gapMs = 200; },
  ]) {
    const rows = six(); edit(rows[3]);
    assert.equal(summarizeNativeQualification(rows).decision, 'inconclusive');
  }
});

test('interval crossing 500 ms stays inconclusive', () => {
  const rows = six(100, 600);
  for (const row of rows.slice(3)) {
    const report = row.envelope.report;
    report.calibration.offset = [0, 20];
    report.clockExchanges = report.clockExchanges.map((p, i) => ({
      ...p, w0: p.n + (i === 0 ? 0 : 0), w1: p.n + 20,
    }));
    rebind(row);
  }
  // Different calibration widths are permitted per trial, but the claimed bounds
  // must be exactly derivable from all six probes.
  assert.equal(summarizeNativeQualification(rows).decision, 'inconclusive');
});

test('binding protects six independent attempts sharing one source binary', () => {
  const rows = six();
  assert.equal(new Set(rows.map(r => r.build.sha256)).size, 1);
  assert.equal(new Set(rows.map(r => r.binding.artifacts.result.sha256)).size, 6);
  assert.equal(summarizeNativeQualification(rows).decision, 'pass');

  const missing = six(); delete missing[2].binding;
  const absent = summarizeNativeQualification(missing);
  assert.equal(absent.decision, 'inconclusive');
  assert.ok(absent.attempts[2].reasons.includes('missing-or-mismatched-attempt-binding'));

  const mixed = six();
  const newer = attempt(mixed[3].trialId);
  newer.build.sha256 = 'new-binary';
  newer.provenance.backendSourceSha256 = 'new-backend';
  rebind(newer);
  mixed[3].build = newer.build;
  mixed[3].provenance = newer.provenance;
  mixed[3].artifactBytes.build = newer.artifactBytes.build;
  mixed[3].artifactBytes.provenance = newer.artifactBytes.provenance;
  const changedBuild = summarizeNativeQualification(mixed);
  assert.equal(changedBuild.decision, 'inconclusive');
  assert.ok(changedBuild.attempts[3].reasons.includes('missing-or-mismatched-attempt-binding'));

  const wrongVerification = six();
  const other = attempt(wrongVerification[4].trialId);
  other.verification.utf8Bytes = 123;
  rebind(other);
  wrongVerification[4].verification = other.verification;
  wrongVerification[4].artifactBytes.verification = other.artifactBytes.verification;
  const changedVerification = summarizeNativeQualification(wrongVerification);
  assert.equal(changedVerification.decision, 'inconclusive');
  assert.ok(changedVerification.attempts[4].reasons.includes('missing-or-mismatched-attempt-binding'));
});

test('runner binding hashes exact retained bytes and refuses overwrite', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'native-summary-binding-'));
  try {
    // The runner emits bindings for all live trials, even though the summary
    // compares only the six warm baseline/Continue attempts.
    const row = attempt('cold-0');
    const files = { result: 'result-one.json', verification: 'qualification-verification.json',
      build: 'native-build.json', provenance: 'backend-provenance.json' };
    for (const [key, file] of Object.entries(files)) {
      await writeFile(path.join(directory, file), row.artifactBytes[key]);
    }
    await writeNativeQualificationBinding(directory, path.join(directory, files.result), row.trialId);
    const binding = JSON.parse(await readFile(path.join(directory, 'qualification-binding.json'), 'utf8'));
    assert.equal(binding.trialId, 'cold-0');
    assert.throws(() => summarizeNativeQualification([row]), /unplanned trial/);
    for (const [key, file] of Object.entries(files)) {
      assert.equal(binding.artifacts[key].file, file);
      assert.equal(binding.artifacts[key].sha256,
        createHash('sha256').update(await readFile(path.join(directory, file))).digest('hex'));
    }
    await assert.rejects(writeNativeQualificationBinding(directory, path.join(directory, files.result), row.trialId),
      { code: 'EEXIST' });
  } finally { await rm(directory, { recursive: true, force: true }); }
});
