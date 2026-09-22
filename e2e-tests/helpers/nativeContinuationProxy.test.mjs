import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { hashPcm16Mono16k, startConfigDelayProxy } from './nativeContinuationProxy.mjs';
import { warmProviderCanaryTrial } from './nativeContinuation.mjs';
const require = createRequire(import.meta.url);
const WebSocket = createRequire(require.resolve('jsdom'))('ws');

test('proxy rejects external endpoints and unplanned fault delays before opening sockets', async () => {
  for (const url of ['wss://example.com', 'ws://127.0.0.1:51866', 'ws://secret@127.0.0.1:51869']) {
    await assert.rejects(startConfigDelayProxy(url, 0, () => {}));
  }
  await assert.rejects(startConfigDelayProxy('ws://127.0.0.1:51869', 1, () => {}));
});
test('proxy admits the exact paid warm-provider canary delay', async () => {
  assert.equal(warmProviderCanaryTrial.configDelayMs, 1000);
  const proxy = await startConfigDelayProxy(
    'ws://127.0.0.1:51869', warmProviderCanaryTrial.configDelayMs, () => {});
  await proxy.close();
});
test('bounded Config fault preserves text control frames and exact ordered PCM', { timeout: 8000 }, async () => {
  const server = new WebSocket.Server({ host: '127.0.0.1', port: 0 });
  await once(server, 'listening');
  const received = [];
  server.on('connection', socket => socket.on('message', (data, binary) => received.push({ data, binary })));
  const events = [];
  const proxy = await startConfigDelayProxy(`ws://127.0.0.1:${server.address().port}`, 4000,
    (name, details = {}) => events.push({ name, ...details, at: performance.now() }));
  const client = new WebSocket(`${proxy.url}/api/v1/transcribe/stream`);
  try {
    await once(client, 'open');
    client.send(JSON.stringify({ type: 'config', test: true }));
    client.send(Buffer.from([0, 1, 2, 3]));
    client.send(JSON.stringify({ type: 'pause', request_id: 'test' }));
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.equal(received.length, 0);
    await new Promise(resolve => setTimeout(resolve, 4100));
    assert.equal(received.length, 3);
    assert.deepEqual(received.map(x => x.binary), [false, true, false]);
    assert.deepEqual(received[1].data, Buffer.from([0, 1, 2, 3]));
    assert.equal(JSON.parse(received[2].data).type, 'pause');
    assert.deepEqual(events.filter(x => x.name === 'client_binary').map(x => ({
      connectionId: x.connectionId, bytes: x.bytes, pcmHash: x.pcmHash })),
    [{ connectionId: 1, bytes: 4, pcmHash: hashPcm16Mono16k([Buffer.from([0, 1, 2, 3])]) }]);
    const begin = events.find(x => x.name === 'fault_config_received');
    const end = events.find(x => x.name === 'fault_config_forwarded');
    assert.ok(end.at - begin.at >= 3950);
  } finally {
    client.terminate();
    await proxy.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});
test('queued PCM overflow terminates both sockets without forwarding partial input', { timeout: 5000 }, async () => {
  const server = new WebSocket.Server({ host: '127.0.0.1', port: 0 });
  await once(server, 'listening');
  let forwarded = 0;
  server.on('connection', socket => socket.on('message', () => forwarded++));
  const events = [];
  const proxy = await startConfigDelayProxy(`ws://127.0.0.1:${server.address().port}`, 8000, name => events.push(name));
  const client = new WebSocket(`${proxy.url}/api/v1/transcribe/stream`);
  try {
    await once(client, 'open');
    const closed = once(client, 'close');
    client.send(JSON.stringify({ type: 'config' }));
    client.send(Buffer.alloc(480000));
    client.send(Buffer.alloc(480000));
    await closed;
    assert.equal(forwarded, 0);
    assert.ok(events.includes('fault_proxy_overflow'));
    assert.equal(events.includes('fault_config_forwarded'), false);
  } finally {
    client.terminate();
    await proxy.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});

for (const abnormal of [false, true]) {
  test(`loopback ${abnormal ? 'abnormal termination fails' : 'empty Close survives delayed remote response'}`, { timeout: 5000 }, async () => {
    const { liveTrials, verifyQualificationConnections } = await import('./nativeContinuation.mjs');
    const { EventEmitter } = await import('node:events');
    const signal = AbortSignal.timeout(3500);
    const wait = (emitter, event) => {
      const pending = once(emitter, event, { signal });
      pending.catch(() => {}); // Cleanup may precede another awaited event on failure.
      return pending;
    };
    const observed = new EventEmitter();
    const events = [];
    const server = new WebSocket.Server({ host: '127.0.0.1', port: 0 });
    let proxy, client;
    try {
      await wait(server, 'listening');
      const connected = wait(server, 'connection');
      proxy = await startConfigDelayProxy(`ws://127.0.0.1:${server.address().port}`, 0, (event, detail) => {
        events.push({ event, ...detail });
        if (event === 'fault_proxy_close' && detail.direction === 'upstream') observed.emit('upstreamClosed');
      });
      client = new WebSocket(`${proxy.url}/api/v1/transcribe/stream`);
      await wait(client, 'open');
      const [remote] = await connected;
      const received = wait(remote, 'message');
      client.send(JSON.stringify({ type: 'config' }));
      await received;
      const closeMessage = wait(remote, 'message');
      client.send(JSON.stringify({ type: 'close' }));
      const [data, binary] = await closeMessage;
      assert.equal(binary, false);
      assert.equal(JSON.parse(data).type, 'close');
      const upstreamClosed = wait(observed, 'upstreamClosed');
      const remoteClosed = wait(remote, 'close');
      if (abnormal) client.terminate();
      else {
        // Hold the actual remote Close response until the relayed frame arrives.
        // Event-driven release avoids sleep-based success assertions.
        const responseRequested = wait(observed, 'responseRequested');
        const respond = remote.close.bind(remote);
        remote.close = (...args) => observed.emit('responseRequested', args);
        client.close(); // Empty wire Close, matching close(None).
        const [args] = await responseRequested;
        assert.equal(args[0], undefined); // ws received no wire status.
        assert.equal(events.some(e => e.direction === 'upstream'), false);
        assert.ok(events.some(e => e.direction === 'client' && e.code === 1005));
        remote.close = respond;
        respond(...args);
      }
      const [remoteCode] = await remoteClosed;
      await upstreamClosed;
      assert.equal(remoteCode, abnormal ? 1006 : 1005);
      assert.equal(events.filter(e => e.event === 'fault_proxy_connected').length, 1);
      assert.equal(events.find(e => e.direction === 'upstream').code, abnormal ? 1006 : 1005);
      const boundary = { event: 'qualification_pre_teardown', atMs: performance.now(), clock: 'runner-performance-now', nativeProcessAlive: true };
      const verify = extra => verifyQualificationConnections(liveTrials[0], [...events, ...extra, boundary]);
      if (abnormal) assert.throws(() => verify([]), /Unclean upstream close/);
      else {
        assert.equal(verify([]).normalStopConnectionReleaseVerified, true);
        assert.throws(() => verify([{ event: 'backend_control', type: 'error' }]), /Retained backend error/);
        assert.throws(() => verifyQualificationConnections(liveTrials[0], [...events, { ...boundary, nativeProcessAlive: false }]));
        assert.throws(() => verifyQualificationConnections(liveTrials[0], [...events.filter(e => e.direction !== 'upstream'), boundary]));
      }
    } finally {
      client?.terminate();
      for (const socket of server.clients) socket.terminate();
      if (proxy) await proxy.close();
      await new Promise(resolve => server.close(resolve));
    }
  });
}

test('HTTP upgrade routes exactly, forwards authorization and rejects other routes before upstream', { timeout: 5000 }, async () => {
  const { createServer } = await import('node:http');
  const http = createServer((_req, res) => res.end('healthy'));
  const ws = new WebSocket.Server({ noServer: true });
  const upgrades = [];
  http.on('upgrade', (req, socket, head) => {
    upgrades.push(req.url);
    if (req.url !== '/api/v1/transcribe/stream') {
      socket.end('HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n'); return;
    }
    ws.handleUpgrade(req, socket, head, remote => ws.emit('connection', remote, req));
  });
  const wait = (emitter, event) => once(emitter, event, { signal: AbortSignal.timeout(3000) });
  const logs = [];
  let proxy, client;
  try {
    http.listen(0, '127.0.0.1'); await wait(http, 'listening');
    const base = `ws://127.0.0.1:${http.address().port}`;
    const root = new WebSocket(base);
    const [error] = await wait(root, 'error');
    assert.match(error.message, /Unexpected server response: 200/);
    upgrades.length = 0;
    proxy = await startConfigDelayProxy(base, 0, (event, detail) => logs.push({ event, ...detail }));
    for (const route of ['/', '/other', '/api/v1/transcribe/stream/', '/api/v1/transcribe/stream?x=1', '/api/v1/transcribe/%73tream']) {
      const rejected = new WebSocket(proxy.url + route);
      await wait(rejected, 'error');
    }
    assert.deepEqual(upgrades, []);
    assert.deepEqual(logs, []);
    const connected = wait(ws, 'connection');
    client = new WebSocket(proxy.url + '/api/v1/transcribe/stream', { headers: { Authorization: 'Bearer fixture-only' } });
    await wait(client, 'open');
    const [remote, request] = await connected;
    assert.equal(request.headers.authorization, 'Bearer fixture-only');
    assert.deepEqual(upgrades, ['/api/v1/transcribe/stream']);
    const received = wait(remote, 'message');
    client.send(JSON.stringify({ type: 'config' }));
    const [data, binary] = await received;
    assert.equal(binary, false); assert.equal(JSON.parse(data).type, 'config');
    assert.doesNotMatch(JSON.stringify(logs), /Bearer|fixture-only|authorization/i);
  } finally {
    client?.terminate();
    if (proxy) await proxy.close();
    for (const socket of ws.clients) socket.terminate();
    await new Promise(resolve => ws.close(resolve));
    await new Promise(resolve => http.close(resolve));
  }
});
