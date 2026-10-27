#!/usr/bin/env node
// Read-only visual acceptance against a running Glory frontend and Gateway.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const root = path.resolve(__dirname, '..');
const cache = process.env.SAVFOX_VISUAL_CACHE || (process.platform === 'win32' ? 'D:/BuildCache/savfox-glory/visual-review' : '/mnt/d/BuildCache/savfox-glory/visual-review');
fs.mkdirSync(cache, { recursive: true });
process.env.TEMP = process.env.TMP = cache;
const { chromium } = require(process.env.SAVFOX_PLAYWRIGHT || path.resolve(root, '../../glory-rs/glory/tests/playwright/node_modules/playwright'));
const base = process.env.SAVFOX_VISUAL_BASE || 'http://127.0.0.1:18080';
const tokenFile = process.env.SAVFOX_VISUAL_TOKEN_FILE || path.join(os.homedir(), '.savfox/gateway/token');
const reference = process.env.SAVFOX_VISUAL_REFERENCE || path.resolve(cache, '../reference/screenshots');
const token = fs.readFileSync(tokenFile, 'utf8').trim();
const allRoutes = [['sessions', '/sessions'], ['dashboard', '/'], ['agents', '/agents'], ['channels', '/channels'], ['models', '/models'], ['config', '/config'], ['cron', '/cron'], ['skills', '/skills'], ['tts', '/tts'], ['voice', '/voice'], ['instances', '/instances'], ['logs', '/logs'], ['usage', '/usage'], ['approvals', '/approvals'], ['nodes', '/nodes'], ['debug', '/debug'], ['device-pair', '/device-pair'], ['settings', '/settings']];
const requestedRoutes = process.env.SAVFOX_VISUAL_ROUTES?.split(',').map(value => value.trim()).filter(Boolean);
const routes = requestedRoutes ? allRoutes.filter(([name]) => requestedRoutes.includes(name)) : allRoutes;
if (!routes.length || requestedRoutes?.some(name => !allRoutes.some(([known]) => known === name))) throw new Error('SAVFOX_VISUAL_ROUTES must contain known route names');
const viewportSpecs = process.env.SAVFOX_VISUAL_VIEWPORTS;
const viewports = viewportSpecs ? viewportSpecs.split(',').map(spec => {
  const match = spec.trim().match(/^(\d{3,4})x(\d{3,4})$/);
  if (!match) throw new Error('SAVFOX_VISUAL_VIEWPORTS must contain WIDTHxHEIGHT pairs');
  return [Number(match[1]), Number(match[2])];
}) : [[1440, 900], [390, 844]];
const report = { base, source: 'actual frontend with read-only Gateway requests', pages: [], resizes: [], popups: [], errors: [] };
let browser;
let page;

