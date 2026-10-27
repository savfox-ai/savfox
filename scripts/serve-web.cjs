#!/usr/bin/env node
// Dependency-free Glory development server: static files, HTTP/WS proxy and reload.
const http = require('node:http');
const https = require('node:https');
const fs = require('node:fs');
const path = require('node:path');
const { spawn } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const port = Number(process.argv[2] || '18080');
const backend = new URL(process.env.SAVFOX_DEV_BACKEND || 'http://127.0.0.1:18881');
if (!['http:', 'https:'].includes(backend.protocol)) throw new Error('SAVFOX_DEV_BACKEND must use http:// or https://');
const transport = backend.protocol === 'https:' ? https : http;
const buildRoot = process.platform === 'win32' ? 'D:/BuildCache/savfox-glory' : '/mnt/d/BuildCache/savfox-glory';
const output = path.join(buildRoot, 'web/debug');
const clients = new Set();
const sockets = new Set();
const watchers = [];
let building = false;
let pending = false;
let timer;
let child;
let stopping = false;

function build() {
  if (stopping) return;
  if (building) { pending = true; return; }
  building = true;
  child = spawn(process.execPath, [path.join(__dirname, 'build-web.cjs'), '--skip-copy'], { cwd: root, stdio: 'inherit', windowsHide: true, detached: process.platform !== 'win32' });
  child.on('error', error => { console.error(error); building = false; });
  child.on('exit', code => {
    building = false;
    if (code === 0) for (const client of clients) client.write('data: reload\n\n');
    else console.error(`Frontend build failed (${code}); keeping the current page.`);
    if (pending) { pending = false; build(); }
  });
}

function schedule() { clearTimeout(timer); timer = setTimeout(build, 250); }
const proxyPath = value => /^\/(api(?:\/|\?|$)|health(?:\?|$)|ws(?:\/|\?|$))/.test(value);
const mime = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.wasm': 'application/wasm', '.svg': 'image/svg+xml', '.ico': 'image/x-icon', '.png': 'image/png', '.json': 'application/json' };

const server = http.createServer((request, response) => {
  if (request.url === '/__reload') {
    response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
    response.write(': connected\n\n');
    clients.add(response);
    response.on('close', () => clients.delete(response));
    return;
  }
  if (proxyPath(request.url)) {
    const upstream = transport.request(new URL(request.url, backend), { method: request.method, headers: { ...request.headers, host: backend.host } }, result => {
      if (response.destroyed) { result.destroy(); return; }
      response.writeHead(result.statusCode, result.headers);
      result.on('error', () => response.destroy());
      result.on('aborted', () => response.destroy());
      result.pipe(response);
    });
    upstream.on('error', error => {
      if (response.destroyed) return;
      if (response.headersSent) { response.destroy(); return; }
      response.writeHead(502); response.end(`Gateway unavailable: ${error.message}`);
    });
    request.on('aborted', () => upstream.destroy());
    request.on('error', () => upstream.destroy());
    response.on('close', () => { if (!response.writableEnded) upstream.destroy(); });
    request.pipe(upstream);
    return;
  }
  let pathname;
  try { pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname); }
  catch { response.writeHead(400); response.end(); return; }
  let file = path.resolve(output, '.' + pathname);
  if (file !== output && !file.startsWith(output + path.sep)) { response.writeHead(403); response.end(); return; }
  try { if (!fs.statSync(file).isFile()) file = path.join(output, 'index.html'); }
  catch { file = path.join(output, 'index.html'); }
  fs.readFile(file, (error, data) => {
    if (error) { response.writeHead(503, { 'Content-Type': 'text/plain; charset=utf-8' }); response.end('Frontend is building. Reload when the build finishes.'); return; }
    response.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
    if (path.extname(file) === '.html') {
      data = data.toString().replace('</body>', '<script>new EventSource("/__reload").onmessage=()=>location.reload()</script></body>');
    }
    response.end(data);
  });
});

server.on('upgrade', (request, socket, head) => {
  if (!proxyPath(request.url)) { socket.destroy(); return; }
  const upstream = transport.request(new URL(request.url, backend), { method: request.method, headers: { ...request.headers, host: backend.host } });
  socket.on('error', () => upstream.destroy());
  socket.on('close', () => upstream.destroy());
  upstream.on('upgrade', (result, remote, remoteHead) => {
    if (socket.destroyed) { remote.destroy(); return; }
    socket.on('error', () => remote.destroy());
    remote.on('error', () => socket.destroy());
    socket.on('close', () => remote.destroy());
    remote.on('close', () => socket.destroy());
    socket.write(`HTTP/1.1 ${result.statusCode} ${result.statusMessage}\r\n`);
    for (let i = 0; i < result.rawHeaders.length; i += 2) socket.write(`${result.rawHeaders[i]}: ${result.rawHeaders[i + 1]}\r\n`);
    socket.write('\r\n');
    if (remoteHead.length) socket.write(remoteHead);
    if (head.length) remote.write(head);
    socket.pipe(remote).pipe(socket);
  });
  upstream.on('response', result => { socket.end(`HTTP/1.1 ${result.statusCode} ${result.statusMessage}\r\nConnection: close\r\n\r\n`); result.resume(); });
  upstream.on('error', () => socket.destroy());
  upstream.end();
});

server.on('connection', socket => {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
});

for (const relative of ['crates/gateway-glory/src', 'crates/gateway-glory/assets', 'crates/gateway-glory/locales', 'crates/gateway-shared/src', 'crates/utils/src']) {
  const directory = path.resolve(root, relative);
  if (fs.existsSync(directory)) watchers.push(fs.watch(directory, { recursive: true }, schedule));
}
for (const relative of ['Cargo.toml', 'Cargo.lock', '.cargo/config.toml', 'crates/gateway-glory/Cargo.toml', 'crates/gateway-glory/index.html', 'crates/gateway-shared/Cargo.toml', 'crates/utils/Cargo.toml', 'scripts/build-web.cjs', 'scripts/ui-sources.cjs', 'scripts/build-web.ps1']) {
  watchers.push(fs.watch(path.resolve(root, relative), schedule));
}
server.listen(port, '127.0.0.1', () => {
  console.log(`Savfox Glory: http://127.0.0.1:${port} (gateway ${backend.origin})`);
  build();
});
function close() {
  if (stopping) return;
  stopping = true;
  clearTimeout(timer);
  for (const watcher of watchers) watcher.close();
  for (const client of clients) client.end();
  for (const socket of sockets) socket.destroy();
  sockets.clear();
  if (child && child.exitCode === null && child.pid) {
    if (process.platform === 'win32') spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    else { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }
  }
  server.close();
}
process.on('SIGINT', close);
process.on('SIGTERM', close);
