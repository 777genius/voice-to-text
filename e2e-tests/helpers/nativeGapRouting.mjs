// Source-level composition only: real store function bodies, IPC/log ports stubbed.
// Invoked by nativeGapContracts.py with eligibility results from the Rust checks.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import vm from 'node:vm';
const source = readFileSync(new URL('../../src/stores/transcription.ts', import.meta.url), 'utf8');
function block(signature) {
  const start = source.indexOf(signature);
  assert.ok(start >= 0, `missing ${signature}`);
  const brace = source.indexOf('{', start);
  let end = brace + 1, depth = 1;
  while (depth && end < source.length) {
    depth += Number(source[end] === '{') - Number(source[end] === '}');
    end++;
  }
  assert.equal(depth, 0);
  return source.slice(start, end);
}
const functions = ['function getAutoPasteDelta(', 'function applyDeliveryMode(',
  'async function guardedContinuationCopy(', 'async function runAutoPasteCurrentText(']
  .map(block).join('\n');
const eligibility = JSON.parse(process.env.VT_GAP_ELIGIBILITY ?? 'null');
assert.ok(Array.isArray(eligibility) && eligibility.length === 3, 'Rust eligibility evidence required');
for (const mode of [...eligibility, true]) {
  assert.equal(typeof mode, 'boolean');
  const calls = [];
  const context = vm.createContext({
    invoke: async (command, args) => {
      calls.push({ command, args });
      return { status: 'confirmed', revision: 1 };
    },
    clientLog() {}, console: { log() {}, warn() {}, error() {} },
    deliveryRevision: { value: 0 },
  });
  vm.runInContext(stripTypeScriptTypes(functions), context);
  const ledger = { continuation: null, terminal: false, sessionId: 71, baseline: '',
    automaticDeliveryRefused: false, nativeDeliverySeq: 0, nativeRevision: 0 };
  context.applyDeliveryMode(ledger, mode);
  assert.equal(await context.runAutoPasteCurrentText('E50', 'owned dictation', ledger), true);
  assert.deepEqual(calls.map(c => c.command),
    [mode ? 'auto_paste_continuation_text' : 'auto_paste_text']);
  assert.equal(calls[0].args.sessionId, 71);
  assert.equal(calls[0].args.text, 'owned dictation');
  assert.equal(ledger.baseline, 'owned dictation');
  if (!mode) {
    assert.equal(ledger.nativeDeliverySeq, 0);
    assert.equal(ledger.nativeRevision, 0);
    assert.equal(await context.runAutoPasteCurrentText('E50 repeat', 'owned dictation', ledger), true);
    assert.equal(calls.length, 1);
  }
}
console.log('E50: three unsupported target routes plus qualified positive control passed (IPC mocked).');
