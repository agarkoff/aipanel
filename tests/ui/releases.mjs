// Проверка Release Notes в панели: имя «AiPanel» в шапке списка — кнопка,
// которая открывает статический /releases.md на месте ленты чата. Адрес
// заметок — #releases, «Назад» возвращает к списку, открытая сессия
// закрывается.
//
// Запуск: node tests/ui/releases.mjs
// Нужен установленный Chrome (или Edge); путь можно задать в CHROME_PATH.
// Без браузера проверка пропускается.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const STATIC = path.join(ROOT, 'crates/server/static');

function findChrome() {
  const candidates = [
    process.env.CHROME_PATH,
    'C:/Program Files/Google/Chrome/Application/chrome.exe',
    'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
    'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ].filter(Boolean);
  return candidates.find((p) => fs.existsSync(p)) || null;
}

const chromePath = findChrome();
if (!chromePath) {
  console.log('Chrome не найден: проверка пропущена (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- стенд

const session = {
  key: 'opencode:ui-test',
  tool: 'opencode',
  title: 'Заметки',
  cwd: 'C:\\ui-test',
  status: 'idle',
  live: true,
  updated_at: Date.now(),
  delivery: 'api',
  model: 'opencode-go/deepseek-v4.1-flash',
};

// Заглушка сервера: список из одной сессии и пустая лента. Заметки панель
// берёт настоящие — из crates/server/static/releases.md.
const stub = `
(() => {
  const session = ${JSON.stringify(session)};
  window.__sent = [];
  class FakeSocket {
    static OPEN = 1;
    constructor() {
      this.readyState = 0;
      setTimeout(() => {
        this.readyState = 1;
        if (this.onopen) this.onopen();
        this.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [session] }] });
      }, 0);
    }
    send(text) {
      const msg = JSON.parse(text);
      window.__sent.push(msg);
      if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items: [], more: false }), 0);
      } else if (msg.t === 'favorites') {
        setTimeout(() => this.emit({ t: 'favorites', agent: msg.agent, session: msg.session, items: [] }), 0);
      }
    }
    close() { this.readyState = 3; }
    emit(msg) { if (this.onmessage) this.onmessage({ data: JSON.stringify(msg) }); }
  }
  window.WebSocket = FakeSocket;
  const realFetch = window.fetch.bind(window);
  window.fetch = (url, opts) =>
    String(url) === '/api/me' ? Promise.resolve(new Response(null, { status: 204 })) : realFetch(url, opts);
})();
`;

const fixture = fs
  .readFileSync(path.join(STATIC, 'index.html'), 'utf8')
  .replace('<script src="/hl.js"></script>', `<script>${stub}</script>\n<script src="/hl.js"></script>`);

const types = {
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.md': 'text/markdown; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.webmanifest': 'application/manifest+json',
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/' || url.pathname === '/index.html') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' }).end(fixture);
    return;
  }
  const file = path.join(STATIC, url.pathname.slice(1));
  if (!file.startsWith(STATIC) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    res.writeHead(404).end();
    return;
  }
  const type = types[path.extname(file)] || 'application/octet-stream';
  res.writeHead(200, { 'Content-Type': type }).end(fs.readFileSync(file));
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const pageUrl = `http://127.0.0.1:${server.address().port}/`;

// ---------------------------------------------------------------- браузер

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'aipanel-ui-'));
const chrome = spawn(
  chromePath,
  [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    ...(process.platform === 'linux' ? ['--disable-dev-shm-usage'] : []),
    ...(process.platform === 'linux' && process.getuid && process.getuid() === 0 ? ['--no-sandbox'] : []),
    '--window-size=1280,900',
    'about:blank',
  ],
  { stdio: 'ignore' },
);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function devtoolsPort() {
  const file = path.join(profile, 'DevToolsActivePort');
  for (let i = 0; i < 100; i++) {
    if (fs.existsSync(file)) {
      const port = Number(fs.readFileSync(file, 'utf8').split('\n')[0]);
      if (port) return port;
    }
    await sleep(100);
  }
  throw new Error('Браузер не начал отвечать');
}

const port = await devtoolsPort();
let page = null;
for (let i = 0; i < 50 && !page; i++) {
  try {
    const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    page = list.find((p) => p.type === 'page');
  } catch {}
  if (!page) await sleep(100);
}
if (!page) throw new Error('Не нашлась вкладка браузера');

const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = () => reject(new Error('Нет соединения с браузером'));
});

let seq = 0;
const pending = new Map();
const pageErrors = [];
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.method === 'Runtime.exceptionThrown') {
    pageErrors.push(msg.params.exceptionDetails.exception?.description || msg.params.exceptionDetails.text);
  }
  if (msg.id && pending.has(msg.id)) {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
  }
};
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });

async function evaluate(expression) {
  const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
  return r.result.value;
}

async function waitFor(expression, ms = 5000) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if (await evaluate(expression)) return true;
    await sleep(50);
  }
  return false;
}

// ---------------------------------------------------------------- проверки

