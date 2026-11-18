// Проверка места чтения в настоящем браузере: index.html, style.css и части
// панели берутся из crates/server/static, а сервер и агент заменены заглушкой.
// Тест проверяет, что панель ставит список сессий и ленту сессии на сохранённое
// агентом место (`scroll.json`) — в том числе когда сессию открыли по ссылке, —
// и отправляет новое место агенту, когда человек прокручивает.
//
// Запуск: node tests/ui/scroll.mjs
// Нужен установленный Chrome (или Edge); путь можно задать в CHROME_PATH.
// Без браузера тест пропускается с кодом 0.

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
  console.log('Chrome не найден: тест места чтения пропущен (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- данные

const now = Date.now();
const sessions = [];
for (let i = 0; i < 40; i++) {
  sessions.push({
    key: `opencode:s${i}`,
    tool: 'opencode',
    title: `Сессия ${i}`,
    cwd: 'C:\\work\\api',
    status: 'idle',
    live: true,
    updated_at: now - i * 60e3,
    delivery: 'api',
  });
}

const items = [];
for (let i = 1; i <= 100; i++) {
  items.push({
    id: `m${i}`,
    ts: now - (101 - i) * 1000,
    role: i % 2 ? 'assistant' : 'user',
    kind: 'text',
    text:
      `Сообщение ${i}. ` +
      'Длинная строка ленты, чтобы каждое сообщение занимало несколько строк и ' +
      'прокрутка была заметной.\n\n' +
      'Ещё одна строка того же сообщения для проверки места чтения.',
  });
}

// Сохранённое место списка: строка двадцатой сессии чуть ниже верхней кромки.
const listScroll = { id: 'row:pc\nopencode:s20', off: 6, top: 1300, end: false, ts: 7 };

const stub = `
(() => {
  const sessions = ${JSON.stringify(sessions)};
  const items = ${JSON.stringify(items)};
  const listScroll = ${JSON.stringify(listScroll)};
  window.__places = [];
  window.__feedPos = null;
  window.__socket = null;
  class FakeSocket {
    static OPEN = 1;
    constructor() {
      this.readyState = 0;
      window.__socket = this;
      setTimeout(() => {
        this.readyState = 1;
        if (this.onopen) this.onopen();
        this.emit({ t: 'agents',
          agents: [{ name: 'pc', online: true, os: 'win', version: 'test', sessions, pins: [], closed: [] }],
          scroll: listScroll });
      }, 0);
    }
    send(text) {
      const msg = JSON.parse(text);
      if (msg.t === 'scroll') {
        // Агент помнит место чтения сессии: тест задаёт его перед открытием.
        setTimeout(() => this.emit({ t: 'scroll', id: msg.id, agent: msg.agent, session: msg.session, pos: window.__feedPos }), 0);
      } else if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items, more: false }), 0);
      } else if (msg.t === 'scroll_set' || msg.t === 'list_scroll') {
        window.__places.push(msg);
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

// ---------------------------------------------------------------- страница

const assets = new Map([
  ['/md.js', 'text/javascript; charset=utf-8'],
  ['/hl.js', 'text/javascript; charset=utf-8'],
  ['/suggest.js', 'text/javascript; charset=utf-8'],
  ['/style.css', 'text/css; charset=utf-8'],
  ['/icon.svg', 'image/svg+xml'],
  ['/icon-192.png', 'image/png'],
]);
for (const name of fs.readdirSync(STATIC).filter((f) => /^app\..*\.js$/.test(f))) {
  assets.set(`/${name}`, 'text/javascript; charset=utf-8');
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/' || url.pathname === '/index.html') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' }).end(fixture);
    return;
  }
  const type = assets.get(url.pathname);
  if (!type) {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { 'Content-Type': type }).end(fs.readFileSync(path.join(STATIC, url.pathname.slice(1))));
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const pageUrl = `http://127.0.0.1:${server.address().port}/`;

// ---------------------------------------------------------------- браузер

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'aipanel-scroll-'));
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
  throw new Error('браузер не открыл порт отладки');
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
if (!page) throw new Error('не нашёл вкладку браузера');

const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = () => reject(new Error('нет соединения с браузером'));
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

