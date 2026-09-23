import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import path from 'node:path';
import { approvedFixtures, liveTrials } from './nativeContinuation.mjs';

const ids = ['warm-baseline-1', 'warm-baseline-2', 'warm-baseline-3',
  'warm-continue-1', 'warm-continue-2', 'warm-continue-3'];
const finite = n => typeof n === 'number' && Number.isFinite(n) && Math.abs(n) <= Number.MAX_SAFE_INTEGER;
const interval = v => Array.isArray(v) && v.length === 2 && v.every(finite) && v[0] <= v[1];
const normalize = s => s.toLocaleLowerCase('ru').replace(/ё/g, 'е').replace(/[.,!?]/g, '').replace(/\s+/g, ' ');
const median3 = rows => rows.map(v => v).sort((a, b) => a - b)[1];
const subtract = (a, b) => interval(a) && interval(b) ? [a[0] - b[1], a[1] - b[0]] : null;
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const artifactNames = ['result', 'verification', 'build', 'provenance'];
const marker = 'VOICETEXT_NATIVE_WINDOW_E2E_V1';

/** Hash exact on-disk bytes, including whitespace; no JSON reserialization in the CLI. */
export function createNativeQualificationBinding(trialId, files, bytes) {
  if (!liveTrials.some(trial => trial.id === trialId) || artifactNames.some(k => !Buffer.isBuffer(bytes?.[k]) ||
      typeof files?.[k] !== 'string' || path.basename(files[k]) !== files[k]))
    throw new Error('Complete attempt artifacts required for binding');
  return { schema: 'native-qualification-binding-v1', trialId, marker,
    artifacts: Object.fromEntries(artifactNames.map(k =>
      [k, { file: files[k], sha256: digest(bytes[k]) }])) };
}

export async function writeNativeQualificationBinding(directory, resultPath, trialId) {
  const files = { result: path.basename(resultPath), verification: 'qualification-verification.json',
    build: 'native-build.json', provenance: 'backend-provenance.json' };
  const bytes = Object.fromEntries(await Promise.all(artifactNames.map(async k =>
    [k, await readFile(path.join(directory, files[k]))])));
  const binding = createNativeQualificationBinding(trialId, files, bytes);
  await writeFile(path.join(directory, 'qualification-binding.json'), JSON.stringify(binding, null, 2), { flag: 'wx' });
}

function bindingValid(row) {
  const { trialId, envelope, verification, build, provenance, binding, artifactBytes } = row;
  if (binding?.schema !== 'native-qualification-binding-v1' || binding.trialId !== trialId ||
      binding.marker !== marker || envelope?.marker !== marker || build?.marker !== marker ||
      build?.trialId !== trialId || verification?.trialId !== trialId ||
      verification?.documentIdentity !== envelope?.report?.targetDocument ||
      verification?.sha256 !== digest(Buffer.from(envelope?.report?.expectedInsertion ?? '', 'utf8'))) return false;
  const parsed = { result: envelope, verification, build, provenance };
  return artifactNames.every(k => {
    const raw = artifactBytes?.[k], entry = binding.artifacts?.[k];
    if (!Buffer.isBuffer(raw) || !entry || typeof entry.file !== 'string' ||
        path.basename(entry.file) !== entry.file || !/^[a-f0-9]{64}$/.test(entry.sha256 ?? '') ||
        digest(raw) !== entry.sha256) return false;
    try { return isDeepStrictEqual(JSON.parse(raw.toString('utf8')), parsed[k]); }
    catch { return false; }
  });
}

function calibration(report) {
  const probes = report?.clockExchanges;
  if (!Array.isArray(probes) || probes.length !== 6 || report.calibration?.error !== null) return null;
  let lo = -Number.MAX_SAFE_INTEGER, hi = Number.MAX_SAFE_INTEGER;
  for (const [i, p] of probes.entries()) {
    if (![p?.w0, p?.n, p?.w1].every(finite) || p.w0 < 0 || p.n < 0 || p.w1 < p.w0 ||
        p.phase !== (i < 3 ? 'pre' : 'post') ||
        (i > 0 && (p.w0 < probes[i - 1].w1 || p.n < probes[i - 1].n))) return null;
    lo = Math.max(lo, p.w0 - p.n); hi = Math.min(hi, p.w1 - p.n);
    if (lo > hi) return null;
  }
  const claimed = report.calibration?.offset;
  return interval(claimed) && claimed[0] === lo && claimed[1] === hi ? claimed : null;
}