const failures = [];
function check(name, ok, details) {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${ok || details === undefined ? '' : ` - ${JSON.stringify(details)}`}`);
  if (!ok) failures.push(name);
}

await send('Page.enable');
await send('Runtime.enable');
await send('Page.navigate', { url: pageUrl });
await waitFor(`!document.getElementById('app').hidden`);

// Имя — кнопка, но выглядит как прежний <strong>: тот же шрифт и цвет,
// без подчёркивания; о нажатии говорит только курсор.
const look = await evaluate(`(() => {
  const b = document.getElementById('brand');
  const s = getComputedStyle(b);
  const body = getComputedStyle(document.body);
  return {
    tag: b.tagName,
    text: b.textContent.trim(),
    weight: s.fontWeight,
    family: s.fontFamily,
    size: s.fontSize,
    cursor: s.cursor,
    decoration: s.textDecorationLine,
    color: s.color,
    bodyColor: body.color,
    bodyFamily: body.fontFamily,
  };
})()`);
check('имя AiPanel — кнопка', look.tag === 'BUTTON' && look.text === 'AiPanel', look);
check('шрифт тот же, что у прежнего strong', look.weight === '700' && look.family === look.bodyFamily, look);
check('не выглядит ссылкой: без подчёркивания и цвета ссылки', look.decoration === 'none' && look.color === look.bodyColor, look);
check('по наведению курсор меняется', look.cursor === 'pointer', look);

// Список на месте, заметки скрыты.
check('заметки закрыты, пока их не открыли', await evaluate(`document.getElementById('notes').hidden`));

// Клик по имени открывает заметки вместо ленты.
await evaluate(`document.getElementById('brand').click()`);
await waitFor(`!document.getElementById('notes').hidden && document.getElementById('notes-body').childElementCount > 0`);
check('заметки открылись', await evaluate(`!document.getElementById('notes').hidden`));
check('адрес заметок — #releases', (await evaluate(`location.hash`)) === '#releases');
check('шапка чата уступила место заметкам', await evaluate(`getComputedStyle(document.querySelector('.main > header.top:not(.notes-head)')).display === 'none'`));
const notes = await evaluate(`(() => {
  const body = document.getElementById('notes-body');
  return { text: body.textContent, headings: body.querySelectorAll('h2').length };
})()`);
check('в заметках история выпусков', notes.text.includes('Release Notes') && notes.text.includes('17.4'), notes.headings);
check('каждый выпуск — свой раздел', notes.headings > 10, notes.headings);
const desktop = await evaluate(`(() => {
  const app = document.getElementById('app');
  const section = document.getElementById('notes');
  const body = document.getElementById('notes-body');
  return {
    appDisplay: getComputedStyle(app).display,
    appClass: app.className,
    notesHeight: Math.round(section.getBoundingClientRect().height),
    bodyHeight: Math.round(body.getBoundingClientRect().height),
  };
})()`);
check('заметки занимают область ленты', desktop.appDisplay === 'grid' && desktop.notesHeight > 300 && desktop.bodyHeight > 300, desktop);

// Кнопка «назад» в заметках возвращает к списку.
await evaluate(`document.getElementById('notes-back').click()`);
await waitFor(`document.getElementById('notes').hidden`);
check('«назад» закрывает заметки', await evaluate(`document.getElementById('notes').hidden && location.hash === ''`));

// Открытая сессия закрывается: заметки занимают её место.
await evaluate(`document.querySelector('#sessions .session').click()`);
await waitFor(`document.getElementById('app').classList.contains('viewing')`);
check('чат открылся', await evaluate(`document.getElementById('app').classList.contains('viewing')`));
await evaluate(`document.getElementById('brand').click()`);
await waitFor(`!document.getElementById('notes').hidden`);
check('заметки заменили ленту, чат закрыт', await evaluate(`document.getElementById('app').classList.contains('releases') && !document.getElementById('app').classList.contains('viewing')`));
// Из заметок «Назад» ведёт к списку, а не возвращает закрытый чат.
await evaluate(`history.back()`);
await waitFor(`document.getElementById('notes').hidden`);
check('«назад» из заметок ведёт к списку', await evaluate(`document.getElementById('notes').hidden && location.hash === '' && !document.getElementById('app').classList.contains('viewing')`));

// Прямая ссылка: заметки открываются после перезагрузки, «назад» просто
// закрывает их, а не уводит со страницы.
await send('Page.navigate', { url: `${pageUrl}#releases` });
await waitFor(`!document.getElementById('notes').hidden && document.getElementById('notes-body').childElementCount > 0`);
check('ссылка #releases открывает заметки', await evaluate(`!document.getElementById('notes').hidden && location.hash === '#releases'`));
await evaluate(`document.getElementById('notes-back').click()`);
await waitFor(`document.getElementById('notes').hidden`);
check('с прямой ссылки «назад» закрывает заметки', await evaluate(`document.getElementById('notes').hidden && location.href === ${JSON.stringify(pageUrl)}`));

// Телефон: заметки открываются вместо списка и занимают весь экран.
await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
await send('Page.navigate', { url: `${pageUrl}?mobile=1` });
await waitFor(`location.search === '?mobile=1' && !document.getElementById('app').hidden`);
await evaluate(`document.getElementById('brand').click()`);
await waitFor(`document.getElementById('app').classList.contains('releases')`);
await sleep(300);
const mobile = await evaluate(`(() => {
  const notes = document.getElementById('notes');
  const main = document.querySelector('.main');
  const body = document.getElementById('notes-body');
  return {
    mainDisplay: getComputedStyle(main).display,
    sidebarDisplay: getComputedStyle(document.getElementById('sidebar')).display,
    appDisplay: getComputedStyle(document.getElementById('app')).display,
    notesHeight: Math.round(notes.getBoundingClientRect().height),
    bodyHeight: Math.round(body.getBoundingClientRect().height),
    text: body.textContent.trim().length,
  };
})()`);
check(
  'на телефоне заметки занимают экран',
  mobile.mainDisplay === 'flex' && mobile.appDisplay === 'grid' && mobile.sidebarDisplay === 'none' && mobile.notesHeight > 300 && mobile.bodyHeight > 300 && mobile.text > 100,
  mobile,
);
await send('Emulation.clearDeviceMetricsOverride');

check('ошибок на странице нет', pageErrors.length === 0, pageErrors);

chrome.kill();
server.close();
if (failures.length) {
  console.log(`\nПровалено: ${failures.length}`);
  process.exit(1);
}
console.log('\nГотово: Release Notes работают.');