function inspectLayout() {
  const visible = node => {
    const rect = node.getBoundingClientRect();
    const style = getComputedStyle(node);
    if (rect.width <= 2 || rect.height <= 2 || style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0 || style.clip !== 'auto' || style.clipPath === 'inset(50%)') return false;
    let left = Math.max(0, rect.left), top = Math.max(0, rect.top), right = Math.min(innerWidth, rect.right), bottom = Math.min(innerHeight, rect.bottom);
    for (let ancestor = node.parentElement; ancestor; ancestor = ancestor.parentElement) {
      const ancestorStyle = getComputedStyle(ancestor);
      const bounds = ancestor.getBoundingClientRect();
      if (ancestorStyle.overflowX !== 'visible') { left = Math.max(left, bounds.left); right = Math.min(right, bounds.right); }
      if (ancestorStyle.overflowY !== 'visible') { top = Math.max(top, bounds.top); bottom = Math.min(bottom, bounds.bottom); }
      if (ancestor.matches(':popover-open')) break;
    }
    return right - left > 2 && bottom - top > 2;
  };
  const geometry = selector => {
    const node = selector.endsWith('-listbox')
      ? [...document.querySelectorAll(selector)].find(node => node.getBoundingClientRect().width > 2 && !node.hidden)
      : document.querySelector(selector);
    if (!node) return null;
    const rect = node.getBoundingClientRect();
    const style = getComputedStyle(node);
    return { x: rect.x, y: rect.y, width: rect.width, height: rect.height, right: rect.right, bottom: rect.bottom, scrollHeight: node.scrollHeight, scrollWidth: node.scrollWidth, clientHeight: node.clientHeight, clientWidth: node.clientWidth, overflowY: style.overflowY, paddingBottom: parseFloat(style.paddingBottom) || 0, display: style.display };
  };
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = 1;
  const painter = canvas.getContext('2d', { willReadFrequently: true });
  const rgba = value => {
    painter.clearRect(0, 0, 1, 1);
    painter.fillStyle = value;
    painter.fillRect(0, 0, 1, 1);
    return [...painter.getImageData(0, 0, 1, 1).data].map((v, i) => i === 3 ? v / 255 : v);
  };
  const over = (foreground, background) => foreground.slice(0, 3).map((value, i) => value * foreground[3] + background[i] * (1 - foreground[3])).concat(1);
  const luminance = color => color.slice(0, 3).map(v => v / 255).map(v => v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4).reduce((sum, value, i) => sum + value * [.2126, .7152, .0722][i], 0);
  const rootBackground = rgba(getComputedStyle(document.body).getPropertyValue('--bg-primary').trim() || getComputedStyle(document.documentElement).getPropertyValue('--bg-primary').trim() || '#ffffff');
  const labels = [...document.querySelectorAll('label,.field-label,.form-label,.session-list-filter__label,.chat-input-model-badge__label,.page-title,.panel-header__title,.session-main-title,.page-description,.section-description,.nav-link .nav-label,.sapid-combobox-option,.sapid-select-option,.sapid-collapsible>button,.skill-card__name,.skill-card__description,.skill-card__source')].filter(visible).filter(node => node.textContent.trim()).slice(0, 100).map(node => {
    let background = rootBackground;
    const ancestors = [];
    for (let current = node; current; current = current.parentElement) ancestors.push(current);
    for (const ancestor of ancestors.reverse()) background = over(rgba(getComputedStyle(ancestor).backgroundColor), background);
    const foreground = over(rgba(getComputedStyle(node).color), background);
    const values = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
    return { selector: node.className || node.tagName.toLowerCase(), text: node.textContent.trim().slice(0, 45), contrast: Number(((values[0] + .05) / (values[1] + .05)).toFixed(2)), color: getComputedStyle(node).color };
  });
  const glyphs = [...document.querySelectorAll('.shell-icon,.session-icon')].filter(visible);
  const hasPaintedIcon = node => [getComputedStyle(node), getComputedStyle(node, '::before'), getComputedStyle(node, '::after')].some(style => style.maskImage !== 'none' || style.backgroundImage !== 'none');
  const missingIcons = glyphs.filter(node => node.tagName === 'IMG' ? !node.complete || node.naturalWidth === 0 : !hasPaintedIcon(node)).map(node => node.className);
  const duplicateButtons = [...document.querySelectorAll('button')].filter(visible).map(node => node.innerText.replace(/[\s+＋]/g, '')).filter(value => value.length >= 4 && value.length % 2 === 0 && value.slice(0, value.length / 2) === value.slice(value.length / 2));
  const clippedConfigCards = [...document.querySelectorAll('.config-scroll > .card')].filter(node => getComputedStyle(node).overflowY === 'hidden' && node.scrollHeight > node.clientHeight + 12).map(node => ({ title: node.innerText.split('\n')[0], clientHeight: node.clientHeight, scrollHeight: node.scrollHeight }));
  return {
    viewport: { width: innerWidth, height: innerHeight },
    document: { width: document.documentElement.scrollWidth, height: document.documentElement.scrollHeight },
    geometry: Object.fromEntries(['.top-header', '.sidebar', '.main-content', '.session-list-pane', '.session-list-scroll', '.session-main', '.chat-input-area', '.chat-input', '.chat-picker--agent .sapid-select', '.chat-picker--model .chat-model-select', '.chat-picker--effort .sapid-select', '.sapid-prompt-actions>button:first-child', '.sapid-prompt-actions .sapid-file-button', '.session-search-field', '.session-search-input', '.session-search-field .session-icon', '.bottom-tab-bar', '.sapid-combobox-listbox', '.sapid-select-listbox', '.skill-card .sapid-collapsible-panel'].map(selector => [selector, geometry(selector)])),
    labels,
    icons: { count: glyphs.length, missing: missingIcons },
    controlSkin: [...document.querySelectorAll('.sapid-checkbox input,.sapid-switch input')].filter(visible).map(input => {
      const host = input.closest('.sapid-checkbox,.sapid-switch');
      const bounds = input.getBoundingClientRect();
      return { switch: host.classList.contains('sapid-switch'), unstyled: host.classList.contains('sapid-unstyled'), width: bounds.width, height: bounds.height, scheme: getComputedStyle(input).colorScheme, expectedScheme: getComputedStyle(document.body).colorScheme };
    }),
    navigationHeights: [...document.querySelectorAll('.sidebar .nav-link')].filter(visible).map(node => node.getBoundingClientRect().height),
    formRows: [...document.querySelectorAll('.form-row')].filter(visible).map(row => [...row.children].filter(visible).map(node => {
      const control = node.matches('.form-group') ? node.querySelector('input,textarea,.sapid-select') : node;
      const rect = control.getBoundingClientRect();
      return { x: rect.x, y: rect.y, right: rect.right, bottom: rect.bottom, height: rect.height };
    })),
    forcedCase: [...document.querySelectorAll('h1,h2,h3,button,label,a,[data-gly-kind="text"]')]
      .filter(visible)
      .filter(node => node.textContent.trim() && getComputedStyle(node).textTransform !== 'none')
      .map(node => ({ text: node.textContent.slice(0, 70), className: node.className, transform: getComputedStyle(node).textTransform })),
    textActions: [...document.querySelectorAll('.btn-plain')].filter(visible).map(node => {
      const style = getComputedStyle(node);
      return { className: node.className, shadow: style.boxShadow, background: style.backgroundImage, border: style.borderTopWidth, backgroundColor: style.backgroundColor };
    }),
    duplicateButtons,
    clippedConfigCards,
    searchSkin: (() => { const input = document.querySelector(".session-search-input"); if (!input) return null; const style = getComputedStyle(input); return { border: parseFloat(style.borderTopWidth), shadow: style.boxShadow, background: style.backgroundImage }; })(),
  };
}