function readback(report, label, start, end) {
  const native = report.final?.nativeReadback, observations = report.osReadback?.observations;
  const row = observations?.find(v => v.label === label);
  const comparison = row?.sourceComparisons?.[1];
  if (!native?.armed || !native?.stopped || !native?.valid || native.error || native.shutdownError ||
      !Array.isArray(native.records) || native.records.length < 2 || native.records.length > 512 ||
      !finite(native.maxSamplingGapMs) || native.records[0]?.text !== '' ||
      !Array.isArray(observations) || observations.filter(v => v.label === label).length !== 1 ||
      !interval(row.bracket) || !interval(comparison?.fromSourceStartMs) ||
      !interval(comparison?.residualFromSourceEndMs)) return null;
  let previousEnd = -Infinity;
  for (const [i, r] of native.records.entries()) {
    if (r.sequence !== i || r.identityValid !== true || typeof r.text !== 'string' || r.text.length > 4096 ||
        !Number.isSafeInteger(r.samples) || r.samples < 1 ||
        ![r.readStartMs, r.readEndMs, r.lastReadStartMs, r.lastReadEndMs, r.maxSamplingGapMs].every(finite) ||
        r.readStartMs < previousEnd || r.readEndMs < r.readStartMs || r.lastReadStartMs < r.readStartMs ||
        r.lastReadEndMs < Math.max(r.lastReadStartMs, r.readEndMs) || r.maxSamplingGapMs < 0) return null;
    previousEnd = r.lastReadEndMs;
  }
  const predicate = label === 'empirical B prefix' ? s => normalize(s).includes('за окном') :
    label === 'empirical B full sentence marker' ? s => normalize(s).includes('за окном растет береза') :
    s => s === report.expectedInsertion && s.length > 0;
  let absent = null, bracket = null;
  for (const r of native.records) {
    if (predicate(r.text)) { if (absent !== null) bracket = [absent, r.readEndMs]; break; }
    absent = r.lastReadStartMs;
  }
  if (!bracket || bracket.some((v, i) => v !== row.bracket[i]) ||
      subtract(bracket, [start, start])?.some((v, i) => v !== comparison.fromSourceStartMs[i]) ||
      subtract(bracket, [end, end])?.some((v, i) => v !== comparison.residualFromSourceEndMs[i])) return null;
  return { bracket, fromSourceStartMs: comparison.fromSourceStartMs,
    residualFromSourceEndMs: comparison.residualFromSourceEndMs };
}

function receipt(report, phrase, offset, start, end, stableOnly = false) {
  const events = report.events?.filter(e => e.sessionId === report.episodes[1].logicalRunId &&
    (e.event === 'transcription:partial' || e.event === 'transcription:final') &&
    (!stableOnly || e.stable === true) &&
    e.syntheticTextValid === true && e.syntheticTextTruncated === false &&
    typeof e.rawSyntheticText === 'string' && normalize(e.rawSyntheticText).includes(phrase) && finite(e.atMs)) ?? [];
  const event = events.sort((a, b) => a.atMs - b.atMs)[0];
  const probes = report.clockExchanges;
  if (!event || event.atMs < probes[2].w1 || event.atMs > probes[3].w0) return null;
  const fromSourceStartMs = subtract([event.atMs, event.atMs], offset)?.map(v => v - start);
  const residualFromSourceEndMs = subtract([event.atMs, event.atMs], offset)?.map(v => v - end);
  if (!interval(fromSourceStartMs) || !interval(residualFromSourceEndMs)) return null;
  return { event: event.event, atMs: event.atMs, sessionId: event.sessionId,
    deliverySeq: event.deliverySeq, stable: event.stable,
    fromSourceStartMs, residualFromSourceEndMs };
}

