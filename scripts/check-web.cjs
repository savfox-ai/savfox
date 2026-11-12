#!/usr/bin/env node
// Browser regression test against the generated bundle and deterministic gateway fixtures.
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const root = path.resolve(__dirname, '..');
const cache = process.env.SAVFOX_WEB_CHECK_CACHE || path.join(root, 'artifacts/web-check');
fs.mkdirSync(cache, { recursive: true });
const { chromium } = require(process.env.SAVFOX_PLAYWRIGHT || path.resolve(root, '../../glory-rs/glory/tests/playwright/node_modules/playwright'));
const { cargoTargetDirectory } = require('./cargo-target.cjs');
const bundle = path.resolve(process.env.SAVFOX_WEB_BUNDLE || path.join(cargoTargetDirectory(root), 'web-build/web/debug'));
const token = 'browser-test-token';
const calls = [];
const errors = [];
const report = { routes: [], checks: [], errors };
let browser;
let page;
let currentSocket;
let connections = 0;
async function waitForCall(predicate) {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (calls.some(predicate)) return;
    await new Promise(resolve => setTimeout(resolve, 25));
  }
  assert.fail("Expected gateway request did not arrive");
}
let agentName = 'Browser agent';
let signInComplete = false;
const agent = () => ({ id: 'browser-agent', name: agentName, kind: 'native', native: { provider: 'openai', model: 'test-model' } });
const terminalAgent = { id: 'terminal-agent', name: 'Fixture terminal', kind: 'terminal', terminal: { mode: 'managed_pty', command: 'fixture-shell' } };
const arkretDelivery = { bindingId: 'fixture-binding', mode: 'task_delivery', state: 'active', conversation: { channelConfigId: 'fixture-channel', accountId: 'fixture-account', realmId: 'fixture-realm', strandId: 'fixture-strand' }, sourceSenderDid: 'did:fixture:source', agentSenderDid: 'did:fixture:agent', pendingOutbox: 0, publishedCheckpoints: 0, hydratedEvents: 0, hydrationTruncated: false };
let terminalRunning = false;
let terminalFailNextWrite = false;
let terminalEntries = [];
const terminalMetadata = () => ({ key: { agent_id: 'terminal-agent', session_id: 'terminal-session' }, completion: terminalRunning ? 'running' : 'explicit_close', command: 'fixture-shell' });
const terminalAppend = text => terminalEntries.push({ sequence: terminalEntries.length + 1, text, stream: 'stdout' });
const redactedChannelSecret = '__SAVFOX_CHANNEL_SECRET_REDACTED__';
const channel = { id: 'discord-test', kind: 'discord', name: 'Browser Discord', enabled: true, config: { bot_token: redactedChannelSecret, mode: 'stream' } };
let channelRunning = true;
let fixtureConfig = { model: { provider: 'openai', slug: 'test-model' }, gateway: { port: 18881 }, model_context_window: 32000 };
let configVersion = 1;
let fixtureClients = [{session_id:'fixture-client-session',name:'Fixture client',connected_at:'2026-11-01T00:00:00Z',is_current:false}];
let voiceSettings = { enabled: false, keyword: "hey savfox" };
let skillEnabled = true;
const model = { id: 'openai/test-model', name: 'Test model', provider: 'openai', model_slug: 'test-model' };
function fixture(method, params) {
  switch (method) {
    case 'status': return { version: 'test', uptime_ms: 1000, connected_clients: 1 };
    case 'agents.list': return { agents: [agent(), terminalAgent] };
    case 'agents.get': return agent();
    case 'agents.create': agentName = params.name; return { id: params.id };
    case 'agents.update': agentName = params.name || agentName; return { ok: true };
    case 'agents.files.list': return { files: [] };
    case 'agents.skills.get': return { skills: [] };
    case 'models.list': return { models: [model] };
    case 'models.test': return { ok: true, message: 'Connection verified' };
    case 'account/login/start': signInComplete = false; return params.type === 'apiKey' ? { type: 'apiKey' } : { loginId: 'fixture-login', authUrl: 'https://example.test/sign-in', verificationUrl: 'https://example.test/device', userCode: 'TEST-CODE' };
    case 'account/read': return { account: signInComplete ? { type: 'chatgpt', email: 'browser@example.test', planType: 'test' } : null };
    case 'account/login/cancel': return { cancelled: true };
    case 'sessions.list': return { entries: [{ session_id: 'browser-session', label: 'Browser conversation', agent_id: 'browser-agent', message_count: 1 }, { session_id: 'terminal-session', label: 'Terminal conversation', agent_id: 'terminal-agent', message_count: 1 }, { session_id: 'delivery-session', label: 'Delivery conversation', agent_id: 'browser-agent', arkret_delivery: arkretDelivery, message_count: 1 }] };
    case 'sessions.patch': return { session_id: params.session_id || 'browser-new-session' };
    case 'chat.history':
      if (params.session_id === 'performance-session') return { messages: Array.from({ length: 2000 }, (_, index) => ({ role: index % 2 ? 'assistant' : 'user', content: `Performance message ${index}\n\n` + 'Variable height paragraph.\n\n'.repeat(index % 7 + 1) })) };
      return { messages: [{ role: 'user', content: 'Earlier message', attachments: [{ filename: 'fixture-preview.png', mime_type: 'image/png', data_base64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aFtwAAAAASUVORK5CYII=' }] }, { role: 'assistant', content: 'History restored\n\n**Fixture strong**\n\n- Fixture list item\n\n```js\nconst answer = 42;\n```' }] };
    case 'sessions.ambient.get': return { session_id: params.session_id, messages: [] };
    case 'sessions.idle_reply.get': return { pending: null, recent_sent_count: 0 };
    case 'sessions.arkret.delivery.preview': return { rendered: `Reviewed checkpoint: ${params.summary}`, senderDid: 'did:fixture:agent', previousRendered: null };
    case 'sessions.arkret.delivery.publish': return { remoteEventId: 'fixture-published-event' };
    case 'agent.terminal.pty.list': return { sessions: terminalRunning ? [terminalMetadata()] : [] };
    case 'agent.terminal.pty.start':
      assert.equal(params.agent, 'terminal-agent'); assert.equal(params.session_id, 'terminal-session');
      terminalRunning = true; terminalEntries = []; terminalAppend('Fixture terminal ready\n');
      return { started: true, metadata: terminalMetadata(), transcript: terminalEntries };
    case 'agent.terminal.pty.read':
      assert(terminalRunning, 'Cannot read a terminal before it starts');
      return { metadata: terminalMetadata(), entries: terminalEntries.filter(entry => entry.sequence > (params.since_sequence || 0)) };
    case 'agent.terminal.pty.write':
      assert(terminalRunning, 'Cannot write a terminal before it starts');
      assert.equal(params.agent, 'terminal-agent'); assert.equal(params.session_id, 'terminal-session');
      terminalAppend(params.kind === 'interrupt' ? 'Interrupted\n' : `Executed: ${params.text}\n`);
      return { written: true, metadata: terminalMetadata() };
    case 'agent.terminal.pty.close': terminalRunning = false; return { closed: true, metadata: terminalMetadata() };
    case 'channels.config.list': return [channel];
    case 'channels.config.get': return { config: params.channel === channel.id || params.channel === channel.kind ? channel : null };
    case 'channels.status': {
      const discord = { platform: 'discord', configured: true, connected: channelRunning, running: channelRunning };
      if (params.channel) {
        assert(!params.channel.includes('-'), 'Channel status must pass canonical platform separately from instance id');
        if (params.id) assert.equal(params.id, channel.id);
        return params.channel === 'discord' ? discord : { platform: params.channel, configured: false, running: false, connected: false };
      }
      return { channels: { discord }, instances: { [channel.id]: { ...discord, id: channel.id } } };
    }
    case 'channels.config.save': {
      if (params.id) channel.id = params.id;
      if (params.name) channel.name = params.name;
      if (typeof params.config?.enabled === 'boolean') channel.enabled = params.config.enabled;
      channel.config = { ...channel.config, ...params.config };
      return { ok: true, config: { ...channel, config: { ...channel.config, bot_token: redactedChannelSecret } } };
    }
    case 'channels.login': channelRunning = true; return { status: 'started' };
    case 'channels.logout': channelRunning = false; return { status: 'stopped' };
    case 'channels.test': return { ok: true, message: 'Channel connection verified' };
    case 'channels.config.delete': return { deleted: true };
    case 'channels.matrix.invites': return { invites: [] };
    case 'channels.nostr.profile.get': return { profile: { name: 'Fixture profile', public_key: 'npub-fixture' } };
    case 'channels.nostr.relays.get': return { relays: ['wss://relay.example.org'] };
    case 'channels.nostr.profile.set': return { status: 'saved' };
    case 'channels.nostr.profile.import': return { status: 'imported' };
    case 'channels.nostr.profile.export': return { profile: { name: 'Fixture profile', public_key: 'npub-fixture' } };
    case 'channels.nostr.relays.set': return { status: 'saved', relays: params.relays };
    case 'config.get': return { config: structuredClone(fixtureConfig), revision: String(configVersion), removed_fields: [] };
    case 'config.set': { assert.equal(params.revision,String(configVersion)); fixtureConfig=structuredClone(params.config); configVersion++; return {status:'saved',config:structuredClone(fixtureConfig),revision:String(configVersion),restart_required:true}; }
    case 'config.export': return {content:JSON.stringify(fixtureConfig),redacted:true};
    case 'config.schema': return { schema: { type: 'object', properties: { gateway: { type: 'object', properties: { port: { type: 'integer' }, response_footer:{type:'object',properties:{enabled:{type:'boolean',default:true}}}, rate_limit:{type:'object',properties:{max_requests:{type:'integer'}}} } }, model_context_window:{type:'integer'},check_for_update_on_startup:{anyOf:[{type:'boolean'},{type:'null'}]},mcp_servers:{type:'object',additionalProperties:{type:'object'}},model:{type:'object'} } } };
    case 'cron.list': return { jobs: [] };
    case 'cron.status': return { enabled: true };
    case 'cron.runs': return { runs: [] };
    case 'node.list': return { nodes: [] };
    case 'node.describe': return { id: 'browser-node', name: 'Browser node' };
    case 'device.pair.list': return { requests: [] };
    case 'system.connections': return {entries:fixtureClients};
    case 'system.disconnect': {assert.equal(params.session_id,'fixture-client-session');assert.equal(params.host,undefined);fixtureClients=[];return {disconnected:true};}
    case 'logs.tail': return { lines: ['fixture log entry'], cursor: 1 };
    case 'skills.status': return { skills: [] };
    case 'skills.bins': return { bins: [{ name: 'Fixture skill', installed: true, enabled: skillEnabled, primary_env: 'SAVFOX_FIXTURE_KEY', description: 'Installed test skill' }] };
    case 'skills.update': skillEnabled = params.enabled; return { ok: true };
    case 'security.audit': return { findings: [], summary: { critical: 0 } };
    case 'security.rules.list': return { rules: [] };
    case 'exec.approvals.get': return { approvals: [] };
    case 'tts.status': return { enabled: false, provider: 'openai' };
    case 'tts.providers': return { providers: [] };
    case 'tts.voices': return { voices: [] };
    case 'voicewake.get': return voiceSettings;
    case 'voicewake.set': voiceSettings = { ...params }; return { ok: true };
    case 'talk.mode': return { mode: 'off' };
    case 'log.get_level': return { level: 'info' };
    case 'usage.status': return { total_tokens: 120, requests: 2 };
    case 'usage.cost': return { total_cost: 0.01 };
    case 'health': return { ok: true };
    default: return {};
  }
}
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.svg': 'image/svg+xml', '.ico': 'image/x-icon', '.png': 'image/png' };
const server = http.createServer((request, response) => {
  const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
  if (pathname === '/api/chat/completions') {
    request.resume();
    response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-store' });
    let index = 0;
    const timer = setInterval(() => {
      response.write(`data: ${JSON.stringify({ choices: [{ delta: { content: `Live fragment ${index++}\n\n` } }] })}\n\n`);
      if (index === 16) {
        clearInterval(timer);
        response.end('data: {"choices":[{"delta":{"content":"LIVE_END"}}]}\n\ndata: [DONE]\n\n');
      }
    }, 80);
    response.on('close', () => clearInterval(timer));
    return;
  }
  let file = path.resolve(bundle, '.' + pathname);
  if (file !== bundle && !file.startsWith(bundle + path.sep)) { response.writeHead(403); response.end(); return; }
  try { if (!fs.statSync(file).isFile()) file = path.join(bundle, 'index.html'); }
  catch { file = path.join(bundle, 'index.html'); }
  fs.readFile(file, (error, content) => {
    response.writeHead(error ? 404 : 200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' });
    response.end(error ? 'Missing frontend build' : content);
  });
});

(async () => {
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const base = `http://127.0.0.1:${server.address().port}`;
    report.base = base;
    browser = await chromium.launch({ headless: true });
    page = await browser.newPage({ locale: 'en-US', viewport: { width: 1440, height: 1000 } });
    page.on('pageerror', error => errors.push(error.stack || error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await page.route('**/api/token/validate', route => route.fulfill({ json: { valid: route.request().headers().authorization === `Bearer ${token}` } }));
    await page.route('**/api/chat/completions?*', async route => {
      const body = route.request().postDataJSON();
      calls.push({ method: 'http.chat', params: body });
      if (body.session_id === 'performance-session') {
        if (body.message?.content === 'Live tail test' || body.message?.content === 'Abort tail test') {
          await route.continue();
          return;
        }
        const event = delta => `data: ${JSON.stringify({ choices: [{ delta }] })}\n\n`;
        await route.fulfill({ contentType: 'text/event-stream', body: event({ reasoning_content: '批量推理' }) + Array.from({ length: 1000 }, () => event({ content: '批次' })).join('') + event({ content: '最终尾字' }) + 'data: [DONE]\n\n' });
        return;
      }
      await route.fulfill({ contentType: 'text/event-stream', body: 'data: {"choices":[{"delta":{"reasoning_content":"Thinking fixture"}}]}\n\ndata: {"choices":[{"delta":{"content":"Streamed browser reply"}}]}\n\ndata: [DONE]\n\n' });
    });
    await page.routeWebSocket('**/ws', socket => {
      connections++;
      currentSocket = socket;
      socket.onMessage(message => {
        const frame = JSON.parse(message);
        if (frame.type === 'connect') {
          const expected = crypto.createHmac('sha256', token).update('browser-challenge').digest('hex');
          if (frame.token !== expected) { errors.push('Invalid WebSocket challenge signature'); return; }
          socket.send(JSON.stringify({ type: 'connected', protocol: 1 }));
          return;
        }
        if (!frame.method) return;
        calls.push({ method: frame.method, params: frame.params });
        if (frame.method === 'agent.terminal.pty.write' && terminalFailNextWrite) {
          terminalFailNextWrite = false;
          socket.send(JSON.stringify({ jsonrpc: '2.0', id: frame.id, error: { code: -32603, message: 'Fixture terminal write failed' } }));
          return;
        }
        socket.send(JSON.stringify({ jsonrpc: '2.0', id: frame.id, result: fixture(frame.method, frame.params || {}) }));
      });
      socket.send(JSON.stringify({ type: 'connectChallenge', nonce: 'browser-challenge' }));
    });
    await page.goto(base);
    await page.getByLabel('Gateway token', { exact: true }).fill('invalid');
    await page.getByRole('button', { name: 'Connect', exact: true }).click();
    await page.getByText('Invalid token', { exact: true }).waitFor();
    await page.getByLabel('Gateway token', { exact: true }).fill(token);
    await page.getByRole('button', { name: 'Connect', exact: true }).click();
    await page.locator('.top-header__health--ok').waitFor();
    report.checks.push('invalid token rejected; valid token signed WebSocket challenge');
    if (process.argv.includes('--chat-performance')) {
      await page.goto(base + '/sessions?session_id=performance-session');
      const viewport = page.locator('.chat-messages > [data-gly-virtual="root"]');
      await page.getByText('Performance message 1999', { exact: true }).waitFor();
      assert(await page.locator('.chat-message').count() < 80, 'Only visible rows and overscan may be mounted');
      const lastRows = await page.locator('.chat-message').count();
      await viewport.evaluate(node => { node.scrollTop = 0; });
      await page.getByText('Performance message 0', { exact: true }).waitFor();
      await viewport.evaluate(node => { node.scrollTop = node.scrollHeight / 2; });
      await page.waitForFunction(() => [...document.querySelectorAll('.chat-message')].some(node => /Performance message [1-9]\d{2}/.test(node.textContent)));
      await page.waitForTimeout(250);
      const anchor = await viewport.evaluate(node => {
        window.__chatStableNode = document.querySelector('.chat-message .markdown-prose');
        return node.scrollTop;
      });
      await page.locator('.chat-composer textarea').fill('Performance streaming test');
      await page.locator('.chat-composer textarea').press('Enter');
      await page.waitForFunction(() => document.querySelector('.session-main-badge')?.textContent.includes('2002'));
      await page.waitForFunction(() => !document.querySelector('.chat-composer--busy'));
      assert(Math.abs(await viewport.evaluate(node => node.scrollTop) - anchor) < 3, 'Streaming must not pull a reader away from older messages');
      assert(await page.evaluate(() => window.__chatStableNode?.isConnected), 'Streaming must retain existing historical widgets');
      await viewport.evaluate(node => { node.scrollTop = node.scrollHeight; });
      await page.getByText('批次'.repeat(1000) + '最终尾字', { exact: true }).waitFor();
      const rows = await page.locator('.chat-message').count();
      assert(rows < 80, 'Appends must preserve bounded mounted rows');
      await page.getByRole('button', { name: 'Options', exact: true }).click();
      await page.getByRole('button', { name: 'Options', exact: true }).click();
      await page.locator('.chat-composer textarea').fill('Live tail test');
      await page.locator('.chat-composer textarea').press('Enter');
      await page.getByText('Live fragment 0', { exact: true }).waitFor();
      await page.getByText('LIVE_END', { exact: true }).waitFor();
      await page.waitForFunction(() => !document.querySelector('.chat-composer--busy'));
      await page.waitForFunction(() => {
        const node = document.querySelector('.chat-messages > [data-gly-virtual="root"]');
        return node.scrollHeight - node.clientHeight - node.scrollTop < 3;
      });
      await page.locator('.chat-composer textarea').fill('Abort tail test');
      await page.locator('.chat-composer textarea').press('Enter');
      await page.waitForFunction(() => document.querySelector('.session-main-badge')?.textContent.includes('2006'));
      await page.locator('.chat-message').last().getByText('Live fragment 1', { exact: true }).waitFor();
      await page.getByRole('button', { name: 'Stop generation', exact: true }).click();
      await page.waitForFunction(() => !document.querySelector('.chat-composer--busy'));
      const interrupted = await page.locator('.chat-message').last().innerText();
      assert(interrupted.includes('Live fragment 1') && !interrupted.includes('LIVE_END'));
      await page.waitForTimeout(200);
      assert.equal(await page.locator('.chat-message').last().innerText(), interrupted, 'Aborted timers must not append stale fragments');
      await page.setViewportSize({ width: 390, height: 844 });
      await page.waitForTimeout(250);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
      assert(await viewport.evaluate(node => node.clientHeight > 100), 'The mobile list must fill the available pane');
      await page.getByRole('button', { name: 'Clear', exact: true }).click();
      await page.getByText('Start a new session', { exact: true }).waitFor();
      assert.equal(await page.locator('.chat-message').count(), 0);
      report.checks.push(`2000 variable-height messages: ${lastRows} mounted at tail; ${rows} after append; head/middle/tail scrolling; stable reading anchor; 1000 UTF-8 fragments and final drain; panel resize; live streaming follows tail; abort drains and stops updates; mobile resize; clear`);
      assert.equal(errors.length, 0, errors.join('\n'));
      return;
    }
    await page.evaluate(() => { window.__savfoxBrowserSentinel = 'same-document'; });
    await page.getByRole('link', { name: 'Agents', exact: true }).click();
    await page.locator('.page-title').filter({ hasText: 'Agents' }).waitFor();
    assert.equal(await page.evaluate(() => window.__savfoxBrowserSentinel), 'same-document');
    const navigationConnections = connections;
    const navigationDocuments = [];
    const recordNavigationDocument = request => {
      if (request.resourceType() === 'document') navigationDocuments.push(request.url());
    };
    page.on('request', recordNavigationDocument);
    // Click the link at its actual text/icon coordinates. A centered click can
    // miss both children and conceal a native document navigation regression.
    async function clickNavigationPart(selector, part, options = {}) {
      const link = page.locator(selector);
      await link.scrollIntoViewIfNeeded();
      const linkBox = await link.boundingBox();
      const partBox = await link.locator(part).boundingBox();
      assert(linkBox && partBox, `Missing navigation target ${selector} ${part}`);
      await link.click({
        position: { x: partBox.x - linkBox.x + partBox.width / 2, y: partBox.y - linkBox.y + partBox.height / 2 },
        ...options,
      });
    }
    async function assertLiveNavigation(route) {
      await page.waitForURL(base + route);
      await page.locator('.page-title').first().waitFor();
      assert.equal(await page.evaluate(() => window.__savfoxBrowserSentinel), 'same-document', `Reload navigating to ${route}`);
      assert.equal(connections, navigationConnections, `WebSocket reconnected navigating to ${route}`);
      assert.deepEqual(navigationDocuments, [], 'Internal links must not request a new document');
    }
    for (const header of await page.locator('.sidebar-nav .nav-group > .sapid-collapsible-trigger').all()) {
      if (await header.getAttribute('aria-expanded') === 'false') await header.click();
    }
    const disclosure = page.locator('.sidebar-nav .nav-group > .sapid-collapsible-trigger').first();
    await disclosure.focus();
    await disclosure.press('Space');
    await page.waitForFunction(() => document.querySelector('.sidebar-nav .nav-group > .sapid-collapsible-trigger')?.getAttribute('aria-expanded') === 'false');
    assert.equal(await page.locator('.sidebar-nav a[href="/agents"]').isVisible(), false);
    await disclosure.press('Enter');
    await page.waitForFunction(() => document.querySelector('.sidebar-nav .nav-group > .sapid-collapsible-trigger')?.getAttribute('aria-expanded') === 'true');
    await page.locator('.sidebar-nav a[href="/agents"]').waitFor({ state: 'visible' });
    const navigationRoutes = await page.locator('.sidebar-nav a').evaluateAll(links => links.map(link => link.getAttribute('href')));
    for (const route of navigationRoutes) {
      for (const part of ['.nav-label', '.shell-icon']) {
        await clickNavigationPart(`.sidebar-nav a[href="${route}"]`, part);
        await assertLiveNavigation(route);
      }
    }
    await clickNavigationPart('.sidebar-nav a[href="/devices"]', '.nav-label');
    await assertLiveNavigation('/devices');
    await clickNavigationPart('.sidebar-nav a[href="/logs"]', '.shell-icon');
    await assertLiveNavigation('/logs');
    await page.goBack();
    await assertLiveNavigation('/devices');
    await page.goForward();
    await assertLiveNavigation('/logs');
    await page.locator('.sidebar-nav a[href="/devices"]').focus();
    await page.keyboard.press('Enter');
    await assertLiveNavigation('/devices');
    for (const part of ['.nav-label', '.shell-icon']) {
      const popupOpened = page.context().waitForEvent('page');
      await clickNavigationPart('.sidebar-nav a[href="/logs"]', part, { modifiers: ['Control'] });
      const popup = await popupOpened;
      await popup.waitForURL(base + '/logs');
      assert.equal(new URL(popup.url()).pathname, '/logs');
      await popup.close();
      await assertLiveNavigation('/devices');
    }
    await page.locator('.hamburger-btn').click();
    await clickNavigationPart('.sidebar-rail a[href="/logs"]', '.shell-icon');
    await assertLiveNavigation('/logs');
    await page.setViewportSize({ width: 390, height: 844 });
    await clickNavigationPart('.bottom-tab-bar a[href="/sessions"]', '.nav-label');
    await assertLiveNavigation('/sessions');
    await clickNavigationPart('.bottom-tab-bar a[href="/agents"]', '.shell-icon');
    await assertLiveNavigation('/agents');
    await page.locator('.bottom-tab-bar__more').click();
    await clickNavigationPart('.more-sheet a[href="/devices"]', '.nav-label');
    await assertLiveNavigation('/devices');
    await page.locator('.bottom-tab-bar__more').click();
    await clickNavigationPart('.more-sheet a[href="/logs"]', '.shell-icon');
    await assertLiveNavigation('/logs');
    await page.setViewportSize({ width: 1440, height: 1000 });
    page.off('request', recordNavigationDocument);
    report.checks.push('all navigation labels/icons preserve the document and WebSocket; history, keyboard, new tabs, rail and mobile menus');
    for (const route of ['/agents', '/agents/new', '/agents/browser-agent', '/agents/browser-agent/files', '/agents/browser-agent/tools', '/agents/browser-agent/skills', '/models', '/models/connect-provider', '/channels', '/channels/add', '/channels/edit/discord-test', '/channels/health/discord-test', '/channels/arkret', '/channels/whatsapp', '/channels/matrix', '/channels/nostr', '/cron', '/cron/new', '/config', '/config/gateway', '/config/execution', '/config/context', '/config/advanced', '/devices/connections', '/logs', '/usage', '/approvals', '/devices', '/devices/nodes/browser-node', '/config/diagnostics', '/skills', '/speech', '/speech/wake', '/devices/pair', '/config/interface', '/sessions?session_id=browser-session']) {
      await page.goto(base + route);
      await page.locator('.top-header__health--ok').waitFor();
      await page.locator('.page-title').first().waitFor();
      assert(!await page.getByText('Page not found', { exact: true }).count(), `Unmapped route ${route}`);
      report.routes.push(route);
      console.log(`Checked ${route}`);
    }
    await page.goto(base + '/channels');
    const channelMenu = page.getByRole('button', { name: 'Channel actions', exact: true }).first();
    await channelMenu.focus();
    await channelMenu.press('ArrowDown');
    const configureChannel = page.getByRole('menuitem', { name: 'Configure', exact: true });
    await configureChannel.waitFor({ state: 'visible' });
    assert(await configureChannel.evaluate(node => node === document.activeElement), 'Menu must focus its first action');
    await configureChannel.press('Escape');
    await configureChannel.waitFor({ state: 'hidden' });
    assert(await channelMenu.evaluate(node => node === document.activeElement), 'Escape must restore the channel action trigger');
    await channelMenu.press('Enter');
    await configureChannel.press('Enter');
    await page.waitForURL(base + '/channels/edit/discord-test');
    report.checks.push('Sapid disclosure keyboard state; channel menu keyboard selection and Escape focus restoration');
    await page.goto(base + '/settings');
    await page.getByText('Page not found', {exact:true}).waitFor();
    assert.equal(await page.locator('.sidebar a[href="/settings"]').count(),0);
    await page.goto(base + '/config/interface');
    const browserSave=page.getByRole('button',{name:'Save configuration',exact:true});
    await browserSave.waitFor();
    const themeChoice=page.getByRole('combobox',{name:'Theme',exact:true});
    await themeChoice.click(); await page.getByRole('option',{name:'Light',exact:true}).click();
    await page.getByRole('button',{name:'Gateway and security',exact:true}).click();
    const browserUrl=page.getByLabel('Gateway WebSocket URL (empty uses this host)',{exact:true});
    await browserUrl.fill('https://invalid.example');
    await page.getByText('Gateway URL must start with ws:// or wss://',{exact:true}).waitFor();
    assert.equal(await browserSave.isDisabled(),true);
    await browserUrl.fill('wss://gateway.example.test/ws');
    await page.getByRole('button',{name:'Sessions and context',exact:true}).click();
    const defaultModel=page.getByRole('combobox',{name:'Default model for new chats (this browser)',exact:true});
    await defaultModel.click(); await page.getByRole('option',{name:'Test model',exact:true}).click();
    const beforeBrowserSave=calls.filter(call=>call.method==='config.set').length;
    await browserSave.click();
    await page.getByText('Browser settings saved. Reload the page to connect to the new gateway.',{exact:true}).waitFor();
    assert.equal(calls.filter(call=>call.method==='config.set').length,beforeBrowserSave,'Browser-only saves must not write server configuration');
    assert.equal(await page.evaluate(()=>localStorage.getItem('savfox_theme')),'light');
    assert.equal(await page.evaluate(()=>localStorage.getItem('savfox_default_model')),'openai/test-model');
    await page.getByRole('button',{name:'Interface and notifications',exact:true}).click();
    await themeChoice.click(); await page.getByRole('option',{name:'Dark',exact:true}).click();
    await page.getByRole('button',{name:'Discard changes',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('[role="combobox"][aria-label="Theme"]')?.textContent.includes('Light'));
    assert((await themeChoice.innerText()).includes('Light'),'Discard must restore browser options too');
    await page.getByRole('combobox',{name:'Language',exact:true}).click();
    await page.getByRole('option',{name:'简体中文',exact:true}).click();
    await browserSave.click();
    await page.getByText('浏览器选项已保存。',{exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>localStorage.getItem('savfox_locale')),'zh-CN');
    await page.getByRole('button',{name:'EN',exact:true}).click();
    // Restore this-host connection before reloading; the test gateway is deliberately local.
    await page.getByRole('button',{name:'Gateway and security',exact:true}).click();
    await browserUrl.fill(''); await browserSave.click();
    await page.getByText('Browser settings saved. Reload the page to connect to the new gateway.',{exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>localStorage.getItem('savfox_gateway_url')),'');
    await page.reload();
    await page.getByRole('button',{name:'Interface and notifications',exact:true}).click();
    assert((await themeChoice.innerText()).includes('Light'),'Saved browser theme must survive reload');
    await page.getByRole('button',{name:'Gateway and security',exact:true}).click();
    await page.getByLabel('Search configuration fields',{exact:true}).fill('Desktop notifications');
    await page.getByRole('checkbox',{name:'Desktop notifications',exact:true}).waitFor();
    await page.getByLabel('Search configuration fields',{exact:true}).fill('');
    report.checks.push('browser options grouped in system configuration; retired preferences route; shared save/discard/search; local persistence without server writes');
    await page.goto(base + '/config/interface');
    const saveConfiguration=page.getByRole('button',{name:'Save configuration',exact:true});
    await themeChoice.waitFor();
    assert.equal(await saveConfiguration.isDisabled(),true);
    await themeChoice.click(); await page.getByRole('option',{name:'Dark',exact:true}).click();
    await page.getByRole('button',{name:'Gateway and security',exact:true}).click();
    assert.equal(await page.getByRole('button',{name:'All settings',exact:true}).count(),0);
    await page.getByLabel('Port',{exact:true}).fill('18882');
    await saveConfiguration.click();
    await waitForCall(call=>call.method==='config.set' && call.params.config.gateway.port===18882);
    await page.getByText('Configuration saved. Restart the gateway to activate changes. Browser settings saved.',{exact:true}).waitFor();
    assert.equal(calls.filter(call=>call.method==='config.set').length,beforeBrowserSave+1,'Mixed changes must send one server write');
    assert.equal(calls.find(call=>call.method==='config.set').params.config.browser,undefined,'Browser options must stay out of the server document');
    assert.equal(await page.evaluate(()=>localStorage.getItem('savfox_theme')),'dark');
    assert.equal(await saveConfiguration.isDisabled(),true);
    assert.equal(calls.some(call=>call.method==='config.patch'||call.method==='config.apply'),false,'Saving must send one replacement, without a second apply');
    await page.getByRole('button',{name:'Advanced configuration',exact:true}).click();
    const rawConfig=page.getByLabel('System configuration JSON',{exact:true});
    await rawConfig.fill('{ invalid json');
    assert.equal(await saveConfiguration.isDisabled(),true);
    const imported=structuredClone(fixtureConfig); delete imported.model_context_window; imported.gateway.port=18883;
    await rawConfig.fill(JSON.stringify(imported));
    await saveConfiguration.click();
    await waitForCall(call=>call.method==='config.set' && call.params.config.gateway.port===18883 && !('model_context_window' in call.params.config));
    await page.reload();
    await rawConfig.waitFor();
    await page.waitForFunction(()=>{try{return JSON.parse(document.querySelector('textarea[aria-label="System configuration JSON"]')?.value || '{}').gateway?.port===18883;}catch{return false;}});
    assert.equal(JSON.parse(await rawConfig.inputValue()).gateway.port,18883);
    await page.getByRole('button',{name:'Execution and tools',exact:true}).click();
    const mcp=page.getByLabel('Mcp servers',{exact:true});
    await mcp.fill('{"fixture":{"command":"server"}}');
    await saveConfiguration.click();
    await waitForCall(call=>call.method==='config.set' && call.params.config.mcp_servers?.fixture?.command==='server');
    await page.getByRole('button',{name:'Runtime environment',exact:true}).click();
    const inherited=page.getByRole('combobox',{name:'Check for update on startup',exact:true});
    assert((await inherited.innerText()).includes('Use default'),'Unset optional boolean must not be displayed as disabled');
    await inherited.click(); await page.getByRole('option',{name:'Enabled',exact:true}).click();
    await saveConfiguration.click();
    await waitForCall(call=>call.method==='config.set' && call.params.config.check_for_update_on_startup===true);
    await inherited.click(); await page.getByRole('option',{name:'Use default',exact:true}).click();
    await saveConfiguration.click();
    await waitForCall(call=>call.method==='config.set' && call.params.revision==='5' && !('check_for_update_on_startup' in call.params.config));
    await page.getByRole('button',{name:'Gateway and security',exact:true}).click();
    assert.equal(await page.getByRole('checkbox',{name:'Enabled',exact:true}).isChecked(),true,'A missing setting must display its actual schema default');
    await page.getByLabel('Search configuration fields',{exact:true}).fill('port');
    await page.getByLabel('Port',{exact:true}).waitFor();
    await page.goto(base + '/devices/connections');
    await page.getByText('Fixture client',{exact:true}).waitFor();
    page.once('dialog',dialog=>dialog.accept());
    await page.getByRole('button',{name:'Disconnect',exact:true}).click();
    await waitForCall(call=>call.method==='system.disconnect' && call.params.session_id==='fixture-client-session');
    await page.getByText('Fixture client',{exact:true}).waitFor({state:'hidden'});
    report.checks.push('configuration save once, restart feedback, JSON validation/import/deletion, persistent reload, field search and client disconnect identity');
    await page.goto(base + '/speech/wake');
    const wake = page.getByRole('checkbox', { name: 'Wake word enabled', exact: true });
    await wake.waitFor();
    assert.equal(await wake.evaluate(input => input.closest('.sapid-checkbox')?.classList.contains('sapid-unstyled')), false, 'Checkbox must retain Sapid styling');
    await page.getByText('Wake word enabled', { exact: true }).click();
    assert.equal(await wake.isChecked(), true, 'Clicking the checkbox label must toggle its value');
    await wake.focus();
    await wake.press('Space');
    assert.equal(await wake.isChecked(), false, 'Checkbox must support keyboard toggling');
    await wake.press('Space');
    await page.getByLabel('Wake word', { exact: true }).fill('browser wake word');
    await page.getByRole('button', { name: 'Save wake settings', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('input[aria-label="Wake word"]')?.value === 'browser wake word');
    await waitForCall(call => call.method === 'voicewake.set' && call.params.enabled && call.params.keyword === 'browser wake word');
    await page.goto(base + '/models');
    const visibility = page.getByRole('switch', { name: 'Visible in model picker', exact: true }).first();
    await visibility.waitFor();
    const previousVisibility = await visibility.isChecked();
    const storedPreferences = await page.evaluate(() => localStorage.getItem('savfox_model_preferences_v2'));
    await visibility.press('Space');
    assert.equal(await visibility.isChecked(), !previousVisibility);
    await page.waitForFunction(previous => localStorage.getItem('savfox_model_preferences_v2') !== previous, storedPreferences);
    await page.reload();
    await visibility.waitFor();
    assert.equal(await visibility.isChecked(), !previousVisibility, 'Model switch must persist the preference');
    await page.goto(base + '/skills');
    const skillSwitch = page.getByRole('switch', { name: 'Enabled', exact: true });
    await skillSwitch.waitFor();
    await skillSwitch.click();
    await page.waitForFunction(() => document.querySelector('.skill-card .sapid-switch input')?.checked === false);
    await waitForCall(call => call.method === 'skills.update' && call.params.name === 'Fixture skill' && call.params.enabled === false);
    report.checks.push('Sapid checkbox label/keyboard activation and save RPC; semantic model/skill switches and persistence');
    await page.locator('.skill-card .sapid-collapsible>button').click();
    const credential = page.getByLabel('SAVFOX_FIXTURE_KEY', { exact: true });
    await credential.waitFor({ state: 'visible' });
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 1000 });
      await credential.scrollIntoViewIfNeeded();
      const layout = await credential.evaluate(input => {
        const group = input.closest('.sapid-text-field-editor');
        const toggle = group.querySelector('button');
        const card = input.closest('.skill-card').getBoundingClientRect();
        const a = input.getBoundingClientRect(), b = toggle.getBoundingClientRect();
        const save = input.closest('.actions').querySelector(':scope > button').getBoundingClientRect();
        return { aligned: Math.abs(a.y + a.height / 2 - b.y - b.height / 2) <= 1, contained: a.width >= 40 && a.x >= card.x && b.right <= a.right && b.y >= a.y && b.bottom <= a.bottom && save.right <= card.right, overflow: document.documentElement.scrollWidth > innerWidth + 1, input: a.toJSON(), toggle: b.toJSON(), card: card.toJSON(), save: save.toJSON() };
      });
      assert(layout.aligned && layout.contained && !layout.overflow, `Expanded skill credentials must fit and align at ${width}px: ${JSON.stringify(layout)}`);
      await page.screenshot({ path: path.join(cache, `skills-credentials-${width}.png`) });
    }
    await page.setViewportSize({ width: 1440, height: 1000 });
    report.checks.push('expanded skill credentials and visibility action fit desktop/mobile layouts');
    await page.goto(base + '/usage');
    await page.locator('.top-header__health--ok').waitFor();
    const period = page.getByRole('combobox', { name: 'Period', exact: true });
    assert.equal(await period.evaluate(element => element.tagName), 'BUTTON', 'Period must use the themed Sapid listbox rather than an OS popup');
    assert((await period.innerText()).includes('7 days'));
    await period.click();
    await page.getByRole('listbox').waitFor({ state: 'visible' });
    await page.getByRole('option', { name: 'Today', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.usage-cost-hero__period')?.textContent.includes('Today'));
    assert(calls.some(call => call.method === 'usage.cost' && call.params.period === 'today'), 'Selecting a period must refresh the matching usage data');
    await period.press('ArrowDown');
    await period.press('End');
    await page.waitForFunction(() => {
      const trigger = document.querySelector('.usage-page .sapid-select');
      const current = document.getElementById(trigger?.getAttribute('aria-activedescendant'));
      return current?.textContent === 'All time' && getComputedStyle(current).backgroundColor !== 'rgba(0, 0, 0, 0)' && current.getAttribute('aria-selected') === 'false';
    });
    await period.press('Enter');
    await page.waitForFunction(() => document.querySelector('.usage-cost-hero__period')?.textContent.includes('All time'));
    assert(calls.some(call => call.method === 'usage.cost' && call.params.period === 'all'), 'Keyboard selection must update the application value');
    await period.click();
    await period.press('Escape');
    await page.waitForFunction(() => document.querySelector('.usage-page .sapid-select')?.getAttribute('aria-expanded') === 'false');
    assert(await period.evaluate(element => document.activeElement === element), 'Escape must retain focus on the select trigger');
    report.checks.push('themed dropdown pointer/keyboard selection; usage RPC period mapping; active option highlight; Escape focus');
    await page.goto(base + '/sessions?session_id=browser-session');
    await page.locator('.top-header__health--ok').waitFor();
    await page.getByText('History restored', { exact: true }).waitFor();
    const strong = page.locator('.chat-message.assistant .markdown-strong').filter({ hasText: 'Fixture strong' });
    await strong.waitFor();
    assert(await strong.evaluate(element => Number(getComputedStyle(element).fontWeight) >= 600), 'Markdown strong text must render bold');
    await page.locator('.chat-message.assistant .markdown-paragraph').filter({ hasText: '- Fixture list item' }).waitFor();
    await page.locator('.chat-message.assistant .markdown-code-block .markdown-code').filter({ hasText: 'const answer = 42;' }).waitFor();
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
    const copyCode = page.locator('.chat-message.assistant .sapid-copy-button').first();
    await copyCode.click();
    await page.waitForFunction(() => navigator.clipboard.readText().then(text => text === 'const answer = 42;\n'));
    await copyCode.filter({ hasText: 'Copied' }).waitFor();
    report.checks.push('Sapid code block copies the full source with clipboard completion feedback');
    const historyImage = page.locator('.chat-message-attachment[alt="fixture-preview.png"]');
    await historyImage.waitFor({ state: 'visible' });
    await page.waitForFunction(() => {
      const image = document.querySelector('.chat-message-attachment[alt="fixture-preview.png"]');
      return image?.complete && image.naturalWidth === 1 && image.naturalHeight === 1;
    });
    assert((await historyImage.getAttribute('src')).startsWith('data:image/png;base64,'), 'History image must retain its MIME type and encoded data');
    await page.screenshot({ path: path.join(cache, 'history.png'), fullPage: false });
    report.checks.push('nested route refresh; rich Markdown history; image attachment preview');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    await page.getByRole('combobox', { name: 'Reasoning display', exact: true }).waitFor();
    assert.equal(await page.locator('.session-terminal-body').isVisible(), false, 'Native agents must not offer terminal execution');
    await page.getByText('This session uses a native agent and has no interactive terminal.', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'View ambient context', exact: true }).click();
    await page.getByText('No buffered context messages.', { exact: true }).waitFor();
    assert.deepEqual(calls.findLast(call => call.method === 'sessions.ambient.get').params, { session_id: 'browser-session' });
    await page.getByRole('button', { name: 'View idle reply', exact: true }).click();
    await page.getByText('No pending idle reply.', { exact: false }).waitFor();
    assert.equal(await page.locator('.session-delivery-body').isVisible(), false, 'Unbound sessions must not offer publication');
    await page.goto(base + '/sessions');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    assert.equal(await page.locator('.session-terminal-body').isVisible(), false);
    assert.equal(await page.getByRole('button', { name: 'View ambient context', exact: true }).isDisabled(), true);
    assert.equal(await page.getByRole('button', { name: 'View idle reply', exact: true }).isDisabled(), true);
    assert.equal(await page.locator('.session-main > .error-message').innerText(), '');
    await page.screenshot({ path: path.join(cache, 'session-options-draft.png'), fullPage: false });
    await page.goto(base + '/sessions?session_id=terminal-session');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    const terminalStart = page.getByRole('button', { name: 'Start terminal', exact: true });
    await page.waitForFunction(() => ![...document.querySelectorAll('.session-option-button')].find(el => el.textContent === 'Start terminal')?.disabled);
    assert.equal(await page.getByRole('button', { name: 'Send line', exact: true }).isDisabled(), true);
    await terminalStart.click();
    await page.locator('.session-terminal-output').filter({ hasText: 'Fixture terminal ready' }).waitFor();
    const terminalInput = page.getByRole('textbox', { name: 'Terminal input', exact: true });
    await terminalInput.fill('ls');
    await page.getByRole('button', { name: 'Send line', exact: true }).click();
    await page.locator('.session-terminal-output').filter({ hasText: 'Executed: ls' }).waitFor();
    assert.equal(await terminalInput.inputValue(), '');
    assert(calls.some(call => call.method === 'agent.terminal.pty.write' && call.params.kind === 'line' && call.params.text === 'ls' && call.params.agent === 'terminal-agent' && call.params.session_id === 'terminal-session'));
    terminalFailNextWrite = true;
    await terminalInput.fill('pwd');
    await page.getByRole('button', { name: 'Send line', exact: true }).click();
    await page.locator('.session-option-card--terminal .error-message').filter({ hasText: 'Fixture terminal write failed' }).waitFor();
    assert.equal(await terminalInput.inputValue(), 'pwd', 'A failed write must preserve terminal input');
    await page.getByRole('button', { name: 'Send line', exact: true }).click();
    await page.locator('.session-terminal-output').filter({ hasText: 'Executed: pwd' }).waitFor();
    await page.getByRole('button', { name: 'Interrupt', exact: true }).click();
    await page.locator('.session-terminal-output').filter({ hasText: 'Interrupted' }).waitFor();
    assert.equal((await page.locator('.session-terminal-output').innerText()).split('Fixture terminal ready').length - 1, 1, 'Incremental reads must not duplicate output');
    await page.screenshot({ path: path.join(cache, 'session-options-terminal.png'), fullPage: false });
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    const readsAfterClosing = calls.filter(call => call.method === 'agent.terminal.pty.read').length;
    await page.waitForTimeout(1300);
    assert.equal(calls.filter(call => call.method === 'agent.terminal.pty.read').length, readsAfterClosing, 'Closing options must retire terminal polling');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    await page.locator('.session-terminal-output').filter({ hasText: 'Executed: pwd' }).waitFor();
    page.once('dialog', dialog => dialog.accept());
    await page.getByRole('button', { name: 'Close terminal', exact: true }).click();
    await page.locator('.session-terminal-status').filter({ hasText: 'Not running' }).waitFor();
    assert.equal(await page.getByRole('button', { name: 'Send line', exact: true }).isDisabled(), true);
    await page.goto(base + '/sessions?session_id=delivery-session');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    const publicSummary = page.getByRole('textbox', { name: 'Public checkpoint summary', exact: true });
    await publicSummary.waitFor();
    const publishCheckpoint = page.getByRole('button', { name: 'Publish checkpoint', exact: true });
    assert.equal(await publishCheckpoint.isDisabled(), true);
    await publicSummary.fill('First reviewed progress');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    assert.equal(await publicSummary.inputValue(), 'First reviewed progress', 'Closing options must preserve the public summary draft');
    await page.getByRole('button', { name: 'Preview checkpoint', exact: true }).click();
    await page.locator('.session-delivery-preview').filter({ hasText: 'Reviewed checkpoint: First reviewed progress' }).waitFor();
    assert.equal(await publishCheckpoint.isDisabled(), false);
    await publicSummary.fill('Updated reviewed progress');
    await page.getByText('Summary changed. Preview again before publishing.', { exact: true }).waitFor();
    assert.equal(await publishCheckpoint.isDisabled(), true);
    await page.getByRole('button', { name: 'Preview checkpoint', exact: true }).click();
    await page.locator('.session-delivery-preview').filter({ hasText: 'Reviewed checkpoint: Updated reviewed progress' }).waitFor();
    page.once('dialog', dialog => dialog.accept());
    await publishCheckpoint.click();
    await page.getByText('Checkpoint published.', { exact: true }).waitFor();
    assert.equal(await publishCheckpoint.isDisabled(), true, 'Repeated publication requires a fresh preview');
    assert.equal(calls.findLast(call => call.method === 'sessions.arkret.delivery.publish').params.summary, 'Updated reviewed progress');
    for (const width of [390, 1440]) {
      await page.setViewportSize({ width, height: 900 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, 'Session options must fit the viewport');
    }
    report.checks.push('labeled reasoning settings; session and agent prerequisites; managed terminal lifecycle, incremental output, local errors and polling cleanup; reviewed Arkret publication');
    await page.goto(base + '/sessions');
    await page.locator('.top-header__health--ok').waitFor();
    await page.locator('.chat-composer textarea').fill('Browser streaming test');
    await page.getByRole('button', { name: '中文', exact: true }).click();
    assert.equal(await page.locator('.chat-composer textarea').inputValue(), 'Browser streaming test');
    await page.getByRole('link', { name: '会话', exact: true }).waitFor();
    await page.getByRole('textbox', { name: '消息', exact: true }).waitFor();
    assert.equal(await page.locator('.chat-composer textarea').inputValue(), 'Browser streaming test');
    await page.getByRole('button', { name: 'EN', exact: true }).click();
    await page.getByRole('link', { name: 'Sessions', exact: true }).waitFor();
    await page.getByRole('textbox', { name: 'Message', exact: true }).waitFor();
    assert.equal(await page.locator('.chat-composer textarea').inputValue(), 'Browser streaming test');
    report.checks.push('changing language preserves the active draft');
    const prompt = page.locator('.chat-composer textarea');
    await prompt.focus();
    await page.keyboard.press('Control+k');
    await page.getByRole('dialog').waitFor();
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === 'Search commands');
    await page.keyboard.press('Escape');
    await page.getByRole('dialog').waitFor({ state: 'hidden' });
    await page.waitForFunction(() => document.activeElement === document.querySelector('.chat-composer textarea'), null, { timeout: 1000 });
    assert(await prompt.evaluate(element => document.activeElement === element), 'Palette dismissal must restore the composer focus');
    report.checks.push('command palette keyboard activation and focus restoration');
    const beforeReconnect = connections;
    currentSocket.close({ code: 1012, reason: 'Fixture restart' });
    for (let attempt = 0; attempt < 250 && connections === beforeReconnect; attempt++) await new Promise(resolve => setTimeout(resolve, 20));
    assert(connections > beforeReconnect, 'WebSocket must reconnect after a gateway restart');
    await page.locator('.top-header__health--ok').waitFor();
    assert.equal(await prompt.inputValue(), 'Browser streaming test');
    report.checks.push('authenticated WebSocket reconnect preserves the active draft');
    await page.locator('.chat-composer textarea').press('Enter');
    await page.getByText('Streamed browser reply', { exact: true }).waitFor();
    assert(calls.some(call => call.method === 'http.chat' && call.params.session_id === 'browser-new-session'));
    report.checks.push('new session creation, prompt send, SSE reasoning and response');
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    const reasoningDisplay = page.getByRole('combobox', { name: 'Reasoning display', exact: true });
    await reasoningDisplay.click();
    await page.getByRole('option', { name: 'Hidden', exact: true }).click();
    await page.waitForFunction(() => [...document.querySelectorAll('.message-thinking')].every(el => !el.textContent));
    await reasoningDisplay.click();
    await page.getByRole('option', { name: 'After reply', exact: true }).click();
    await page.waitForFunction(() => [...document.querySelectorAll('.message-thinking')].some(el => el.textContent.includes('Thinking fixture')));
    await page.getByRole('button', { name: 'Options', exact: true }).click();
    report.checks.push('reasoning display toggles completed streamed content without losing it');
    currentSocket.send(JSON.stringify({ type: 'event', event: 'exec.approval.requested', payload: { request_id: 'fixture-approval', nonce: 'fixture-nonce', command: 'test command', session_id: 'browser-new-session' } }));
    await page.getByRole('button', { name: 'Approve once', exact: true }).click();
    await page.getByText('Execution approval required', { exact: true }).waitFor({ state: 'hidden' });
    assert(calls.some(call => call.method === 'exec.approval.resolve' && call.params.request_id === 'fixture-approval' && call.params.nonce === 'fixture-nonce'));
    await page.getByText('Streamed browser reply', { exact: true }).waitFor();
    report.checks.push('gateway Event notification resolves inline approval without retiring chat');
    currentSocket.send(JSON.stringify({ type: 'event', event: 'claude.question', payload: { request_id: 'fixture-question', nonce: 'question-nonce', questions: [{ question: 'Choose environment', options: [{ label: 'Dev' }, { label: 'Prod' }] }] } }));
    await page.getByRole('button', { name: 'Dev', exact: true }).click();
    await page.getByRole('button', { name: 'Send answers', exact: true }).click();
    await page.getByText('Answer required', { exact: true }).waitFor({ state: 'hidden' });
    assert(calls.some(call => call.method === 'exec.approval.resolve' && call.params.request_id === 'fixture-question' && call.params.answers['0'] === 'Dev'));
    report.checks.push('inline questionnaire sends numeric question answers');
    await page.screenshot({ path: path.join(cache, 'chat.png'), fullPage: true });
    await page.goto(base + '/models/connect-provider');
    await page.getByRole('button', { name: 'Sign in with device code', exact: true }).click();
    await page.getByText('TEST-CODE', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Cancel sign-in', exact: true }).click();
    await page.getByText('Sign-in cancelled', { exact: true }).waitFor();
    await page.getByRole('button', { name: 'Sign in with ChatGPT', exact: true }).click();
    await page.getByRole('link', { name: 'Open sign-in page', exact: true }).waitFor();
    const cancellations = calls.filter(call => call.method === 'account/login/cancel').length;
    await page.getByRole('link', { name: 'Agents', exact: true }).click();
    for (let attempt = 0; attempt < 100 && calls.filter(call => call.method === 'account/login/cancel').length === cancellations; attempt++) await new Promise(resolve => setTimeout(resolve, 20));
    assert(calls.filter(call => call.method === 'account/login/cancel').length > cancellations, 'Navigation must cancel the server login worker');
    report.checks.push('route retirement cancels pending OpenAI sign-in');
    await page.goto(base + '/models/connect-provider');
    await page.getByRole('button', { name: 'Sign in with ChatGPT', exact: true }).click();
    await page.getByRole('link', { name: 'Open sign-in page', exact: true }).waitFor();
    signInComplete = true;
    await page.getByRole('button', { name: 'Check sign-in status', exact: true }).click();
    await page.getByText('Signed in successfully', { exact: false }).waitFor();
    report.checks.push('OpenAI device-code cancel and browser sign-in status');
    await page.goto(base + '/agents');
    await page.getByText(agentName, { exact: true }).waitFor();
    agentName = 'Updated browser agent';
    await page.getByRole('button', { name: 'Refresh', exact: true }).click();
    await page.getByText(agentName, { exact: true }).waitFor();
    report.checks.push('same-id record refresh updates rendered content');
    await page.goto(base + '/channels/edit/discord-test');
    const botToken = page.getByLabel('Bot Token', { exact: true });
    await page.waitForFunction(expected => document.querySelector('input[aria-label="Bot Token"]')?.value === expected, redactedChannelSecret);
    assert.equal(await botToken.getAttribute('type'), 'password', 'Channel credentials must remain masked');
    await botToken.fill('browser-channel-secret');
    await page.getByLabel('Channel name', { exact: true }).fill('Renamed browser Discord');
    const channelSaveStart = calls.length;
    await page.getByRole('button', { name: 'Save channel', exact: true }).click();
    await page.getByText('Channel saved', { exact: true }).waitFor();
    const channelSave = calls.slice(channelSaveStart).find(call => call.method === 'channels.config.save');
    assert(channelSave, 'Channel editor did not save over JSON-RPC');
    assert.equal(channelSave.params.id, 'discord-test', 'Renaming a channel must preserve the instance id');
    assert.equal(channelSave.params.channel, 'discord');
    assert.equal(channelSave.params.config.bot_token, 'browser-channel-secret');
    assert.equal(await botToken.getAttribute('type'), 'password');
    await page.goto(base + '/channels/health/discord-test');
    await page.locator('.top-header__health--ok').waitFor();
    await page.locator('.page-title').waitFor();
    await page.waitForFunction(() => document.querySelector('.json-output')?.textContent.includes('discord'));
    assert(calls.some(call => call.method === 'channels.status' && call.params.channel === 'discord' && call.params.id === 'discord-test'));
    report.checks.push('masked channel credentials; editor saves with stable id; per-instance health routing');
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({ path: path.join(cache, 'mobile.png'), fullPage: true });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, 'Mobile horizontal overflow');
    await page.keyboard.press('Control+k');
    await page.getByRole('dialog').waitFor();
    await page.getByRole('combobox', { name: 'Search commands', exact: true }).fill('Sign out');
    await page.getByRole('option', { name: 'Sign out', exact: true }).click();
    await page.getByLabel('Gateway token', { exact: true }).waitFor();
    assert.equal(await page.evaluate(() => localStorage.getItem('savfox_token')), null);
    report.checks.push('mobile layout; sign-out clears auth and retires subscriptions');
    assert.equal(errors.length, 0, errors.join('\n'));
  } catch (error) {
    errors.push(error.stack || String(error)); process.exitCode = 1;
    if (page) {
      await page.screenshot({ path: path.join(cache, 'failure.png'), fullPage: true }).catch(() => {});
      fs.writeFileSync(path.join(cache, 'failure.html'), await page.content().catch(() => ''));
    }
  }
  finally {
    if (browser) await browser.close();
    server.close();
    report.calls = calls;
    fs.writeFileSync(path.join(cache, 'report.json'), JSON.stringify(report, null, 2));
    console.log(JSON.stringify({ routes: report.routes.length, checks: report.checks, errors }, null, 2));
  }
})();
