import { createRequire } from 'node:module';
// Reuse pinned jsdom's installed ws; no added dependency or network installation.
const require = createRequire(import.meta.url);
const WebSocket = createRequire(require.resolve('jsdom'))('ws');
const { WebSocketServer } = WebSocket;
const FNV1A_OFFSET_BASIS = 0xcbf29ce484222325n;
const FNV1A_PRIME = 0x100000001b3n;
const FNV1A_MASK = 0xffffffffffffffffn;
const PCM_FORMAT = Buffer.from([0x80, 0x3e, 0x00, 0x00, 0x01, 0x00]);
const updatePcmHash = (hash, bytes) => {
  for (const byte of bytes) hash = ((hash ^ BigInt(byte)) * FNV1A_PRIME) & FNV1A_MASK;
  return hash;
};
export function hashPcm16Mono16k(chunks) {
  let hash = updatePcmHash(FNV1A_OFFSET_BASIS, PCM_FORMAT);
  for (const chunk of chunks) hash = updatePcmHash(hash, chunk);
  return hash.toString(16).padStart(16, '0');
}

// Delay forwarding Config, not merely displaying Ready. Audio queued behind Config
// keeps the same order and bytes. This is an explicit test fault, never production code.
export async function startConfigDelayProxy(upstreamUrl, delayMs, record) {
  const url = new URL(upstreamUrl);
  if (url.protocol !== 'ws:' || url.hostname !== '127.0.0.1' || !url.port || url.port === '51866' || url.username || url.password || url.search || url.hash || url.pathname !== '/') throw new Error('Explicit TEST loopback required');
  if (![0, 1000, 4000, 8000].includes(delayMs)) throw new Error('Unplanned Config delay');
  const route = '/api/v1/transcribe/stream';
  url.pathname = route;
  const server = new WebSocketServer({ verifyClient: ({ req }) => req.url === route, host: '127.0.0.1', port: 0, maxPayload: 960_000, perMessageDeflate: false });
  await new Promise((resolve, reject) => { server.once('listening', resolve); server.once('error', reject); });
  const sockets = new Set();
  const timers = new Set();
  let nextConnectionId = 0;
  server.on('connection', (client, request) => {
    const connectionId = ++nextConnectionId;
    record('fault_proxy_connected', { connectionId });
    const upstream = new WebSocket(url, { headers: { Authorization: request.headers.authorization ?? '' }, maxPayload: 960_000, perMessageDeflate: false, handshakeTimeout: 15000 });
    sockets.add(client); sockets.add(upstream);
    const queue = []; let bytes = 0; let open = false; let released = false; let configSeen = false;
    let intervalPcmHash = null;
    let deadline; let releaseTimer;
    const stop = () => { clearTimeout(deadline); timers.delete(deadline); clearTimeout(releaseTimer); timers.delete(releaseTimer); queue.length = 0; bytes = 0; client.terminate(); upstream.terminate(); };
    deadline = setTimeout(() => { timers.delete(deadline); record('fault_proxy_deadline'); stop(); }, 120000);
    timers.add(deadline);
    const flush = () => {
      if (!open || !released || upstream.readyState !== WebSocket.OPEN) return;
      while (queue.length) {
        const item = queue.shift(); bytes -= item.data.length;
        upstream.send(item.data, { binary: item.binary });
        if (item.config) record('fault_config_forwarded', { delay_ms: delayMs });
      }
    };
    upstream.on('open', () => { open = true; flush(); });
    upstream.on('message', (data, binary) => {
      if (!binary) {
        try {
          const message = JSON.parse(data.toString());
          if (['ready', 'pause_accepted', 'pause_rejected', 'continue_result', 'pause_restore_result', 'finalize_complete', 'error'].includes(message.type)) {
            const details = {};
            for (const key of ['type', 'session_id', 'provider_session_id', 'request_id', 'pause_epoch', 'decision', 'current_phase', 'eligible_now', 'accepted_capabilities', 'code', 'reason']) {
              if (message[key] !== undefined) details[key] = message[key];
            }
            record('backend_control', { connectionId, ...details });
            if (message.type === 'pause_accepted' && message.decision === 'accepted') intervalPcmHash = null;
          }
        } catch {}
      }
      if (client.bufferedAmount > 960_000) { record('fault_proxy_overflow'); stop(); return; }
      if (client.readyState === WebSocket.OPEN) client.send(data, { binary });
    });
    client.on('message', (data, binary) => {
      let config = false;
      if (!binary) { try { config = JSON.parse(data.toString()).type === 'config'; } catch {} }
      if (config && !configSeen) {
        configSeen = true; record('fault_config_received', { delay_ms: delayMs });
        releaseTimer = setTimeout(() => { timers.delete(releaseTimer); released = true; flush(); }, delayMs);
        timers.add(releaseTimer);
      }
      if (binary) {
        if (intervalPcmHash === null) intervalPcmHash = updatePcmHash(FNV1A_OFFSET_BASIS, PCM_FORMAT);
        intervalPcmHash = updatePcmHash(intervalPcmHash, data);
        record('client_binary', { connectionId, bytes: data.length,
          pcmHash: intervalPcmHash.toString(16).padStart(16, '0') });
      }
      bytes += data.length;
      if (bytes > 960_000 || queue.length >= 2048 || upstream.bufferedAmount > 960_000) { record('fault_proxy_overflow', { bytes }); stop(); return; }
      queue.push({ data, binary, config }); flush();
    });
    for (const socket of [client, upstream]) {
      socket.on('error', () => { record('fault_proxy_transport_error'); stop(); });
      socket.on('close', (code, reason) => {
        sockets.delete(socket);
        clearTimeout(deadline); timers.delete(deadline); clearTimeout(releaseTimer); timers.delete(releaseTimer);
        queue.length = 0; bytes = 0;
        const peer = socket === upstream ? client : upstream;
        record('fault_proxy_close', { connectionId, direction: socket === upstream ? 'upstream' : 'client', code, reasonBytes: reason.length });
        // Relay the actual close handshake; terminate would manufacture code 1006.
        if (peer.readyState === WebSocket.OPEN) {
          if (code === 1005) peer.close(); // Empty Close: never send reserved 1005 on the wire.
          else if (code === 1000 || code === 1001 || (code >= 1002 && code <= 1014 && ![1004,1005,1006].includes(code)) || code >= 3000) peer.close(code, reason);
          else peer.terminate();
        } else if (peer.readyState === WebSocket.CONNECTING) peer.terminate();
      });
    }
  });
  return {
    url: `ws://127.0.0.1:${server.address().port}`,
    close: async () => { for (const timer of timers) clearTimeout(timer); for (const socket of sockets) socket.terminate(); await new Promise(resolve => server.close(resolve)); },
  };
}