export function summarizeNativeQualification(attempts) {
  if (!Array.isArray(attempts)) throw new Error('Attempts array required');
  const seen = new Set();
  const rows = attempts.map(({ trialId, envelope, verification, build, provenance, fixtureHashes,
    binding, artifactBytes, artifactPaths = null, readErrors = [] }) => {
    if (!ids.includes(trialId) || seen.has(trialId)) throw new Error(`Duplicate or unplanned trial: ${trialId}`);
    seen.add(trialId);
    const report = envelope?.report, reasons = [];
    if (!bindingValid({ trialId, envelope, verification, build, provenance, binding, artifactBytes }))
      reasons.push('missing-or-mismatched-attempt-binding');
    if (readErrors.length) reasons.push('artifact-read-or-path-error');
    if (verification?.passed !== true || verification.actualPasteVerified !== true ||
        verification.trialId !== trialId || envelope?.passed !== true || report?.passed !== true ||
        report?.trial?.id !== trialId || report?.errors?.length)
      reasons.push('failed-or-missing-outcome');
    if (report?.eventOverflow !== false || report?.final?.nativeInsertionTrace?.overflow !== false ||
        report?.final?.fixture?.observationOverflow !== false || report?.nativeInsertions?.valid !== true ||
        report?.events?.some(e => !finite(e.atMs) || e.syntheticTextTruncated ||
          e.syntheticTextValid === false)) reasons.push('overflow-truncation-or-invalid-event');
    const source = report?.episodes?.[1]?.source, start = source?.nativeSourceStartMs, end = source?.nativeSourceEndMs;
    if (report?.episodes?.length !== 2 || !finite(start) || !finite(end) || start < 0 || end < start ||
        !Number.isSafeInteger(report.episodes[1].logicalRunId) || report.episodes[1].logicalRunId <= 0)
      reasons.push('invalid-B-source');
    const offset = calibration(report);
    if (!offset) reasons.push('invalid-calibration');
    const metrics = offset && finite(start) && finite(end) ? {
      receiptPrefix: receipt(report, 'за окном', offset, start, end),
      receiptFull: receipt(report, 'за окном растет береза', offset, start, end),
      receiptStablePrefix: receipt(report, 'за окном', offset, start, end, true),
      receiptStableFull: receipt(report, 'за окном растет береза', offset, start, end, true),
      axPrefix: readback(report, 'empirical B prefix', start, end),
      axFull: readback(report, 'empirical B full sentence marker', start, end),
      axExact: readback(report, 'exact cumulative stable text', start, end),
    } : null;
    if (!metrics || ['receiptPrefix', 'receiptFull', 'axPrefix', 'axFull', 'axExact']
      .some(name => !metrics[name])) reasons.push('missing-or-invalid-empirical-marker');
    const planned = liveTrials.find(t => t.id === trialId);
    if (!isDeepStrictEqual(report?.trial, planned))
      reasons.push('trial-settings-mismatch');
    const identity = build?.sourceInputSha256 && build?.sourceSha256 && build?.sha256 &&
      provenance?.testOnly === true && provenance?.backendSourceSha256 &&
      fixtureHashes?.['episode-a.pcm'] === approvedFixtures['episode-a.pcm'][1] &&
      fixtureHashes?.['episode-b.pcm'] === approvedFixtures['episode-b.pcm'][1] ?
      { sourceInputSha256: build.sourceInputSha256, sourceSha256: build.sourceSha256,
        binarySha256: build.sha256, backendSourceSha256: provenance.backendSourceSha256,
        fixtures: fixtureHashes } : null;
    if (!identity) reasons.push('provenance-unknown');
    return { trialId, eligible: reasons.length === 0, reasons, identity, artifactPaths, readErrors,
      failureEvidence: { reportErrors: report?.errors ?? null, verificationError: verification?.error ?? null },
      outcome: { envelopePassed: envelope?.passed ?? null, reportPassed: report?.passed ?? null,
        verificationPassed: verification?.passed ?? null, actualPasteVerified: verification?.actualPasteVerified ?? null },
      source: { nativeSourceStartMs: start ?? null, nativeSourceEndMs: end ?? null },
      offset, metrics };
  });
  const missing = ids.filter(id => !seen.has(id));
  const identities = rows.map(r => JSON.stringify(r.identity));
  const comparable = !missing.length && rows.length === 6 && rows.every(r => r.eligible) &&
    new Set(identities).size === 1;
  if (!comparable && rows.every(r => r.eligible) && new Set(identities).size > 1)
    rows.forEach(r => r.reasons.push('cross-trial-provenance-mismatch'));
  const metricNames = ['receiptPrefix', 'receiptFull', 'receiptStablePrefix',
    'receiptStableFull', 'axPrefix', 'axFull', 'axExact'];
  const comparisons = Object.fromEntries(metricNames.map(name => {
    if (!comparable || rows.some(r => !r.metrics[name])) return [name, null];
    const median = group => [0, 1].map(i => median3(rows.filter(r => r.trialId.startsWith(group))
      .map(r => r.metrics[name].fromSourceStartMs[i])));
    const baseline = median('warm-baseline-'), continuation = median('warm-continue-');
    return [name, { baselineMedianMs: baseline, continueMedianMs: continuation,
      differenceMs: subtract(continuation, baseline) }];
  }));
  const required = ['receiptPrefix', 'receiptStablePrefix', 'axPrefix', 'axFull']
    .map(name => comparisons[name]?.differenceMs);
  const decision = !comparable || required.some(v => !interval(v)) ? 'inconclusive' :
    required.some(v => v[0] > 500) ? 'fail' :
    required.every(v => v[1] <= 500) ? 'pass' : 'inconclusive';
  return { schema: 'native-qualification-summary-v1', plannedTrialIds: ids, missingTrialIds: missing,
    eligibleAttempts: rows.filter(r => r.eligible).map(r => r.trialId), attempts: rows,
    comparability: comparable ? 'proven' : 'unknown', comparisons, decision,
    limitation: 'Empirical B text receipt and AX appearance markers only. No earliest B word, causal ASR attribution, or paint timing is proven.' };
}

