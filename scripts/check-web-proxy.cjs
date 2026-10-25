#!/usr/bin/env node
// Exercise the real dependency-free development server against local HTTP/WS fixtures.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const fs = require('node:fs');
const http = require('node:http');
const net = require('node:net');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const cache = process.platform === 'win32' ? 'D:/BuildCache/savfox-glory' : '/mnt/d/BuildCache/savfox-glory';
const output = path.join(cache, 'web/debug');
const evidence = path.join(cache, 'proxy-check');
const authorization = 'Bearer local-proxy-test-token';
const report = { checks: [], errors: [] };
const backendCalls = [];
const sockets = new Set();
const backendWebSockets = [];
let abortedSseClosedUpstream = false;
fs.mkdirSync(evidence, { recursive: true });
let development;
let developmentLog = '';
let backend;

const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
async function waitFor(condition, label, timeout = 30000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (condition()) return;
    if (development && development.exitCode !== null) throw new Error(`Development server exited (${development.exitCode}): ${developmentLog}`);
    await sleep(50);
  }
  throw new Error(`Timed out waiting for ${label}: ${developmentLog}`);
}

function request(port, resource, method = 'GET', body, headers = {}) {
  return new Promise((resolve, reject) => {
    const pending = http.request({ host: '127.0.0.1', port, path: resource, method, headers, agent: false }, response => {
      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.on('end', () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) }));
      response.on('error', reject);
    });
    pending.setTimeout(10000, () => pending.destroy(new Error(`HTTP fixture timed out: ${resource}`)));
    pending.on('error', reject);
    pending.end(body);
  });
}

function withTimeout(promise, label, milliseconds = 5000) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`Timed out waiting for ${label}`)), milliseconds); })]).finally(() => clearTimeout(timer));
}

function abortSse(port) {
  return withTimeout(new Promise((resolve, reject) => {
    let received = false;
    const pending = http.get({ host: '127.0.0.1', port, path: '/api/events', headers: { Authorization: authorization }, agent: false }, response => {
      try { assert.match(response.headers['content-type'], /^text\/event-stream/); }
      catch (error) { response.destroy(); pending.destroy(); reject(error); return; }
      response.once('data', chunk => {
        try { assert(chunk.toString().includes('data: fixture-event')); }
        catch (error) { reject(error); }
        received = true;
        response.destroy();
        pending.destroy();
        resolve();
      });
      response.on('error', error => { if (!received) reject(error); });
    });
    pending.on('error', error => { if (!received) reject(error); });
  }), 'streamed SSE response');
}

function frame(text, masked = false) {
  const payload = Buffer.from(text);
  assert(payload.length < 126, 'Fixtures deliberately use short WebSocket frames');
  const header = Buffer.from([0x81, payload.length | (masked ? 0x80 : 0)]);
  if (!masked) return Buffer.concat([header, payload]);
  const mask = Buffer.from([0x13, 0x24, 0x35, 0x46]);
  const encoded = Buffer.from(payload);
  for (let index = 0; index < encoded.length; index++) encoded[index] ^= mask[index % 4];
  return Buffer.concat([header, mask, encoded]);
}

function consumeFrames(buffer, masked, accept) {
  let offset = 0;
  while (buffer.length - offset >= 2) {
    const length = buffer[offset + 1] & 0x7f;
    assert(length < 126, 'Unexpected long fixture frame');
    assert.equal(Boolean(buffer[offset + 1] & 0x80), masked, 'WebSocket masking must survive the proxy');
    const header = 2 + (masked ? 4 : 0);
    if (buffer.length - offset < header + length) break;
    assert.equal(buffer[offset] & 0x0f, 1, 'Expected a text frame');
    const payload = Buffer.from(buffer.subarray(offset + header, offset + header + length));
    if (masked) for (let index = 0; index < payload.length; index++) payload[index] ^= buffer[offset + 2 + index % 4];
    accept(payload.toString());
    offset += header + length;
  }
  return buffer.subarray(offset);
}

