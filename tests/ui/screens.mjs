// Синтетические скриншоты для README: та же страница, что и в tests/ui/feed.mjs,
// но с выдуманными сессиями, каталогами и перепиской — настоящие данные машины
// в кадр не попадают. Скриншоты складываются в docs/.
//
// Запуск: node tests/ui/screens.mjs
// Нужен установленный Chrome (или Edge); путь можно задать в CHROME_PATH.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const STATIC = path.join(ROOT, 'crates/server/static');
const OUT = path.join(ROOT, 'docs');

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
  console.log('Chrome не найден: скриншоты не сняты (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- данные

const now = Date.now();
const items = [];
let n = 0;
const add = (role, kind, text, extra = {}) => {
  n += 1;
  const id = `m${n}`;
  items.push({ id, ts: now - (100 - n) * 4000, role, kind, text, ...extra });
  return id;
};

add('user', 'text', 'Добавь в ленту подсветку синтаксиса для блоков кода и для правок файлов.');
add(
  'assistant',
  'thinking',
  'У блока кода язык приходит из разметки, у правки — только из расширения файла в заголовке диффа. Значит нужен разбор пути и таблица расширений; для неизвестных расширений оставляем текст как есть.',
);
const call = add('assistant', 'tool_use', 'src/highlight.js', { tool: 'Read' });
add('assistant', 'tool_result', 'export function tokens(text, lang) {\n  // …\n}', { id: `${call}:r` });
add(
  'assistant',
  'text',
  'Разбор языка сделан по расширению файла, для правок используется тот же код:\n\n' +
    '```kotlin\n' +
    'fun langOfPath(path: String): String? =\n' +
    '    path.substringAfterLast(\'.\', "").lowercase().let(EXTENSIONS::get)\n' +
    '```\n\n' +
    'Неизвестные расширения остаются без подсветки — так проще не ошибиться.',
);
add(
  'assistant',
  'diff',
  '--- a/src/App.kt\n' +
    '+++ b/src/App.kt\n' +
    '@@ -12,7 +12,9 @@ fun main() {\n' +
    '     val panel = Panel()\n' +
    '-    panel.render(feed)\n' +
    '+    panel.render(feed, highlight = true)\n' +
    '+    panel.scrollToEnd()\n' +
    ' }\n',
);
add('user', 'text', 'Покажи, как это выглядит в ленте.');
add('assistant', 'text', 'Готово: блоки кода и правки подсвечиваются, длинные строки прокручиваются, а без языка текст остаётся как был.');

const sessions = [
  {
    key: 'opencode:demo-1',
    tool: 'opencode',
    title: 'Подсветка синтаксиса',
    cwd: '~/projects/demo',
    status: 'busy',
    live: true,
    updated_at: now - 60 * 1000,
    model: 'opencode-go/deepseek-v4.1-flash',
  },
  {
    key: 'claude:demo-2',
    tool: 'claude',
    title: 'Тесты для API',
    cwd: '~/projects/api',
    status: 'idle',
    live: true,
    updated_at: now - 12 * 60 * 1000,
  },
  {
    key: 'opencode:demo-3',
    tool: 'opencode',
    title: 'Разбор логов выкатки',
    cwd: '~/projects/logs',
    status: 'idle',
    live: false,
    updated_at: now - 3 * 60 * 60 * 1000,
  },
];

const agents = [{ name: 'home-pc', online: true, os: 'windows', version: '7.0', sessions }];

const commands = [
  { name: 'init', description: 'создать AGENTS.md для проекта' },
  { name: 'review', description: 'просмотреть изменения' },
  { name: 'clear', description: 'начать новую сессию' },
];

// ---------------------------------------------------------------- заглушка

const stub = `
(() => {
  const agents = ${JSON.stringify(agents)};
  const first = ${JSON.stringify(sessions[0].key)};
  const items = ${JSON.stringify(items)};
  const commands = ${JSON.stringify(commands)};
  class FakeSocket {
    static OPEN = 1;
    constructor() {
      this.readyState = 0;
      setTimeout(() => {
        this.readyState = 1;
        if (this.onopen) this.onopen();
        this.emit({ t: 'agents', agents });
      }, 0);
    }
    send(text) {
      const msg = JSON.parse(text);
      if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items }), 0);
      } else if (msg.t === 'commands') {
        setTimeout(() => this.emit({ t: 'commands', agent: msg.agent, session: msg.session, commands }), 0);
      }
    }
    close() { this.readyState = 3; }
    emit(msg) { if (this.onmessage) this.onmessage({ data: JSON.stringify(msg) }); }
  }
  window.WebSocket = FakeSocket;
  const realFetch = window.fetch.bind(window);
  window.fetch = (url, opts) =>
    String(url) === '/api/me'
      ? Promise.resolve(new Response(JSON.stringify({ user: 'owner', version: '7.0' }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }))
      : realFetch(url, opts);
})();
`;

const fixture = fs
  .readFileSync(path.join(STATIC, 'index.html'), 'utf8')
  .replace('<script src="/app.js"></script>', `<script>${stub}</script>\n<script src="/app.js"></script>`);

// ---------------------------------------------------------------- страница

const assets = new Map([
  ['/app.js', 'text/javascript; charset=utf-8'],
  ['/md.js', 'text/javascript; charset=utf-8'],
  ['/hl.js', 'text/javascript; charset=utf-8'],
  ['/suggest.js', 'text/javascript; charset=utf-8'],
  ['/style.css', 'text/css; charset=utf-8'],
  ['/icon.svg', 'image/svg+xml'],
  ['/icon-192.png', 'image/png'],
]);

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

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'aipanel-shots-'));
const chrome = spawn(
  chromePath,
  [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-gpu',
    '--hide-scrollbars',
    ...(process.platform === 'linux' ? ['--disable-dev-shm-usage'] : []),
    ...(process.platform === 'linux' && process.getuid && process.getuid() === 0 ? ['--no-sandbox'] : []),
    '--window-size=1440,900',
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
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
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

async function waitFor(expression, ms = 8000) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if (await evaluate(expression)) return true;
    await sleep(60);
  }
  return false;
}