async function loadIndex(index) {
  const attempts = await Promise.all(index.attempts.map(async row => {
    const readErrors = [];
    const artifactBytes = {};
    const load = async key => {
      if (!row[key]) return null;
      try {
        artifactBytes[key] = await readFile(row[key]);
        return JSON.parse(artifactBytes[key].toString('utf8'));
      }
      catch (error) { readErrors.push(`${key}: ${error.code ?? error.name}`); return null; }
    };
    const fixtureHashes = {};
    for (const [name, file] of Object.entries(row.fixtures ?? {})) {
      try { fixtureHashes[name] = createHash('sha256').update(await readFile(file)).digest('hex'); }
      catch (error) { readErrors.push(`fixture ${name}: ${error.code ?? error.name}`); }
    }
    const envelope = await load('result'), verification = await load('verification');
    const build = await load('build'), provenance = await load('provenance');
    const binding = await load('binding');
    if (binding && row.binding) {
      for (const key of artifactNames) {
        const expected = path.join(path.dirname(path.resolve(row.binding)), binding.artifacts?.[key]?.file ?? '');
        if (path.resolve(row[key] ?? '') !== expected) readErrors.push(`${key}: path disagrees with binding`);
      }
    }
    return { trialId: row.trialId, envelope, verification, build, provenance, binding,
      artifactBytes, fixtureHashes, artifactPaths: row, readErrors };
  }));
  return summarizeNativeQualification(attempts);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [indexPath, outputPath] = process.argv.slice(2);
  if (!indexPath || !outputPath) throw new Error('Usage: node nativeQualificationSummary.mjs index.json output.json');
  const result = await loadIndex(JSON.parse(await readFile(indexPath, 'utf8')));
  await writeFile(outputPath, JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
  console.log(`${result.decision}: ${outputPath}`);
}
