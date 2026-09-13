import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { startConfigDelayProxy } from './nativeContinuationProxy.mjs';
import { liveTrials, verifyQualificationConnections } from './nativeContinuation.mjs';
import { runOwned, createQualificationCollector } from '../run-native-window-e2e.mjs';
const require = createRequire(import.meta.url);
const wsPath = createRequire(require.resolve('jsdom')).resolve('ws');
const { WebSocketServer } = require(wsPath);
const trial = liveTrials.find(t => t.id === 'warm-baseline-1');
const envelope = { marker: 'VOICETEXT_NATIVE_WINDOW_E2E_V1', passed: true, report: { trial },
  preTeardown: { normalStopReleased: true, cleanupDeferredToRunner: true } };
for (const normalClose of [true, false]) {
  test(`actual external proxy close ${normalClose ? 'before' : 'only during'} runner teardown`, async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'p4-r2-collector-'));
    const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
    await new Promise(resolve => server.once('listening', resolve));
    server.on('connection', socket => socket.on('message', () => socket.send(JSON.stringify({ type: 'ready', session_id: 'test' }))));
    const events = [];
    const proxy = await startConfigDelayProxy(`ws://127.0.0.1:${server.address().port}`, 0,
      (event, details) => events.push({ event, ...details }));
    const result = path.join(directory, 'result.json');
    const script = `const fs = require('node:fs'); const WS = require(${JSON.stringify(wsPath)});
      const socket = new WS(${JSON.stringify(`${proxy.url}/api/v1/transcribe/stream`)});
      socket.on('open', () => socket.send(JSON.stringify({type:'config'})));
      socket.on('message', () => {
        fs.writeFileSync(${JSON.stringify(result)}, ${JSON.stringify(JSON.stringify(envelope))});
        ${normalClose ? 'socket.close(1000);' : ''} });
      setInterval(() => {}, 1000);`;
    const collector = createQualificationCollector(trial, events,
      async () => JSON.parse(await readFile(result, 'utf8')), () => performance.now());
    try {
      const run = runOwned(process.execPath, ['-e', script], {}, 10000, path.join(directory, 'child.log'), undefined, collector);
      if (normalClose) {
        await run;
        verifyQualificationConnections(trial, events);
        assert.equal(events.at(-1).event, 'qualification_pre_teardown');
      } else {
        await assert.rejects(run, /count\/cleanup failed|Normal Stop/);
        assert.equal(events.some(e => e.event === 'qualification_pre_teardown'), false);
        assert.throws(() => verifyQualificationConnections(trial, events));
      }
    } finally {
      await proxy.close();
      for (const socket of server.clients) socket.terminate();
      await new Promise(resolve => server.close(resolve));
      await rm(directory, { recursive: true, force: true });
    }
  });
}
test('collector refuses repaired failure evidence, premature exit and abnormal collected exit', async () => {
  const collector = createQualificationCollector(trial, [], async () => ({ ...envelope, passed: false }), () => 0);
  await assert.rejects(collector(), /pre-teardown/);
  const directory = await mkdtemp(path.join(os.tmpdir(), 'p4-r2-early-exit-'));
  try {
    await assert.rejects(runOwned(process.execPath, ['-e', 'process.exit(0)'], {}, 1000,
      path.join(directory, 'child.log'), undefined, async () => false), /exited before normal-Stop/);
    const ready = path.join(directory, 'ready');
    const script = `process.on('SIGTERM', () => process.exit(7)); require('node:fs').writeFileSync(${JSON.stringify(ready)}, 'ready'); setInterval(() => {}, 1000);`;
    await assert.rejects(runOwned(process.execPath, ['-e', script], {}, 1000,
      path.join(directory, 'abnormal.log'), undefined, async () => {
        try { await readFile(ready); return true; } catch { return false; }
      }), /failed.*status.*7/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
