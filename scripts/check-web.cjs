#!/usr/bin/env node
// Browser regression test against the generated bundle and deterministic gateway fixtures.
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const root = path.resolve(__dirname, '..');
const cache = process.env.SAVFOX_WEB_CHECK_CACHE || (process.platform === 'win32' ? 'D:/BuildCache/savfox-glory/browser' : '/mnt/d/BuildCache/savfox-glory/browser');
fs.mkdirSync(cache, { recursive: true });
process.env.TEMP = process.env.TMP = cache;
const { chromium } = require(process.env.SAVFOX_PLAYWRIGHT || path.resolve(root, '../../glory-rs/glory/tests/playwright/node_modules/playwright'));
const bundle = path.resolve(process.env.SAVFOX_WEB_BUNDLE || path.resolve(cache, '../web/debug'));
const token = 'browser-test-token';
const calls = [];
const errors = [];
const report = { routes: [], checks: [], errors };
let browser;
let page;
let currentSocket;
let connections = 0;
let agentName = 'Browser agent';
let signInComplete = false;
const agent = () => ({ id: 'browser-agent', name: agentName, kind: 'native', native: { provider: 'openai', model: 'test-model' } });
const redactedChannelSecret = '__SAVFOX_CHANNEL_SECRET_REDACTED__';
const channel = { id: 'discord-test', kind: 'discord', name: 'Browser Discord', enabled: true, config: { bot_token: redactedChannelSecret, mode: 'stream' } };
let channelRunning = true;
const model = { id: 'openai/test-model', name: 'Test model', provider: 'openai', model_slug: 'test-model' };
function fixture(method, params) {
  switch (method) {
    case 'status': return { version: 'test', uptime_ms: 1000, connected_clients: 1 };
    case 'agents.list': return { agents: [agent()] };
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
    case 'sessions.list': return { entries: [{ session_id: 'browser-session', label: 'Browser conversation', agent_id: 'default', message_count: 1 }] };
    case 'sessions.patch': return { session_id: params.session_id || 'browser-new-session' };
    case 'chat.history': return { messages: [{ role: 'user', content: 'Earlier message', attachments: [{ filename: 'fixture-preview.png', mime_type: 'image/png', data_base64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aFtwAAAAASUVORK5CYII=' }] }, { role: 'assistant', content: 'History restored\n\n**Fixture strong**\n\n- Fixture list item\n\n```js\nconst answer = 42;\n```' }] };
    case 'sessions.ambient.get': return { session_id: params.session_id, messages: [] };
    case 'sessions.idle_reply.get': return { enabled: false };
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
    case 'config.get': return { config: { model: { provider: 'openai', slug: 'test-model' }, gateway: { port: 18881 } }, hash: 'fixture' };
    case 'config.schema': return { schema: { type: 'object', properties: { gateway: { type: 'object', properties: { port: { type: 'integer' } } } } } };
    case 'cron.list': return { jobs: [] };
    case 'cron.status': return { enabled: true };
    case 'cron.runs': return { runs: [] };
    case 'node.list': return { nodes: [] };
    case 'node.describe': return { id: 'browser-node', name: 'Browser node' };
    case 'device.pair.list': return { requests: [] };
    case 'system-presence': return { entries: [] };
    case 'logs.tail': return { lines: ['fixture log entry'], cursor: 1 };
    case 'skills.status': return { skills: [] };
    case 'skills.bins': return { bins: [] };
    case 'security.audit': return { findings: [], summary: { critical: 0 } };
    case 'security.rules.list': return { rules: [] };
    case 'exec.approvals.get': return { approvals: [] };
    case 'tts.status': return { enabled: false, provider: 'openai' };
    case 'tts.providers': return { providers: [] };
    case 'tts.voices': return { voices: [] };
    case 'voicewake.get': return { enabled: false, triggers: ['savfox'] };
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
    await page.evaluate(() => { window.__savfoxBrowserSentinel = 'same-document'; });
    await page.getByRole('link', { name: 'Agents', exact: true }).click();
    await page.locator('.page-title').filter({ hasText: 'Agents' }).waitFor();
    assert.equal(await page.evaluate(() => window.__savfoxBrowserSentinel), 'same-document');
    report.checks.push('sidebar navigation preserves the live app and WebSocket');
    for (const route of ['/agents', '/agents/new', '/agents/browser-agent', '/agents/browser-agent/files', '/agents/browser-agent/tools', '/agents/browser-agent/skills', '/models', '/models/connect-provider', '/channels', '/channels/add', '/channels/edit/discord-test', '/channels/health/discord-test', '/channels/arkret', '/channels/whatsapp', '/channels/matrix', '/channels/nostr', '/cron', '/cron/new', '/config', '/config/gateway', '/instances', '/logs', '/usage', '/approvals', '/nodes', '/nodes/browser-node', '/debug', '/skills', '/tts', '/voice', '/device-pair', '/settings', '/sessions?session_id=browser-session']) {
      await page.goto(base + route);
      await page.locator('.top-header__health--ok').waitFor();
      await page.locator('.page-title').first().waitFor();
      assert(!await page.getByText('Page not found', { exact: true }).count(), `Unmapped route ${route}`);
      report.routes.push(route);
      console.log(`Checked ${route}`);
    }
    await page.getByText('History restored', { exact: true }).waitFor();
    const strong = page.locator('.chat-message.assistant .markdown-strong').filter({ hasText: 'Fixture strong' });
    await strong.waitFor();
    assert(await strong.evaluate(element => Number(getComputedStyle(element).fontWeight) >= 600), 'Markdown strong text must render bold');
    await page.locator('.chat-message.assistant .markdown-paragraph').filter({ hasText: '- Fixture list item' }).waitFor();
    await page.locator('.chat-message.assistant .markdown-code-block .markdown-code').filter({ hasText: 'const answer = 42;' }).waitFor();
    const historyImage = page.locator('.chat-message-attachment[alt="fixture-preview.png"]');
    await historyImage.waitFor({ state: 'visible' });
    await page.waitForFunction(() => {
      const image = document.querySelector('.chat-message-attachment[alt="fixture-preview.png"]');
      return image?.complete && image.naturalWidth === 1 && image.naturalHeight === 1;
    });
    assert((await historyImage.getAttribute('src')).startsWith('data:image/png;base64,'), 'History image must retain its MIME type and encoded data');
    await page.screenshot({ path: path.join(cache, 'history.png'), fullPage: false });
    report.checks.push('nested route refresh; rich Markdown history; image attachment preview');
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