const failures = [];
function check(name, ok, details) {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${ok || details === undefined ? '' : ` — ${JSON.stringify(details)}`}`);
  if (!ok) failures.push(name);
}

// Что стоит у верхней кромки контейнера: id ближайшего узла и его смещение.
async function topOf(selector, nodeSelector) {
  return evaluate(`(() => {
    const box = document.querySelector(${JSON.stringify(selector)});
    const top = box.getBoundingClientRect().top;
    for (const node of box.querySelectorAll(${JSON.stringify(nodeSelector)})) {
      const b = node.getBoundingClientRect();
      if (b.bottom <= top + 1) continue;
      const id = node.dataset.id || '';
      return { id, off: +(b.top - top).toFixed(1), top: Math.round(box.scrollTop) };
    }
    return { id: '', off: 0, top: Math.round(box.scrollTop) };
  })()`);
}

try {
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', {
    width: 390,
    height: 844,
    deviceScaleFactor: 1,
    mobile: true,
  });
  await send('Page.navigate', { url: pageUrl });

  if (!(await waitFor(`document.querySelectorAll('#sessions .session').length === ${sessions.length}`))) {
    throw new Error('список сессий не появился');
  }
  await sleep(300);

  // Сохранённое место списка применяется сразу, как только список отрисован:
  // строка s30 стоит чуть ниже верхней кромки, а не список с начала.
  const listAt = await evaluate(`(() => {
    const box = document.getElementById('sessions');
    const row = [...box.querySelectorAll('.session-row')].find(
      (n) => n.querySelector('.name') && n.querySelector('.name').textContent === 'Сессия 20',
    );
    const b = row.getBoundingClientRect();
    const top = box.getBoundingClientRect().top;
    return {
      off: +(b.top - top).toFixed(1),
      top: Math.round(box.scrollTop),
      scrollable: box.scrollHeight > box.clientHeight,
    };
  })()`);
  check('список прокручивается (тест на месте)', listAt.scrollable, listAt);
  check('список открылся на сохранённом месте', Math.abs(listAt.off - listScroll.off) <= 2, listAt);

  // Прокрутка списка уходит агенту: место запомнится на диске, а не в браузере.
  await evaluate(`(() => {
    const box = document.getElementById('sessions');
    box.scrollTop = 300;
    box.dispatchEvent(new Event('scroll'));
  })()`);
  await sleep(1200);
  const listSaved = await evaluate(`window.__places.filter((m) => m.t === 'list_scroll').at(-1)`);
  check(
    'прокрутка списка сохраняется у агента',
    !!listSaved && typeof listSaved.pos.top === 'number' && listSaved.pos.top > 0 && !listSaved.pos.end,
    listSaved,
  );

  // ------------------------------------------------------------ лента сессии
  // Место чтения ленты тоже приходит от агента: сессию открывают и по ссылке,
  // поэтому ответ на `scroll` приходит раньше самой ленты.
  await evaluate(`window.__feedPos = { id: 'm60', off: -20, top: 600, end: false }`);
  await evaluate(`document.querySelector('#sessions .session').click()`);

  if (!(await waitFor(`document.querySelectorAll('#feed .item').length === ${items.length}`))) {
    throw new Error(`в ленте не ${items.length} элементов`);
  }
  await sleep(200);
  const feedAt = await topOf('#feed', '.item');
  check('лента открылась на сохранённом сообщении', feedAt.id === 'm60', feedAt);
  check('сохранённое смещение внутри сообщения выдержано', Math.abs(feedAt.off + 20) <= 2, feedAt);

  // Читатель не у конца: новый ответ не должен утягивать ленту вниз.
  await evaluate(`window.__socket.emit({ t: 'history', agent: 'pc', session: 'opencode:s0', reset: false,
    items: [{ id: 'm101', ts: Date.now(), role: 'assistant', kind: 'text', text: 'Новый ответ' }], more: false })`);
  await sleep(200);
  const feedNew = await topOf('#feed', '.item');
  check('новый ответ не уводит ленту с места чтения', Math.abs(feedNew.top - feedAt.top) <= 2, { feedAt, feedNew });

  // У конца ленты место сохраняется признаком `end`, а не пикселями: новый
  // ответ делает прежний низ уже не концом.
  await evaluate(`(() => { const f = document.getElementById('feed'); f.scrollTop = f.scrollHeight; f.dispatchEvent(new Event('scroll')); })()`);
  await sleep(1200);
  const feedSaved = await evaluate(`window.__places.filter((m) => m.t === 'scroll_set').at(-1)`);
  check('место в конце ленты сохраняется как конец', !!feedSaved && feedSaved.pos.end === true, feedSaved);
  check('место чтения относится к открытой сессии', !!feedSaved && feedSaved.session === 'opencode:s0', feedSaved);

  check('ошибок на странице нет', pageErrors.length === 0, pageErrors);
} catch (e) {
  console.error(`Ошибка теста: ${e.message}`);
  try {
    const png = await send('Page.captureScreenshot', { format: 'png' });
    const shot = path.join(os.tmpdir(), 'aipanel-scroll-failure.png');
    fs.writeFileSync(shot, Buffer.from(png.data, 'base64'));
    console.error(`Снимок страницы: ${shot}`);
  } catch {}
  failures.push('исключение');
}

if (failures.length) console.error(`\nНе прошло проверок: ${failures.length}`);
else console.log('\nВсе проверки прошли.');

try {
  ws.close();
  chrome.kill();
  server.close();
  fs.rmSync(profile, { recursive: true, force: true });
} catch {}
process.exit(failures.length ? 1 : 0);