function verify(item) {
  const problems = [];
  const metrics = item.metrics;
  const viewport = metrics.viewport;
  const geometry = metrics.geometry;
  if (metrics.document.width > viewport.width + 1) problems.push(`document horizontal overflow: ${metrics.document.width} > ${viewport.width}`);
  if (!geometry['.top-header'] || geometry['.top-header'].height < 48) problems.push('persistent application header is missing or collapsed');
  if (!geometry['.main-content'] || geometry['.main-content'].width < 200 || geometry['.main-content'].height < 100) problems.push('main page content is missing or collapsed');
  if (metrics.icons.count < 4 || metrics.icons.missing.length) problems.push(`missing navigation/control icons: ${metrics.icons.missing.join(', ') || metrics.icons.count}`);
  for (const control of metrics.controlSkin) {
    if (control.unstyled) problems.push('boolean control bypasses Sapid styling');
    if (Math.abs(control.width - (control.switch ? 34 : 17)) > 1 || Math.abs(control.height - (control.switch ? 20 : 17)) > 1) problems.push('Sapid boolean control is distorted');
    if (control.scheme !== control.expectedScheme) problems.push('boolean control uses the wrong color scheme');
  }
  if (viewport.width > 768 && metrics.navigationHeights.some(height => height < 40 || height > 44)) problems.push('desktop navigation item has excessive or insufficient height');
  for (const controls of metrics.formRows) {
    for (let i = 1; i < controls.length; i++) {
      const a = controls[i - 1], b = controls[i];
      if (b.x >= a.right && Math.abs(a.bottom - b.bottom) > 1) problems.push('inline form controls are not aligned');
      if (Math.min(a.right,b.right) - Math.max(a.x,b.x) > 1 && Math.min(a.bottom,b.bottom) - Math.max(a.y,b.y) > 1) problems.push('inline form controls overlap');
    }
  }
  if (metrics.forcedCase.length) problems.push('visible text is forced into another case: ' + JSON.stringify(metrics.forcedCase));
  if (metrics.textActions.some(action => action.shadow !== 'none' || action.background !== 'none')) problems.push('text action inherits a button surface or shadow');
  if (metrics.duplicateButtons.length) problems.push(`duplicate visible button labels: ${metrics.duplicateButtons.join(', ')}`);
  if (metrics.clippedConfigCards.length) problems.push(`configuration cards clip form content: ${metrics.clippedConfigCards.map(card => `${card.title} ${card.clientHeight}/${card.scrollHeight}px`).join(', ')}`);
  if (item.modelPicker) {
    const menu = geometry['.sapid-combobox-listbox'];
    if (!item.options || !menu || menu.width < 100 || menu.height < 20) problems.push('opened model picker has no visible model options');
    if (menu && (menu.y < -1 || menu.bottom > viewport.height + 1 || menu.right > viewport.width + 1)) problems.push('model picker options extend outside the viewport');
  }
  if (item.selectPopup) {
    const menu = geometry['.sapid-select-listbox'];
    if (!item.options || !menu || menu.width < 100 || menu.height < 20) problems.push('opened select has no visible options');
    if (menu && (menu.y < -1 || menu.bottom > viewport.height + 1 || menu.right > viewport.width + 1 || menu.x < -1)) problems.push('select menu extends outside the viewport');
    if (item.selected > 1 || (item.name === 'usage' && item.selected !== 1)) problems.push('select menu has an invalid selected-option count');
  }
  if (item.disclosure) {
    const panel = geometry['.skill-card .sapid-collapsible-panel'];
    if (!panel || panel.height < 40 || Math.min(panel.bottom, viewport.height) - Math.max(panel.y, 56) < 30) problems.push('opened skill More content is not visible in the viewport');
  }
  for (const label of metrics.labels.filter(label => label.contrast < 3)) problems.push(`low contrast ${label.selector}: ${label.contrast} (${label.text})`);
  if (item.name === 'sessions') {
    if (metrics.document.height > viewport.height + 2) problems.push(`sessions stretch document height: ${metrics.document.height} > ${viewport.height}`);
    const list = geometry['.session-list-pane'];
    const scroll = geometry['.session-list-scroll'];
    const composer = geometry['.chat-input-area'];
    const sessionMain = geometry['.session-main'];
    if (!sessionMain || sessionMain.width < 200 || sessionMain.height < 200) problems.push('session workspace disappeared or collapsed');
    if (viewport.width > 760) {
      if (!list || (viewport.width >= 1200 ? Math.abs(list.width - 320) > 2 : list.width < 240 || list.width > 322)) problems.push(`desktop session list width must be 320px, received ${list?.width}`);
      if (!scroll || !['auto', 'scroll'].includes(scroll.overflowY)) problems.push('session list must have independent vertical scrolling');
      if (item.sessionRows >= 20 && scroll && scroll.scrollHeight <= scroll.clientHeight) problems.push('long session list does not scroll inside its own pane');
    }
    const bottom = geometry['.bottom-tab-bar'];
    const boundary = bottom && bottom.height > 0 ? bottom.y : viewport.height;
    if (!composer || composer.height < 100 || composer.bottom > boundary + 2) problems.push(`composer is clipped/overlaps navigation: bottom=${composer?.bottom}, boundary=${boundary}`);
    if (composer && composer.right > viewport.width + 1) problems.push('composer horizontal overflow');
    const card = geometry['.chat-input'];
    const controls = ['.chat-picker--agent .sapid-select', '.chat-picker--model .chat-model-select', '.chat-picker--effort .sapid-select', '.sapid-prompt-actions>button:first-child', '.sapid-prompt-actions .sapid-file-button'].map(selector => ({ selector, bounds: geometry[selector] }));
    for (const { selector, bounds } of controls) {
      if (!bounds || bounds.width < 24 || bounds.height < 28) problems.push('editor control is missing or collapsed: ' + selector);
      else if (!card || bounds.x < card.x - 1 || bounds.right > card.right + 1 || bounds.y < card.y - 1 || bounds.bottom > card.bottom + 1 || bounds.bottom > boundary + 1) problems.push('editor control is clipped: ' + selector);
    }
    for (let i = 0; i < controls.length; i++) for (let j = i + 1; j < controls.length; j++) {
      const a = controls[i].bounds, b = controls[j].bounds;
      if (a && b && Math.min(a.right, b.right) - Math.max(a.x, b.x) > 1 && Math.min(a.bottom, b.bottom) - Math.max(a.y, b.y) > 1) problems.push('editor controls overlap: ' + controls[i].selector + ' / ' + controls[j].selector);
    }
    const search = geometry['.session-search-field'], input = geometry['.session-search-input'], glyph = geometry['.session-search-field .session-icon'];
    if (!search || !input || !glyph || input.x < glyph.right + 4 || input.right > search.right + 1 || Math.abs((input.y + input.height / 2) - (glyph.y + glyph.height / 2)) > 1) problems.push('search icon and input do not align inside their shared field');
    if (search && list && search.bottom > list.bottom + 1) problems.push('session search field is clipped by its pane');
    if (metrics.searchSkin && (metrics.searchSkin.border !== 0 || metrics.searchSkin.shadow !== 'none' || metrics.searchSkin.background !== 'none')) problems.push('session search input has a second frame inside the shared field');

  }
  if (viewport.width < 600) {
    const bottom = geometry['.bottom-tab-bar'];
    if (!bottom || bottom.height < 48 || Math.abs(bottom.bottom - viewport.height) > 2) problems.push('mobile bottom navigation is missing or outside the viewport');
    const main = geometry['.main-content'];
    if (item.name !== 'sessions' && bottom && bottom.height > 0 && main && main.bottom > bottom.y + 2 && main.paddingBottom < bottom.height) problems.push('mobile scroll content lacks room above fixed bottom navigation');
  }
  return problems;
}