async function shoot(name, width, height, mobile) {
  await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 2, mobile });
  await send('Emulation.setTouchEmulationEnabled', { enabled: mobile, maxTouchPoints: mobile ? 5 : 1 });
  // На узком экране вёрстка зависит от `pointer: coarse`, поэтому страницу
  // перезагружаем уже с включённым касанием — как на телефоне.
  await open();
  await sleep(600);
  const shot = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
  const file = path.join(OUT, `${name}.png`);
  fs.writeFileSync(file, Buffer.from(shot.data, 'base64'));
  const kb = Math.round(fs.statSync(file).size / 1024);
  console.log(`ok    ${path.relative(ROOT, file)} — ${width}×${height}, ${kb} КБ`);
}

// Открывает страницу и ленту первой сессии: у каждой ширины экрана она своя,
// потому что мобильная вёрстка включает другое поведение.
async function open() {
  await send('Page.navigate', { url: pageUrl });
  if (!(await waitFor(`document.querySelectorAll('#sessions .session').length === ${sessions.length}`))) {
    throw new Error('сессии не появились в списке');
  }
  await evaluate(`document.querySelector('#sessions .session').click()`);
  if (!(await waitFor(`document.querySelectorAll('#feed .item').length === ${items.length}`))) {
    throw new Error('лента не заполнилась');
  }
}

try {
  fs.mkdirSync(OUT, { recursive: true });
  await send('Page.enable');
  await send('Runtime.enable');

  await shoot('screen-feed', 1440, 900, false);
  await shoot('screen-mobile', 390, 844, true);
} finally {
  try {
    await send('Browser.close');
  } catch {}
  try {
    ws.close();
  } catch {}
  server.close();
  chrome.kill();
  await sleep(300);
  try {
    fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
  } catch {}
}