function attachBackendWebSocket(server) {
  server.on('upgrade', (incoming, socket, head) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    socket.on('error', error => report.errors.push(error.message));
    // An upgraded HTTP socket permits half-open TCP connections; the fixture
    // must finish its writable side when the real proxy closes the readable side.
    socket.on('end', () => socket.end());
    backendWebSockets.push({ socket, closed: new Promise(resolve => socket.once('close', resolve)) });
    backendCalls.push({ upgrade: true, path: incoming.url, authorization: incoming.headers.authorization, host: incoming.headers.host });
    const accept = crypto.createHash('sha1').update(incoming.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    // Coalescing the first server frame with the 101 response exercises remoteHead.
    socket.write(Buffer.concat([
      Buffer.from(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nX-Proxy-Fixture: websocket\r\n\r\n`),
      frame('backend-upgrade-head'),
    ]));
    let buffered = head;
    const acceptMessage = message => {
      backendCalls.push({ message });
      socket.write(frame(`echo:${message}`));
      if (message === 'client-message') socket.write(frame('backend-push'));
    };
    try { buffered = consumeFrames(buffered, true, acceptMessage); }
    catch (error) { report.errors.push(error.message); socket.destroy(); return; }
    socket.on('data', data => {
      try { buffered = consumeFrames(Buffer.concat([buffered, data]), true, acceptMessage); }
      catch (error) { report.errors.push(error.message); socket.destroy(); }
    });
  });
}

function websocket(port, keepOpen = false) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ host: '127.0.0.1', port });
    const closed = new Promise(resolve => socket.once('close', resolve));
    const key = Buffer.from('savfox-ws-fixtur').toString('base64');
    const expectedAccept = crypto.createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    const required = new Set(['backend-upgrade-head', 'echo:client-upgrade-head', 'echo:client-message', 'backend-push']);
    const messages = [];
    let buffered = Buffer.alloc(0);
    let upgraded = false;
    const timeout = setTimeout(() => finish(new Error(`WebSocket proxy timed out; received ${messages.join(', ')}`)), 10000);
    let finished = false;
    function finish(error) {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      if (error || !keepOpen) socket.destroy();
      if (error) reject(error); else resolve(keepOpen ? { messages, socket, closed } : messages);
    }
    socket.on('error', finish);
    socket.on('connect', () => {
      // Coalescing the first client frame with the HTTP upgrade exercises head.
      socket.write(Buffer.concat([
        Buffer.from(`GET /ws?transport=proxy-test HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ${key}\r\nAuthorization: ${authorization}\r\n\r\n`),
        frame('client-upgrade-head', true),
      ]));
    });
    socket.on('data', data => {
      try {
        buffered = Buffer.concat([buffered, data]);
        if (!upgraded) {
          const boundary = buffered.indexOf('\r\n\r\n');
          if (boundary === -1) return;
          const headers = buffered.subarray(0, boundary).toString();
          assert(headers.startsWith('HTTP/1.1 101 '));
          assert(headers.toLowerCase().includes(`sec-websocket-accept: ${expectedAccept}`.toLowerCase()));
          assert(headers.toLowerCase().includes('x-proxy-fixture: websocket'));
          buffered = buffered.subarray(boundary + 4);
          upgraded = true;
          socket.write(frame('client-message', true));
        }
        buffered = consumeFrames(buffered, false, message => { messages.push(message); required.delete(message); });
        if (required.size === 0) finish();
      } catch (error) { finish(error); }
    });
  });
}

async function reservePort() {
  const reservation = net.createServer();
  await new Promise(resolve => reservation.listen(0, '127.0.0.1', resolve));
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  return port;
}

async function stopDevelopment() {
  if (!development || development.exitCode !== null) return;
  // Terminate only the child tree created by this test, including a pending build.
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(development.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
  else development.kill('SIGTERM');
  await Promise.race([new Promise(resolve => development.once('exit', resolve)), sleep(2000)]);
}

(async () => {
  try {
    for (const file of ['index.html', 'wasm/savfox_gateway_glory.js', 'wasm/savfox_gateway_glory_bg.wasm']) assert(fs.existsSync(path.join(output, file)), 'Run scripts/build-web.ps1 before the proxy integration check');
    backend = http.createServer(async (incoming, response) => {
      const chunks = [];
      for await (const chunk of incoming) chunks.push(chunk);
      const record = { method: incoming.method, path: incoming.url, authorization: incoming.headers.authorization, host: incoming.headers.host, body: Buffer.concat(chunks).toString() };
      backendCalls.push(record);
      if (incoming.url === '/api/events') {
        response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' });
        response.write('data: fixture-event\n\n');
        response.on('close', () => { abortedSseClosedUpstream = !response.writableEnded; });
        return;
      }
      response.writeHead(incoming.url.startsWith('/health') ? 200 : 201, { 'Content-Type': 'application/json', 'X-Proxy-Fixture': 'http' });
      response.end(JSON.stringify(incoming.url.startsWith('/health') ? { ok: true } : record));
    });
    backend.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
    attachBackendWebSocket(backend);
    await new Promise(resolve => backend.listen(0, '127.0.0.1', resolve));
    const backendPort = backend.address().port;
    const port = await reservePort();
    // IPC emits the production SIGTERM handler on Windows, where OS kill signals
    // otherwise bypass Node handlers. No test-only production option is needed.
    const bootstrap = 'const entry=process.argv[1];const port=process.argv[2];process.argv=[process.execPath,entry,port];process.on("message",message=>{if(message==="shutdown"){process.emit("SIGTERM");process.disconnect();}});require(entry);';
    development = spawn(process.execPath, ['-e', bootstrap, path.join(__dirname, 'serve-web.cjs'), String(port)], {
      cwd: root, windowsHide: true,
      env: { ...process.env, SAVFOX_DEV_BACKEND: `http://127.0.0.1:${backendPort}`, CARGO_BUILD_JOBS: '2', TEMP: evidence, TMP: evidence },
      stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
    });
    development.on('error', error => report.errors.push(error.message));
    for (const stream of [development.stdout, development.stderr]) stream.on('data', chunk => { developmentLog += chunk; });
    await waitFor(() => developmentLog.includes(`Savfox Glory: http://127.0.0.1:${port}`), 'development listener');

    const body = JSON.stringify({ message: 'forwarded JSON body', count: 2 });
    const forwarded = await request(port, '/api/echo?escaped=%2F&query=test', 'PATCH', body, { Authorization: authorization, 'Content-Type': 'application/json' });
    assert.equal(forwarded.status, 201);
    assert.equal(forwarded.headers['x-proxy-fixture'], 'http');
    const received = JSON.parse(forwarded.body);
    assert.equal(received.path, '/api/echo?escaped=%2F&query=test');
    assert.equal(received.method, 'PATCH');
    assert.equal(received.authorization, authorization);
    assert.equal(received.host, `127.0.0.1:${backendPort}`);
    assert.equal(received.body, body);
    const apiRoot = await request(port, '/api?root=true');
    assert.equal(JSON.parse(apiRoot.body).path, '/api?root=true');
    const health = await request(port, '/health?probe=true');
    assert.equal(health.status, 200);
    assert.deepEqual(JSON.parse(health.body), { ok: true });
    report.checks.push('HTTP API path/query/method/body/Authorization and response headers; health proxy');
    await abortSse(port);
    await waitFor(() => abortedSseClosedUpstream, 'client SSE cancellation closing the upstream response', 5000);
    report.checks.push('aborting a streamed SSE client closes its upstream HTTP response');

    const page = await request(port, '/channels');
    const index = fs.readFileSync(path.join(output, 'index.html'), 'utf8');
    assert.equal(page.status, 200);
    assert.match(page.headers['content-type'], /^text\/html/);
    assert.equal(page.body.toString(), index.replace('</body>', '<script>new EventSource("/__reload").onmessage=()=>location.reload()</script></body>'));
    for (const [file, mime] of [['wasm/savfox_gateway_glory.js', /^text\/javascript/], ['wasm/savfox_gateway_glory_bg.wasm', /^application\/wasm/]]) {
      const asset = await request(port, '/' + file);
      assert.equal(asset.status, 200);
      assert.match(asset.headers['content-type'], mime);
      assert(asset.body.equals(fs.readFileSync(path.join(output, file))), `Proxy served altered ${file}`);
    }
    report.checks.push('SPA nested route and exact generated JavaScript/WebAssembly bytes with correct MIME');

    const messages = await websocket(port);
    const upgrade = backendCalls.find(record => record.upgrade);
    assert.equal(upgrade.path, '/ws?transport=proxy-test');
    assert.equal(upgrade.authorization, authorization);
    assert.equal(upgrade.host, `127.0.0.1:${backendPort}`);
    assert(backendCalls.some(record => record.message === 'client-upgrade-head'));
    assert(backendCalls.some(record => record.message === 'client-message'));
    report.websocketMessages = messages;
    report.checks.push('WebSocket upgrade/query/Authorization; bidirectional text frames; client and backend upgrade-head bytes');

    await waitFor(() => /Frontend inputs unchanged; reusing WASM bundle|Glory \+ Sapid frontend built:/.test(developmentLog), 'successful initial build/cache check', 180000);
    assert(!developmentLog.includes('Frontend build failed'), developmentLog);
    report.checks.push(developmentLog.includes('Frontend inputs unchanged; reusing WASM bundle') ? 'unchanged generated bundle reused by development startup' : 'development startup rebuilt the current source successfully');
    const openWebSocket = await websocket(port, true);
    assert.equal(openWebSocket.socket.destroyed, false);
    const exited = new Promise(resolve => development.once('exit', code => resolve(code)));
    development.send('shutdown');
    const [exitCode] = await withTimeout(Promise.all([exited, openWebSocket.closed, Promise.all(backendWebSockets.map(connection => connection.closed))]), 'development shutdown to close client and upstream WebSockets');
    assert.equal(exitCode, 0, developmentLog);
    report.checks.push('graceful development shutdown closes active WebSocket clients and upstream sockets');
    assert.equal(report.errors.length, 0, report.errors.join('\n'));
  } catch (error) {
    report.errors.push(error.stack || String(error));
    process.exitCode = 1;
  } finally {
    await stopDevelopment();
    for (const socket of sockets) socket.destroy();
    if (backend) await new Promise(resolve => backend.close(resolve));
    report.backendCalls = backendCalls;
    fs.writeFileSync(path.join(evidence, 'development.log'), developmentLog);
    fs.writeFileSync(path.join(evidence, 'report.json'), JSON.stringify(report, null, 2));
    console.log(JSON.stringify(report, null, 2));
  }
})();