function htmlEscape(value) { return String(value).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c])); }

(async () => {
  try {
    browser = await chromium.launch({ headless: true });
    for (const theme of ['dark', 'light']) {
      for (const [width, height] of viewports) {
        const context = await browser.newContext({ viewport: { width, height }, locale: 'zh-CN', colorScheme: theme });
        await context.addInitScript(({ token, theme }) => {
          localStorage.setItem('savfox_token', token);
          localStorage.setItem('savfox_locale', 'zh-CN');
          localStorage.setItem('savfox_theme', theme);
        }, { token, theme });
        page = await context.newPage();
        page.on('pageerror', error => report.errors.push({ type: 'runtime', error: error.message }));
        page.on('console', message => { if (message.type() === 'error') report.errors.push({ type: 'console', error: message.text().slice(0, 2000) }); });
        for (const [name, route] of routes) {
          await page.goto(base + route);
          await page.locator('.top-header__health--ok').waitFor();
          await page.locator(name === 'sessions' ? '.session-main-title' : '.page-title, .page-header__title, .agent-workspace, .agents-workspace, .config-sidebar').first().waitFor();
          if (name === 'sessions') await page.locator('.session-list-item').first().waitFor();
          await page.evaluate(() => document.fonts.ready);
          await page.waitForTimeout(180);
          const size = viewportSpecs ? `${width}x${height}` : `${width}`;
          const file = `${name}-${size}-${theme}.png`;
          await page.screenshot({ path: path.join(cache, file), fullPage: false });
          const item = { name, route, theme, width, height, file, sessionRows: await page.locator('.session-list-item').count(), metrics: await page.evaluate(inspectLayout) };
          item.problems = verify(item);
          report.pages.push(item);
          report.errors.push(...item.problems.map(error => ({ name, theme, width, error })));
          console.log(`${theme} ${width} ${route}: ${item.problems.length ? item.problems.join('; ') : 'PASS'}`);
          if (name === 'sessions') {
            await page.locator('.session-search-input').focus();
            const focused = { name, route, theme, width, height, file: 'sessions-search-focus-' + size + '-' + theme + '.png', sessionRows: item.sessionRows, metrics: await page.evaluate(inspectLayout), searchFocus: true };
            focused.problems = verify(focused);
            await page.screenshot({ path: path.join(cache, focused.file), fullPage: false });
            report.popups.push(focused);
            report.errors.push(...focused.problems.map(error => ({ name, theme, width, height, searchFocus: true, error })));
          }
          const selects = page.locator('.sapid-select:visible');
          const selectCount = await selects.count();
          for (let index = 0; index < selectCount; index++) {
            const trigger = selects.nth(index);
            if (await trigger.isDisabled()) continue;
            await trigger.click();
            const listbox = page.locator('.sapid-select-listbox:visible');
            await listbox.waitFor();
            await page.waitForTimeout(100);
            const popup = { name, route, theme, width, height, file: name + '-select-' + index + '-' + size + '-' + theme + '.png', selectPopup: true, options: await listbox.getByRole('option').count(), selected: await listbox.locator('[aria-selected="true"]').count(), metrics: await page.evaluate(inspectLayout) };
            await page.screenshot({ path: path.join(cache, popup.file), fullPage: false });
            popup.problems = verify(popup);
            report.popups.push(popup);
            report.errors.push(...popup.problems.map(error => ({ name, theme, width, selectPopup: true, error })));
            console.log(theme + ' ' + width + ' ' + route + ' select ' + index + ': ' + (popup.problems.length ? popup.problems.join('; ') : 'PASS'));
            await trigger.press('Escape');
            await listbox.waitFor({ state: 'hidden' });
          }
          if (name === 'sessions' && width === 1440 && height === 900) {
            await page.setViewportSize({ width: 390, height: 844 });
            await page.waitForTimeout(180);
            const resized = { name, route, theme, width: 390, height: 844, file: `sessions-resized-390-${theme}.png`, sessionRows: await page.locator('.session-list-item').count(), metrics: await page.evaluate(inspectLayout) };
            await page.screenshot({ path: path.join(cache, resized.file), fullPage: false });
            resized.problems = verify(resized);
            report.resizes.push(resized);
            report.errors.push(...resized.problems.map(error => ({ name, theme, width: 390, resize: true, error })));
            console.log(`${theme} sessions live resize 1440 to 390: ${resized.problems.length ? resized.problems.join('; ') : 'PASS'}`);
            await page.setViewportSize({ width, height });
            const modelPicker = page.locator('.chat-model-select [role="combobox"],.chat-model-select[role="combobox"]').first();
            await modelPicker.fill('');
            await modelPicker.press('ArrowDown');
            await page.locator('.sapid-combobox-listbox').waitFor({ state: 'visible' });
            await page.waitForTimeout(180);
            const popup = { name, route, theme, width, height, file: `sessions-model-options-${width}-${theme}.png`, modelPicker: true, options: await page.locator('.sapid-combobox-option:visible').count(), sessionRows: await page.locator('.session-list-item').count(), metrics: await page.evaluate(inspectLayout) };
            await page.screenshot({ path: path.join(cache, popup.file), fullPage: false });
            popup.problems = verify(popup);
            report.popups.push(popup);
            report.errors.push(...popup.problems.map(error => ({ name, theme, width, modelPicker: true, error })));
            console.log(`${theme} model picker options: ${popup.problems.length ? popup.problems.join('; ') : 'PASS'}`);
            await modelPicker.press('Escape');
          }
          if (name === 'skills') {
            const disclosure = page.locator('.skill-card .sapid-collapsible>button').first();
            await disclosure.click();
            await page.locator('.skill-card .sapid-collapsible>button[aria-expanded="true"]').first().waitFor();
            await page.locator('.skill-card .sapid-collapsible-panel').first().waitFor({ state: 'visible' });
            await disclosure.evaluate(element => element.scrollIntoView({ block: 'start' }));
            await page.waitForTimeout(180);
            const details = { name, route, theme, width, height, file: `skills-more-${width}-${theme}.png`, disclosure: true, metrics: await page.evaluate(inspectLayout) };
            await page.screenshot({ path: path.join(cache, details.file), fullPage: false });
            details.problems = verify(details);
            report.popups.push(details);
            report.errors.push(...details.problems.map(error => ({ name, theme, width, disclosure: true, error })));
            console.log(`${theme} ${width} skills More disclosure: ${details.problems.length ? details.problems.join('; ') : 'PASS'}`);
          }
        }
        await context.close();
      }
    }
  } catch (error) {
    report.errors.push({ type: 'harness', error: error.stack || String(error) });
    if (page && !page.isClosed()) {
      await page.screenshot({ path: path.join(cache, 'failure.png'), fullPage: false }).catch(() => {});
      fs.writeFileSync(path.join(cache, 'failure.html'), await page.content().catch(() => ''));
    }
  } finally {
    if (browser) await browser.close();
    fs.writeFileSync(path.join(cache, 'report.json'), JSON.stringify(report, null, 2));
    const rows = [...report.pages, ...report.resizes, ...report.popups].map(item => {
      const old = path.join(reference, `${item.name}-${item.width}-dark.png`);
      let referenceImage = '';
      if (item.theme === 'dark' && fs.existsSync(old)) {
        const copied = `reference-${item.name}-${item.width}.png`;
        fs.copyFileSync(old, path.join(cache, copied));
        referenceImage = `<figure><figcaption>Dioxus 03b07b4db</figcaption><img src="${copied}"></figure>`;
      }
      return `<section><h2>${htmlEscape(`${item.name} · ${item.width} · ${item.theme}${item.file.includes('resized') ? ' · live resize' : item.modelPicker ? ' · model options' : item.disclosure ? ' · More disclosure' : ''}`)}</h2><p>${htmlEscape(item.problems.join('; ') || 'Layout and readability checks passed')}</p><div class="comparison">${referenceImage}<figure><figcaption>Glory + Sapid</figcaption><img src="${item.file}"></figure></div></section>`;
    }).join('\n');
    fs.writeFileSync(path.join(cache, 'index.html'), `<!doctype html><meta charset="utf-8"><title>Savfox visual comparison</title><style>body{font:14px system-ui;background:#18151a;color:#eee;margin:24px}.comparison{display:flex;gap:12px;align-items:start}figure{margin:0;min-width:0;flex:1}figcaption{padding:8px}img{max-width:100%;border:1px solid #60515b}section{margin-bottom:36px}p{color:#e6b3b3}</style><h1>Savfox visual comparison</h1><p>${report.pages.length} captures · ${report.resizes.length} live resizes · ${report.popups.length} expanded controls · ${report.errors.length} issues</p>${rows}`);
    console.log(JSON.stringify({ routes: routes.length, pages: report.pages.length, liveResizes: report.resizes.length, modelPickers: report.popups.filter(item => item.modelPicker).length, skillDisclosures: report.popups.filter(item => item.disclosure).length, issues: report.errors.length, report: path.join(cache, 'index.html') }, null, 2));
    if (report.errors.length) process.exitCode = 1;
  }
})();
