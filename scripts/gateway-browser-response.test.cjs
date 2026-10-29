const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

// Execute the production browser-context request expression. Only Rust's
// format arguments and doubled format braces are substituted; its response
// reader and error classification run unchanged against a mocked Fetch API.
const source = fs.readFileSync(path.join(__dirname, '../crates/gateway-server/src/ws_rpc/handlers/browser.rs'), 'utf8');
const marker = source.indexOf('const targetUrl = {url_json};');
assert.ok(marker >= 0, 'production request expression exists');
const start = source.lastIndexOf('r#"', marker) + 3;
const end = source.indexOf('"#', marker);
const limit = Number(source.match(/const MAX_BROWSER_RESPONSE_BYTES: usize = (.*);/)[1].split('*').reduce((n, value) => n * Number(value.trim()), 1));
const script = source.slice(start, end)
  .replaceAll('{url_json}', JSON.stringify('https://example.invalid/request'))
  .replaceAll('{method_json}', JSON.stringify('POST'))
  .replaceAll('{headers_json}', '{}')
  .replaceAll('{body_json}', JSON.stringify('request-body'))
  .replaceAll('{MAX_BROWSER_RESPONSE_BYTES}', String(limit))
  .replaceAll('{{', '{').replaceAll('}}', '}');

async function request(response) {
  let fetches = 0;
  const result = await vm.runInNewContext(script, {
    fetch: async () => { fetches += 1; return response; },
    Uint8Array, TextDecoder,
  });
  return { result, fetches };
}

test('oversize Content-Length retains definitive rejection when cancellation fails', async () => {
  let cancelled = false;
  const { result, fetches } = await request({
    headers: new Map([['content-length', String(limit + 1)]]),
    body: { cancel: async () => { cancelled = true; throw new Error('cancel rejected'); } },
  });
  assert.equal(result.ok, false);
  assert.equal(result.too_large, true, 'Rust dispatcher must not retry this POST through the fallback transport');
  assert.equal(cancelled, true);
  assert.equal(fetches, 1);
});

test('oversize chunked response retains rejection when reader cancellation fails', async () => {
  let released = false;
  const { result } = await request({
    headers: new Map(),
    body: { getReader: () => ({
      read: async () => ({ done: false, value: new Uint8Array(limit + 1) }),
      cancel: async () => { throw new Error('cancel rejected'); },
      releaseLock: () => { released = true; },
    }) },
  });
  assert.equal(result.ok, false);
  assert.equal(result.too_large, true);
  assert.equal(released, true);
});

test('bounded chunked response is decoded successfully', async () => {
  let reads = 0;
  const { result } = await request({
    status: 200, url: 'https://example.invalid/request',
    headers: new Map([['content-type', 'text/plain']]),
    body: { getReader: () => ({
      read: async () => reads++ === 0
        ? { done: false, value: new TextEncoder().encode('complete answer') }
        : { done: true },
      releaseLock: () => {},
    }) },
  });
  assert.equal(result.ok, true);
  assert.equal(result.status, 200);
  assert.equal(result.body, 'complete answer');
});
