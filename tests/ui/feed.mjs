// Проверка ленты в настоящем браузере: index.html, style.css и app.js берутся
// из crates/server/static, а сервер и агент заменены заглушкой (WebSocket и
// /api/me подменяются до загрузки app.js). Так тест проверяет именно вёрстку и
// раскладку ленты — в частности, что полоса закрепа не отнимает у ленты низ
// последнего сообщения.
//
// Запуск: node tests/ui/feed.mjs
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
  console.log('Chrome не найден: тест ленты пропущен (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- данные ленты

const now = Date.now();
const items = [];
let n = 0;
const add = (role, kind, text) => {
  n += 1;
  items.push({ id: `m${n}`, ts: now - (100 - n) * 1000, role, kind, text });
};
const filler =
  'Строка ленты для проверки прокрутки: она нужна, чтобы лента была выше экрана ' +
  'и низ можно было проверить.';

add('user', 'text', 'Первый вопрос');
for (let i = 0; i < 10; i++) add('assistant', 'text', `${filler}\n\n${filler}`);
add('user', 'text', 'Второй вопрос');
for (let i = 0; i < 10; i++) add('assistant', 'text', `${filler}\n\n${filler}`);
add('user', 'text', 'Последний вопрос');
add('assistant', 'text', 'Короткий ответ');

// Более ранние сообщения, которые «пришлёт агент» по кнопке догрузки.
const olderItems = [
  { id: 'o1', ts: now - 200000, role: 'user', kind: 'text', text: 'Самый ранний вопрос' },
  { id: 'o2', ts: now - 199000, role: 'assistant', kind: 'text', text: 'Самый ранний ответ' },
];

const session = {
  key: 'opencode:ui-test',
  tool: 'opencode',
  title: 'Тест ленты',
  cwd: 'C:\\ui-test',
  status: 'idle',
  live: true,
  updated_at: now,
  delivery: 'api',
  model: 'opencode-go/deepseek-v4.1-flash',
};

// Команды, которые «пришлёт агент» в ответ на запрос списка по «/».
const commands = [
  { name: 'init', description: 'guided AGENTS.md setup' },
  { name: 'review', description: 'review changes' },
  { name: 'clear', description: 'start a new session' },
];

// Модели для окна выбора, которое открывает пункт /models.
const models = [
  { provider: 'opencode-go', id: 'deepseek-v4.1-flash', name: 'DeepSeek V4.1 Flash' },
  { provider: 'openai', id: 'gpt-6', name: 'GPT 6' },
  { provider: 'ollama', id: 'qwen3.5', name: 'Qwen 3.5' },
];

// Алиасы, которые «пришлёт агент» для новой сессии Claude Code.
const claudeModels = [
  { provider: 'claude', id: 'sonnet', name: 'Sonnet — на каждый день' },
  { provider: 'claude', id: 'opus', name: 'Opus — самая мощная' },
];

// Дерево каталогов, которое «пришлёт агент» для окна выбора папки: диски,
// вложенные папки и один каталог без доступа.
const dirs = {
  '': [
    { name: 'Локальный диск (C:)', path: 'C:\\' },
    { name: 'Локальный диск (D:)', path: 'D:\\' },
  ],
  'C:\\': [
    { name: 'Projects', path: 'C:\\Projects' },
    { name: 'ui-test', path: 'C:\\ui-test' },
  ],
  'C:\\Projects': [{ name: 'AiPanel', path: 'C:\\Projects\\demo' }],
  'C:\\Projects\\demo': [],
  'C:\\ui-test': [],
};

// ---------------------------------------------------------------- заглушка

const stub = `
(() => {
  const session = ${JSON.stringify(session)};
  window.__session = session;
  const items = ${JSON.stringify(items)};
  const olderItems = ${JSON.stringify(olderItems)};
  const commands = ${JSON.stringify(commands)};
  const models = ${JSON.stringify(models)};
  const claudeModels = ${JSON.stringify(claudeModels)};
  const dirs = ${JSON.stringify(dirs)};
  window.__sent = [];
  // Отвечать ли на «send» результатом агента: доставку проверяем отдельно,
  // остальным проверкам лишняя подсказка не нужна.
  window.__sendReply = false;
  window.__subWatched = [];
  window.__screens = [];  window.__screenShots = [];
  window.__screensError = '';
  window.__older = [];
  window.__olderPages = [];
  window.__setModels = [];
  window.__starts = [];
  // Избранное: агент хранит список и присылает его целиком на каждый запрос
  // и после каждого изменения.
  window.__favs = [];
  window.__favRequests = [];
  window.__favAsked = 0;
  window.__dirs = [];
  // Уведомления: браузерное разрешение выдано, а сами уведомления записываются,
  // чтобы тест мог проверить, о чём панель сообщает.
  window.__notices = [];
  window.__closed = [];
  class FakeNotification {
    static permission = 'granted';
    static requestPermission() { return Promise.resolve('granted'); }
    constructor(title, opts) { this.tag = opts.tag; window.__notices.push({ title, ...opts }); }
    close() { window.__closed.push(this.tag); }
  }
  window.Notification = FakeNotification;
  window.__sock = null;
  class FakeSocket {
    static OPEN = 1;
    constructor() {
      window.__sock = this;
      this.readyState = 0;
      setTimeout(() => {
        this.readyState = 1;
        if (this.onopen) this.onopen();
        this.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [session] }] });
      }, 0);
    }
    send(text) {
      const msg = JSON.parse(text);
      if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items, more: !!window.__more }), 0);
      } else if (msg.t === 'commands') {
        setTimeout(() => this.emit({ t: 'commands', agent: msg.agent, session: msg.session, commands }), 0);
      } else if (msg.t === 'models') {
        setTimeout(() => this.emit({ t: 'models', agent: msg.agent, session: msg.session, models }), 0);
      } else if (msg.t === 'tool_models') {
        const list = msg.tool === 'claude' ? claudeModels : models;
        setTimeout(() => this.emit({ t: 'tool_models', id: msg.id, models: list }), 0);
      } else if (msg.t === 'dirs') {
        window.__dirs.push(msg);
        const error = msg.path === 'D:\\\\' ? 'нет доступа: D:\\\\' : null;
        setTimeout(() => this.emit({
          t: 'dirs', id: msg.id, path: msg.path,
          entries: error ? [] : (dirs[msg.path] || []), error,
        }), 0);
      } else if (msg.t === 'start') {
        window.__starts.push(msg);
        const error = window.__noDir ? 'каталог не найден: ' + msg.cwd : 'тест';
        setTimeout(() => this.emit({ t: 'result', id: msg.id, ok: false, error }), 0);
      } else if (msg.t === 'older') {
        window.__older.push(msg);
        // Страницы задаёт тест: пустой список — ранних сообщений не осталось.
        const page = window.__olderPages.length ? window.__olderPages.shift() : { items: [], more: false };
        setTimeout(() => this.emit({ t: 'older', agent: msg.agent, session: msg.session, items: page.items, more: page.more }), 0);
      } else if (msg.t === 'watch_sub' || msg.t === 'unwatch_sub') {
        // Ленту подагента тест присылает сам: здесь только запоминаем подписку.
        window.__subWatched.push(msg);
      } else if (msg.t === 'send') {
        window.__sent.push(msg);
        if (window.__sendReply) setTimeout(() => this.emit({ t: 'result', id: msg.id, ok: true }), 0);
      } else if (msg.t === 'set_model') {
        window.__setModels.push(msg);
        setTimeout(() => this.emit({ t: 'result', id: msg.id, ok: true }), 0);
      } else if (msg.t === 'screens') {
        window.__screens.push(msg);
        const shots = window.__screenShots || [];
        shots.forEach((shot, i) =>
          setTimeout(() => this.emit({ t: 'screen', id: msg.id, index: i, count: shots.length, ...shot }), 0));
        setTimeout(() => this.emit({ t: 'screens', id: msg.id, error: window.__screensError || null }), 0);
      } else if (msg.t === 'favorites') {
        window.__favAsked += 1;
        setTimeout(() => this.emit({ t: 'favorites', agent: msg.agent, session: msg.session, items: window.__favs }), 0);
      } else if (msg.t === 'push_subscribe' || msg.t === 'push_unsubscribe') {
        // Подписка на push-уведомления: сервер её принимает или отвечает ошибкой.
        (window.__push = window.__push || []).push(msg);
        const error = window.__pushError || '';
        setTimeout(() => this.emit({ t: 'result', id: msg.id, ok: !error, error }), 0);
      } else if (msg.t === 'favorite') {
        window.__favRequests.push(msg);
        window.__favs = msg.on
          ? [...window.__favs.filter((f) => f.id !== msg.item), { id: msg.item, ts: msg.ts, role: msg.role, text: msg.text, ...(msg.of ? { of: msg.of } : {}) }]
          : window.__favs.filter((f) => f.id !== msg.item);
        setTimeout(() => this.emit({ t: 'favorites', agent: msg.agent, session: msg.session, items: window.__favs }), 0);
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
// Части панели: имена берём из каталога, порядок задаёт index.html.
for (const name of fs.readdirSync(STATIC).filter((f) => /^app\..*\.js$/.test(f))) {
  assets.set(`/${name}`, 'text/javascript; charset=utf-8');
}

const imageRequests = [];
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/' || url.pathname === '/index.html') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' }).end(fixture);
    return;
  }
  // Картинка с машины агента: настоящий сервер спрашивает её у агента.
  if (url.pathname === '/api/image') {
    imageRequests.push(Object.fromEntries(url.searchParams));
    if (url.searchParams.get('path').endsWith('shot.png')) {
      res.writeHead(200, { 'Content-Type': 'image/png' }).end(fs.readFileSync(path.join(STATIC, 'icon-192.png')));
    } else {
      res.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' }).end('файл недоступен');
    }
    return;
  }
  const type = assets.get(url.pathname);
  if (!type) {
    res.writeHead(404).end();
    return;
  }
  const body = fs.readFileSync(path.join(STATIC, url.pathname.slice(1)));
  res.writeHead(200, { 'Content-Type': type }).end(body);
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

// Ждём, пока плавная прокрутка остановится: иначе следующее действие
// (например, прокрутка в начало) может поехать вместе с ней.
async function settleScroll(ms = 3000) {
  const until = Date.now() + ms;
  let last = -1;
  let stable = 0;
  while (Date.now() < until) {
    const top = await evaluate(`document.getElementById('feed').scrollTop`);
    if (top === last) {
      stable += 1;
      if (stable >= 3) return;
    } else {
      stable = 0;
      last = top;
    }
    await sleep(80);
  }
}

// ---------------------------------------------------------------- проверки

const failures = [];
function check(name, ok, details) {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${ok || details === undefined ? '' : ` — ${JSON.stringify(details)}`}`);
  if (!ok) failures.push(name);
}

// Где стоит рисунок значка относительно центра кнопки. Значок обязан быть SVG:
// текстовый символ («+», «×») ставится по метрикам шрифта, а не по центру, и на
// другом шрифте съезжает иначе — по одному шрифту эту разницу и не поймать.
async function iconOffsets(selectors) {
  return evaluate(`(() => {
    const out = {};
    for (const sel of ${JSON.stringify(selectors)}) {
      const el = document.querySelector(sel);
      const svg = el && el.querySelector('svg');
      if (!svg) {
        out[sel] = el ? { text: el.textContent.trim() } : null;
        continue;
      }
      const box = el.getBoundingClientRect();
      const b = svg.getBBox();
      const m = svg.getScreenCTM();
      out[sel] = {
        cx: box.left + box.width / 2,
        cy: box.top + box.height / 2,
        x: m.a * (b.x + b.width / 2) + m.e,
        y: m.d * (b.y + b.height / 2) + m.f,
      };
    }
    return out;
  })()`);
}

function checkIcons(offsets) {
  for (const [sel, m] of Object.entries(offsets)) {
    if (!m) check(`значок ${sel} нарисован`, false, 'кнопки нет');
    else if (m.text !== undefined) check(`значок ${sel} нарисован, а не набран шрифтом`, false, m);
    else {
      const off = { x: +(m.x - m.cx).toFixed(2), y: +(m.y - m.cy).toFixed(2) };
      check(`значок ${sel} стоит по центру кнопки`, Math.abs(off.x) <= 0.5 && Math.abs(off.y) <= 0.5, off);
    }
  }
}

try {
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', {
    width: 1280,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await send('Page.navigate', { url: pageUrl });

  if (!(await waitFor(`document.querySelectorAll('#sessions .session').length === 1`))) {
    throw new Error('сессия не появилась в списке');
  }
  // Регрессия: «+», «×» закрытия консоли и «×» удаления раньше были текстовыми
  // символами и висели ниже центра кружка на 1–2 px (на телефоне — иначе).
  checkIcons(await iconOffsets(['#new-session', '.session-close', '.session-delete']));
  // Раньше этих сообщений в сессии ничего нет: агент пришлёт ленту без more.
  await evaluate(`window.__more = false`);
  await evaluate(`document.querySelector('#sessions .session').click()`);
  if (!(await waitFor(`document.querySelectorAll('#feed .item').length === ${items.length}`))) {
    throw new Error(`в ленте не ${items.length} элементов`);
  }

  const layout = await evaluate(`(() => {
    const feed = document.getElementById('feed');
    const box = feed.getBoundingClientRect();
    const last = [...feed.querySelectorAll('.item')].at(-1).getBoundingClientRect();
    return {
      gap: feed.scrollHeight - feed.scrollTop - feed.clientHeight,
      lastBelow: last.bottom - box.bottom,
      pinHidden: document.getElementById('pin').hidden,
      preview: document.getElementById('pin-preview').textContent,
      count: document.getElementById('pin-count').textContent,
      toEndHidden: document.getElementById('to-end').hidden,
      olderHidden: document.getElementById('older').hidden,
      scrollable: feed.scrollHeight > feed.clientHeight,
    };
  })()`);

  // Регрессия: полоса закрепа появляется в той же отрисовке, что и лента.
  // Раньше прокрутка в конец считалась до её появления, и низ последнего
  // сообщения оставался под кромкой ленты, пока не придёт следующее обновление.
  check('лента прокручивается (тест на месте)', layout.scrollable, layout);
  check('лента прокручена в самый низ', layout.gap <= 1, layout);
  check('последнее сообщение видно целиком', layout.lastBelow <= 1, layout);
  check('закреп показывает последнее своё сообщение', !layout.pinHidden && layout.preview === 'Последний вопрос', layout);
  check('счётчик закрепов', layout.count.trim() === '3 / 3', layout);
  check('кнопка «в конец» скрыта внизу', layout.toEndHidden, layout);
  // Раньше этих сообщений ничего нет — кнопки догрузки быть не должно.
  check('кнопки ранних сообщений нет, когда раньше ничего нет', layout.olderHidden, layout);

  // Регрессия: общее CSS-правило `.user` (имя в шапке) сжимало свои сообщения
  // в ленте в ноль — они оставались в DOM, но были не видны.
  const mine = await evaluate(`(() => {
    const users = [...document.querySelectorAll('#feed .item.user')];
    const heights = users.map((n) => Math.round(n.getBoundingClientRect().height));
    return { count: users.length, heights, min: heights.length ? Math.min(...heights) : 0 };
  })()`);
  check('свои сообщения видны в ленте', mine.count === 3 && mine.min > 0, mine);

  // Копирование текста: у каждого сообщения со временем — кнопка справа от него,
  // прижатая к правому краю (время, наоборот, у левого).
  const copy = await evaluate(`(() => {
    const items = [...document.querySelectorAll('#feed .item.text')];
    const first = items[0];
    const btn = first.querySelector('.foot .copy-text');
    const time = first.querySelector('.foot .time');
    const itemBox = first.getBoundingClientRect();
    const btnBox = btn.getBoundingClientRect();
    const timeBox = time.getBoundingClientRect();
    return {
      items: items.length,
      withButton: items.filter((n) => n.querySelector('.foot .copy-text')).length,
      withTime: items.filter((n) => n.querySelector('.foot .time')).length,
      label: btn.getAttribute('aria-label'),
      iconPaths: btn.querySelectorAll('svg path').length,
      rightGap: Math.round(itemBox.right - btnBox.right),
      leftGap: Math.round(timeBox.left - itemBox.left),
      sameRow: Math.abs((timeBox.top + timeBox.bottom) / 2 - (btnBox.top + btnBox.bottom) / 2) < 3,
    };
  })()`);
  check('у каждого сообщения есть копирование текста', copy.items > 0 && copy.withButton === copy.items && copy.withTime === copy.items, copy);
  check('копирование — справа от времени, по правому краю', copy.rightGap === 0 && copy.leftGap === 0 && copy.sameRow, copy);
  check('у кнопки копирования есть подпись и иконка', copy.label === 'Скопировать текст сообщения' && copy.iconPaths === 2, copy);

  // Копирование подтверждается только галочкой на месте значка на пару секунд:
  // текст внизу панели появляться не должен.
  const copied = await evaluate(`(async () => {
    const btn = document.querySelector('#feed .item.text .foot .copy-text');
    const hint = document.getElementById('hint');
    const before = hint.textContent;
    const real = copyToClipboard;
    copyToClipboard = async () => true;
    btn.click();
    await new Promise((r) => setTimeout(r, 60));
    const during = {
      paths: btn.querySelectorAll('svg path').length,
      copied: btn.classList.contains('copied'),
      hint: hint.textContent,
    };
    await new Promise((r) => setTimeout(r, 2100));
    const after = {
      paths: btn.querySelectorAll('svg path').length,
      copied: btn.classList.contains('copied'),
    };
    copyToClipboard = real;
    return { before, during, after };
  })()`);
  check('после копирования значок сменяется галочкой', copied.during.paths === 1 && copied.during.copied, copied);
  check('галочка возвращается к значку копирования', copied.after.paths === 2 && !copied.after.copied, copied);
  check('при копировании текст внизу панели не появляется', copied.during.hint === copied.before, copied);

  // ------------------------------------------------------------ избранное
  // Список хранит агент, а не браузер: панель спрашивает его при открытии
  // сессии, а звёзды в строке сообщения показывают, что уже в избранном.
  const favLayout = await evaluate(`(() => {
    const texts = [...document.querySelectorAll('#feed .item.text')];
    const first = texts[0];
    const fav = first.querySelector('.foot .fav');
    const copy = first.querySelector('.foot .copy-text');
    const favBox = fav.getBoundingClientRect();
    const copyBox = copy.getBoundingClientRect();
    return {
      items: texts.length,
      withFav: texts.filter((n) => n.querySelector('.foot .fav')).length,
      asked: window.__favAsked,
      label: fav.getAttribute('aria-label'),
      pressed: fav.getAttribute('aria-pressed'),
      fill: getComputedStyle(fav.querySelector('svg')).fill,
      leftOfCopy: favBox.right <= copyBox.left + 1,
      sameRow: Math.abs(favBox.top - copyBox.top) < 3,
      headerHidden: document.getElementById('favorites').hidden,
      countHidden: document.getElementById('favorites-count').hidden,
    };
  })()`);
  check('звезда есть у каждого сообщения, слева от копирования', favLayout.items > 0 && favLayout.withFav === favLayout.items && favLayout.leftOfCopy && favLayout.sameRow, favLayout);
  check('при открытии сессии панель спросила избранное у агента', favLayout.asked >= 1, favLayout);
  check('по умолчанию звезда — контур, а не заливка', favLayout.label === 'Добавить в избранное' && favLayout.pressed === 'false' && favLayout.fill === 'none', favLayout);
  check('звезда в шапке видна, когда открыта сессия', !favLayout.headerHidden, favLayout);
  check('без избранного счётчика у звезды нет', favLayout.countHidden, favLayout);

  // Фоновые задачи: строка над полем ввода с числом задач, список — по нажатию.
  const bgTasks = await evaluate(`(async () => {
    const box = document.getElementById('tasks');
    const before = box.hidden;
    const key = session.key;
    const emit = (tasks) => window.__sock.emit({ t: 'tasks', agent: 'ui-test', session: key, tasks });
    emit([
      { id: 'b1', kind: 'bash', title: 'Сборка образа', started_at: Date.now() - 5 * 60000 },
      { id: 'a1', kind: 'agent', title: 'Поиск по коду' },
    ]);
    // Чужая сессия не влияет на показанный список.
    window.__sock.emit({ t: 'tasks', agent: 'ui-test', session: 'other', tasks: [] });
    const shown = {
      hidden: box.hidden,
      title: document.getElementById('tasks-title').textContent,
      rows: [...box.querySelectorAll('.bg-task')].map((r) => r.textContent),
      aboveComposer: !!(box.compareDocumentPosition(document.getElementById('composer')) & Node.DOCUMENT_POSITION_FOLLOWING),
    };
    emit([]);
    return { before, shown, after: box.hidden };
  })()`.replace('session.key', JSON.stringify(session.key)));
  check('без фоновых задач строки нет', bgTasks.before && bgTasks.after, bgTasks);
  check('фоновые задачи видны с числом', !bgTasks.shown.hidden && bgTasks.shown.title === '2 фоновые задачи' && bgTasks.shown.aboveComposer, bgTasks);
  check('в списке задач вид, описание и время', bgTasks.shown.rows[0] === 'командаСборка образа5 мин' && bgTasks.shown.rows[1] === 'агентПоиск по коду', bgTasks);

  // Своё сообщение: нажатие уходит агенту вместе с началом текста.
  const ownStar = await evaluate(`(async () => {
    const node = [...document.querySelectorAll('#feed .item.text.user')].at(-1);
    const fav = node.querySelector('.foot .fav');
    fav.click();
    await new Promise((r) => setTimeout(r, 150));
    const req = window.__favRequests.at(-1);
    return {
      on: req && req.on,
      role: req && req.role,
      item: req && req.item,
      id: node.dataset.id,
      text: req && req.text,
      lit: fav.classList.contains('on'),
      pressed: fav.getAttribute('aria-pressed'),
      fill: getComputedStyle(fav.querySelector('svg')).fill,
      title: document.getElementById('favorites').title,
      count: document.getElementById('favorites-count').textContent,
      countShown: !document.getElementById('favorites-count').hidden,
      countCorner: (() => {
        // Счётчик стоит в углу самой звезды, а не кнопки.
        const badge = document.getElementById('favorites-count').getBoundingClientRect();
        const star = document.querySelector('#favorites svg').getBoundingClientRect();
        return Math.abs(badge.left + badge.width / 2 - star.right) <= 9 &&
          Math.abs(badge.top + badge.height / 2 - star.top) <= 9;
      })(),
    };
  })()`);
  check('звезда у своего сообщения ставит его в избранное', ownStar.on === true && ownStar.item === ownStar.id && ownStar.lit, ownStar);
  check('в избранное уходит начало сообщения', ownStar.role === 'user' && ownStar.text.startsWith('Последний вопрос'), ownStar);
  check('в избранном звезда залита жёлтым', ownStar.pressed === 'true' && ownStar.fill !== 'none', ownStar);
  check('в шапке показано, сколько в избранном', ownStar.title.includes('1'), ownStar);
  check('у звезды в шапке счётчик избранного', ownStar.countShown && ownStar.count === '1', ownStar);
  check('счётчик прижат к углу звезды', ownStar.countCorner, ownStar);

  // Ответ агента тоже можно добавить.
  const agentStar = await evaluate(`(async () => {
    const node = document.querySelector('#feed .item.text.assistant');
    node.querySelector('.foot .fav').click();
    await new Promise((r) => setTimeout(r, 150));
    const req = window.__favRequests.at(-1);
    return {
      on: req && req.on,
      role: req && req.role,
      item: req && req.item,
      id: node.dataset.id,
      text: req && req.text,
      count: document.getElementById('favorites-count').textContent,
    };
  })()`);
  check('ответ агента тоже добавляется в избранное', agentStar.on === true && agentStar.role === 'assistant' && agentStar.item === agentStar.id, agentStar);
  check('счётчик растёт со вторым сообщением', agentStar.count === '2', agentStar);

  // Плашка в шапке: начало каждого сообщения и кто его написал.
  const favMenu = await evaluate(`(() => {
    document.getElementById('favorites').click();
    const box = document.getElementById('favorites-menu');
    const rows = [...box.querySelectorAll('.fav-row')];
    return {
      open: !box.hidden,
      expanded: document.getElementById('favorites').getAttribute('aria-expanded'),
      rows: rows.map((r) => ({
        who: r.querySelector('.fav-who').textContent,
        text: r.querySelector('.fav-preview').textContent,
      })),
    };
  })()`);
  check('звезда в шапке открывает список избранного', favMenu.open && favMenu.expanded === 'true', favMenu);
  check(
    'в списке начало сообщений и кто их написал',
    favMenu.rows.length === 2 &&
      favMenu.rows.some((r) => r.who === 'Вы' && r.text.startsWith('Последний вопрос')) &&
      favMenu.rows.some((r) => r.who === 'Агент'),
    favMenu,
  );

  // Нажатие вне плашки её закрывает.
  await evaluate(`document.getElementById('feed').dispatchEvent(new MouseEvent('click', { bubbles: true }))`);
  await sleep(80);
  check('нажатие вне списка закрывает плашку', await evaluate(`document.getElementById('favorites-menu').hidden`));

  // Переход из списка: первое сообщение далеко вверху — лента должна к нему
  // прокрутиться и подсветить его.
  const favJump = await evaluate(`(async () => {
    [...document.querySelectorAll('#feed .item.text.user')][0].querySelector('.foot .fav').click();
    await new Promise((r) => setTimeout(r, 150));
    const feed = document.getElementById('feed');
    feed.scrollTop = feed.scrollHeight;
    feed.dispatchEvent(new Event('scroll'));
    document.getElementById('favorites').click();
    await new Promise((r) => setTimeout(r, 60));
    const rows = [...document.querySelectorAll('#favorites-menu .fav-row')];
    const row = rows.find((r) => r.querySelector('.fav-preview').textContent.startsWith('Первый вопрос'));
    if (!row) return { missing: true, previews: rows.map((r) => r.querySelector('.fav-preview').textContent) };
    row.click();
    await new Promise((r) => setTimeout(r, 600));
    const flash = document.querySelector('#feed .item.flash');
    return {
      closed: document.getElementById('favorites-menu').hidden,
      flash: flash ? flash.textContent.replace(/\\s+/g, ' ').trim() : '',
      top: flash ? flash.getBoundingClientRect().top : -1,
      feedTop: feed.getBoundingClientRect().top,
      feedBottom: feed.getBoundingClientRect().bottom,
    };
  })()`);
  check(
    'нажатие в списке закрывает плашку и ведёт к сообщению',
    !favJump.missing && favJump.closed && favJump.flash.startsWith('Первый вопрос') &&
      favJump.top >= favJump.feedTop - 1 && favJump.top <= favJump.feedBottom,
    favJump,
  );

  // Повторное нажатие убирает сообщение из избранного.
  const removeStar = await evaluate(`(async () => {
    const node = document.querySelector('#feed .item.text.assistant');
    const fav = node.querySelector('.foot .fav');
    fav.click();
    await new Promise((r) => setTimeout(r, 150));
    const req = window.__favRequests.at(-1);
    return {
      on: req && req.on,
      item: req && req.item,
      id: node.dataset.id,
      lit: fav.classList.contains('on'),
      count: window.__favs.length,
      shown: document.getElementById('favorites-count').textContent,
    };
  })()`);
  check('повторное нажатие убирает сообщение из избранного', removeStar.on === false && removeStar.item === removeStar.id && !removeStar.lit && removeStar.count === 2, removeStar);
  check('счётчик уменьшается вместе со списком', removeStar.shown === '2', removeStar);

  // Выделение куска ответа: под ним плашка «копировать / в избранное».
  const selShow = await evaluate(`(async () => {
    const feed = document.getElementById('feed');
    const first = (n) => document.createTreeWalker(n.querySelector('.body'), NodeFilter.SHOW_TEXT).nextNode();
    const node = [...feed.querySelectorAll('.item.text.assistant')].filter((n) => first(n).length > 40).at(-1);
    node.scrollIntoView({ block: 'center' });
    const t = first(node);
    const range = document.createRange();
    range.setStart(t, 6);
    range.setEnd(t, 30);
    getSelection().removeAllRanges();
    getSelection().addRange(range);
    await new Promise((r) => setTimeout(r, 300));
    const bar = document.querySelector('.sel-bar');
    const box = bar.getBoundingClientRect();
    const sel = range.getBoundingClientRect();
    return {
      hidden: bar.hidden,
      favShown: !bar.querySelector('.sel-fav').hidden,
      below: box.top >= sel.bottom,
      inView: box.left >= 0 && box.right <= innerWidth && box.bottom <= innerHeight,
      text: range.toString(),
      id: node.dataset.id,
    };
  })()`);
  check('выделение в сообщении показывает плашку под ним', !selShow.hidden && selShow.favShown && selShow.below && selShow.inView, selShow);

  const selCopied = await evaluate(`(async () => {
    const real = copyToClipboard;
    let got = null;
    copyToClipboard = async (t) => { got = t; return true; };
    document.querySelector('.sel-bar .sel-copy').click();
    await new Promise((r) => setTimeout(r, 60));
    copyToClipboard = real;
    return { got, done: document.querySelector('.sel-bar').classList.contains('done') };
  })()`);
  check('«Копировать» на плашке копирует выделенное', selCopied.got === selShow.text.trim() && selCopied.done, { selCopied, selShow });

  const selFaved = await evaluate(`(async () => {
    document.querySelector('.sel-bar .sel-fav').click();
    await new Promise((r) => setTimeout(r, 150));
    const req = window.__favRequests.at(-1);
    return {
      req,
      hidden: document.querySelector('.sel-bar').hidden,
      collapsed: getSelection().isCollapsed,
      count: document.getElementById('favorites-count').textContent,
    };
  })()`);
  check(
    'звезда на плашке кладёт фрагмент в избранное со ссылкой на сообщение',
    selFaved.req.on && selFaved.req.of === selShow.id && selFaved.req.item.startsWith('q:') &&
      selFaved.req.text === selShow.text.trim() && selFaved.req.role === 'assistant',
    selFaved,
  );
  check('после добавления плашка и выделение пропадают', selFaved.hidden && selFaved.collapsed && selFaved.count === '3', selFaved);

  // В списке фрагмент помечен и ведёт к своему сообщению с подсветкой куска.
  const fragJump = await evaluate(`(async () => {
    const feed = document.getElementById('feed');
    feed.scrollTop = 0;
    feed.dispatchEvent(new Event('scroll'));
    document.getElementById('favorites').click();
    await new Promise((r) => setTimeout(r, 60));
    const item = document.querySelector('#favorites-menu .fav-item.part');
    if (!item) return { missing: true };
    const tag = item.querySelector('.fav-part') ? item.querySelector('.fav-part').textContent : '';
    item.querySelector('.fav-row').click();
    await new Promise((r) => setTimeout(r, 600));
    const flash = document.querySelector('#feed .item.flash');
    const hl = window.CSS && CSS.highlights ? CSS.highlights.get('fav-part') : null;
    let marked = '';
    if (hl) for (const r of hl) marked = r.toString();
    return { tag, flash: flash ? flash.dataset.id : '', marked, supported: !!(window.CSS && CSS.highlights) };
  })()`);
  check(
    'фрагмент в списке ведёт к сообщению и подсвечивает кусок',
    !fragJump.missing && fragJump.tag === 'фрагмент' && fragJump.flash === selShow.id &&
      (!fragJump.supported || fragJump.marked === selShow.text.trim()),
    fragJump,
  );

  // Крестик в списке убирает фрагмент и не закрывает плашку.
  const fragRemove = await evaluate(`(async () => {
    document.getElementById('favorites').click();
    await new Promise((r) => setTimeout(r, 60));
    const item = document.querySelector('#favorites-menu .fav-item.part');
    item.querySelector('.fav-remove').click();
    await new Promise((r) => setTimeout(r, 150));
    const req = window.__favRequests.at(-1);
    const open = !document.getElementById('favorites-menu').hidden;
    const left = document.querySelectorAll('#favorites-menu .fav-item.part').length;
    document.getElementById('favorites').click();
    return { on: req.on, of: req.of, open, left, count: window.__favs.length };
  })()`);
  check('крестик убирает фрагмент, список остаётся открытым', fragRemove.on === false && fragRemove.open && fragRemove.left === 0 && fragRemove.count === 2, fragRemove);

  // Выделение через два сообщения: скопировать можно, в избранное — нет.
  const selCross = await evaluate(`(async () => {
    const list = [...document.querySelectorAll('#feed .item.text.assistant')];
    const a = list.at(-2).querySelector('.body');
    const b = list.at(-1).querySelector('.body');
    const range = document.createRange();
    range.setStart(document.createTreeWalker(a, NodeFilter.SHOW_TEXT).nextNode(), 3);
    range.setEnd(document.createTreeWalker(b, NodeFilter.SHOW_TEXT).nextNode(), 5);
    getSelection().removeAllRanges();
    getSelection().addRange(range);
    await new Promise((r) => setTimeout(r, 300));
    const bar = document.querySelector('.sel-bar');
    const res = { hidden: bar.hidden, favHidden: bar.querySelector('.sel-fav').hidden };
    getSelection().removeAllRanges();
    await new Promise((r) => setTimeout(r, 300));
    res.afterClear = bar.hidden;
    return res;
  })()`);
  check('выделение через два сообщения: только копирование', !selCross.hidden && selCross.favHidden, selCross);
  check('снятое выделение убирает плашку', selCross.afterClear, selCross);

  // Нажатия на закреп: переход к сообщению и переключение на предыдущее своё.
  const pinState = async () => evaluate(`(() => {
    const feed = document.getElementById('feed');
    const flash = document.querySelector('#feed .item.flash');
    return {
      preview: document.getElementById('pin-preview').textContent,
      count: document.getElementById('pin-count').textContent.trim(),
      flash: flash ? flash.textContent.replace(/\\s+/g, ' ').trim() : '',
      flashTop: flash ? flash.getBoundingClientRect().top : -1,
      feedTop: feed.getBoundingClientRect().top,
      feedBottom: feed.getBoundingClientRect().bottom,
    };
  })()`);

  // Закреп следует за прокруткой: показывает своё сообщение, ближайшее выше
  // верхней кромки ленты, а у самого низа — последнее своё, как в Telegram.
  await evaluate(`document.getElementById('feed').scrollTop = 0`);
  await sleep(200);
  let followed = await pinState();
  check('в начале ленты закреп показывает первое своё сообщение', followed.preview === 'Первый вопрос' && followed.count === '1 / 3', followed);

  await evaluate(`(() => {
    const f = document.getElementById('feed');
    const second = [...f.querySelectorAll('.item.user')][1];
    const box = f.getBoundingClientRect();
    const contentTop = f.scrollTop + second.getBoundingClientRect().top - box.top;
    f.scrollTop = contentTop + 40;
  })()`);
  await sleep(200);
  followed = await pinState();
  check('при прокрутке закреп показывает ближайшее предыдущее', followed.preview === 'Второй вопрос' && followed.count === '2 / 3', followed);

  await evaluate(`(() => { const f = document.getElementById('feed'); f.scrollTop = f.scrollHeight; })()`);
  await sleep(200);
  followed = await pinState();
  check('внизу закреп снова показывает последнее своё', followed.preview === 'Последний вопрос' && followed.count === '3 / 3', followed);

  await evaluate(`document.getElementById('pin').click()`);
  await sleep(300);
  let pin = await pinState();
  check('закреп перешёл к последнему своему сообщению', pin.flash.startsWith('Последний вопрос'), pin);
  check('это сообщение в видимой части ленты', pin.flashTop >= pin.feedTop - 1 && pin.flashTop <= pin.feedBottom, pin);
  check('закреп переключился на предыдущее', pin.preview === 'Второй вопрос' && pin.count === '2 / 3', pin);

  await evaluate(`document.getElementById('pin').click()`);
  await sleep(300);
  pin = await pinState();
  check('второе нажатие ведёт к первому сообщению', pin.preview === 'Первый вопрос' && pin.count === '1 / 3', pin);

  await evaluate(`document.getElementById('pin').click()`);
  await sleep(300);
  pin = await pinState();
  check('после самого старого закреп возвращается к последнему', pin.preview === 'Последний вопрос' && pin.count === '3 / 3', pin);

  // После перехода по закрепу ручная прокрутка снова двигает закреп, а не
  // оставляет его на сообщении, к которому прыгнули.
  await settleScroll();
  await evaluate(`(() => {
    const f = document.getElementById('feed');
    const second = [...f.querySelectorAll('.item.user')][1];
    const box = f.getBoundingClientRect();
    const contentTop = f.scrollTop + second.getBoundingClientRect().top - box.top;
    f.scrollTop = contentTop + 40;
  })()`);
  await sleep(250);
  const afterScroll = await pinState();
  check('после перехода прокрутка снова меняет закреп', afterScroll.preview === 'Второй вопрос' && afterScroll.count === '2 / 3', afterScroll);

  // Кнопка «в конец»: появляется при прокрутке вверх, возвращает в конец.
  await settleScroll();
  await evaluate(`document.getElementById('feed').scrollTop = 0`);
  await sleep(200);
  check('кнопка «в конец» видна после прокрутки вверх', !(await evaluate(`document.getElementById('to-end').hidden`)));

  await evaluate(`document.getElementById('to-end').click()`);
  const reachedEnd = await waitFor(
    `(() => { const f = document.getElementById('feed'); return f.scrollHeight - f.scrollTop - f.clientHeight <= 1; })()`,
    4000,
  );
  const endState = await evaluate(`(() => {
    const f = document.getElementById('feed');
    return { hidden: document.getElementById('to-end').hidden, gap: f.scrollHeight - f.scrollTop - f.clientHeight };
  })()`);
  check('кнопка «в конец» возвращает в конец', reachedEnd && endState.gap <= 1, { reachedEnd, ...endState });
  check('кнопка «в конец» скрывается внизу', endState.hidden, endState);

  // ------------------------------------------------------------ ранние сообщения
  // Агент присылает только последние сообщения сессии; остальные догружаются,
  // когда лента доходит до верхней кромки. Старые элементы встают в начало, а
  // место, которое человек читает, не съезжает.

  const olderState = () => evaluate(`(() => {
    const feed = document.getElementById('feed');
    const btn = document.getElementById('older');
    const first = feed.querySelector('.item');
    return {
      hidden: btn.hidden,
      disabled: btn.disabled,
      first: feed.firstElementChild === btn,
      items: feed.querySelectorAll('.item').length,
      firstItem: first ? first.textContent.replace(/\\s+/g, ' ').trim() : '',
      count: document.getElementById('pin-count').textContent.trim(),
      requests: window.__older.length,
    };
  })()`);
  // Верх элемента в окне: при догрузке он должен остаться на месте.
  const viewTop = () => evaluate(`(() => {
    const item = [...document.querySelectorAll('#feed .item')].find((n) => n.textContent.includes('Второй вопрос'));
    return Math.round(item.getBoundingClientRect().top);
  })()`);

  // Открываем сессию заново: теперь агент говорит, что раньше есть ещё лента.
  await evaluate(`window.__more = true; window.__olderPages = [{ items: ${JSON.stringify(olderItems)}, more: false }]`);
  await evaluate(`document.getElementById('back').click()`);
  await waitFor(`!document.getElementById('app').classList.contains('viewing')`);
  await evaluate(`document.querySelector('#sessions .session').click()`);
  if (!(await waitFor(`document.querySelectorAll('#feed .item').length === ${items.length}`))) {
    throw new Error('сессия не открылась заново');
  }

  const olderBefore = await olderState();
  check('кнопка ранних сообщений видна в начале ленты', !olderBefore.hidden && olderBefore.first, olderBefore);

  await evaluate(`document.getElementById('feed').scrollTop = 0`);
  const keepTop = await viewTop();
  const loaded = await waitFor(
    `document.querySelectorAll('#feed .item').length === ${items.length + olderItems.length}`,
    4000,
  );
  const olderAfter = await olderState();
  const movedTop = await viewTop();
  check('прокрутка к верху догрузила ранние сообщения', loaded && olderAfter.items === items.length + olderItems.length, olderAfter);
  check('они встали в начало ленты', olderAfter.firstItem.startsWith('Самый ранний вопрос'), olderAfter);
  check('кнопка осталась первой строкой ленты', olderAfter.first, olderAfter);
  check('читаемое место не съехало', Math.abs(movedTop - keepTop) <= 2, { keepTop, movedTop });
  check('закреп теперь считает и ранние', olderAfter.count.endsWith('/ 4'), olderAfter);
  check('кнопка скрылась: ранних больше нет', olderAfter.hidden, olderAfter);

  // ------------------------------------------------------------ закреп и ранние
  // Нажатие на самый старый закреп догружает ранние сообщения до предыдущего
  // своего и везёт к нему, а не перескакивает к последнему. Страницу без своих
  // сообщений панель пропускает и просит следующую.

  const earlyAnswer = [
    { id: 'o3', ts: now - 198000, role: 'assistant', kind: 'text', text: 'Ранний ответ без вопроса' },
  ];
  const earlyQuestion = [
    { id: 'o4', ts: now - 197000, role: 'user', kind: 'text', text: 'Совсем ранний вопрос' },
    { id: 'o5', ts: now - 196000, role: 'assistant', kind: 'text', text: 'Ответ рядом' },
  ];

  await evaluate(`window.__more = true; window.__olderPages = [
    { items: ${JSON.stringify(earlyAnswer)}, more: true },
    { items: ${JSON.stringify(earlyQuestion)}, more: false },
  ]`);
  await evaluate(`document.getElementById('back').click()`);
  await waitFor(`!document.getElementById('app').classList.contains('viewing')`);
  await evaluate(`document.querySelector('#sessions .session').click()`);
  if (!(await waitFor(`document.querySelectorAll('#feed .item').length === ${items.length}`))) {
    throw new Error('сессия не открылась заново');
  }

  // Закреп с последнего своего на первое: 3/3 → 2/3 → 1/3.
  await evaluate(`document.getElementById('pin').click()`);
  await sleep(300);
  await evaluate(`document.getElementById('pin').click()`);
  await sleep(300);
  await settleScroll();
  let earlyPin = await pinState();
  check('перед догрузкой закреп стоит на самом старом своём', earlyPin.preview === 'Первый вопрос' && earlyPin.count === '1 / 3', earlyPin);

  const askedBefore = await evaluate(`window.__older.length`);
  await evaluate(`document.getElementById('pin').click()`);
  const earlyLoaded = await waitFor(
    `document.querySelectorAll('#feed .item').length === ${items.length + earlyAnswer.length + earlyQuestion.length}`,
    4000,
  );
  const askedAfter = await evaluate(`window.__older.length`);
  earlyPin = await pinState();
  check('нажатие на самый старый закреп догрузило ранние', earlyLoaded && askedAfter - askedBefore === 2, { earlyLoaded, askedBefore, askedAfter });
  check('лента поехала к предыдущему своему сообщению', earlyPin.flash.startsWith('Совсем ранний вопрос'), earlyPin);
  check('закреп встал на него и считает по-новому', earlyPin.preview === 'Совсем ранний вопрос' && earlyPin.count === '1 / 4', earlyPin);

  await settleScroll();
  const earlyShown = await evaluate(`(() => {
    const feed = document.getElementById('feed');
    const box = feed.getBoundingClientRect();
    const node = [...feed.querySelectorAll('.item.user')].find((n) => n.textContent.includes('Совсем ранний вопрос'));
    return {
      top: node ? Math.round(node.getBoundingClientRect().top) : -1,
      feedTop: Math.round(box.top),
      feedBottom: Math.round(box.bottom),
    };
  })()`);
  check('предыдущее своё сообщение в видимой части ленты', earlyShown.top >= earlyShown.feedTop - 1 && earlyShown.top <= earlyShown.feedBottom, earlyShown);

  // Раньше своих больше нет: нажатие снова идёт по кругу к последнему.
  await evaluate(`document.getElementById('pin').click()`);
  await sleep(300);
  earlyPin = await pinState();
  check('после самого старого закреп снова уходит к последнему', earlyPin.preview === 'Последний вопрос' && earlyPin.count === '4 / 4', earlyPin);

  // ------------------------------------------------------------ слэш-команды
  // Список команд в поле ввода: «/» открывает его, ввод фильтрует, стрелки и
  // Enter подставляют команду, а Enter без выбора отправляет набранное.

  const slashState = () => evaluate(`(() => {
    const list = document.querySelector('#composer .sg-list');
    const shown = !!list && !list.hidden;
    return {
      open: shown,
      items: shown ? [...list.querySelectorAll('.sg-item')].map((n) => n.querySelector('b').textContent) : [],
      empty: shown && list.querySelector('.sg-empty') ? list.querySelector('.sg-empty').textContent : '',
      text: document.getElementById('text').value,
    };
  })()`);

  const typeSlash = (value) => evaluate(`(() => {
    const t = document.getElementById('text');
    t.focus();
    t.value = ${JSON.stringify(value)};
    t.setSelectionRange(t.value.length, t.value.length);
    t.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);

  const press = (key) => evaluate(`(() => {
    document.getElementById('text').dispatchEvent(new KeyboardEvent('keydown', { key: ${JSON.stringify(key)}, bubbles: true, cancelable: true }));
  })()`);

  await typeSlash('/');
  const listed = await waitFor(
    `(() => {
      const list = document.querySelector('#composer .sg-list');
      return list && !list.hidden && list.querySelectorAll('.sg-item').length === ${commands.length + 1};
    })()`,
  );
  const all = await slashState();
  check('«/» открывает список команд', listed && all.open, all);
  check('в списке команды с описаниями', all.items.join(',') === 'clear,init,models,review', all);

  await typeSlash('/cl');
  await sleep(100);
  const filtered = await slashState();
  check('список фильтруется по имени', filtered.items.join(',') === 'clear', filtered);

  await press('ArrowDown');
  await press('Enter');
  await sleep(100);
  const picked = await slashState();
  const sentBefore = await evaluate(`window.__sent.length`);
  check('Enter подставляет выбранную команду', picked.text === '/clear ' && !picked.open, picked);
  check('выбор из списка не отправляет сообщение', sentBefore === 0, sentBefore);

  // Enter без выбранного стрелками пункта — обычная отправка набранного.
  await evaluate(`window.__sendReply = true`);
  await typeSlash('/clear');
  await sleep(100);
  await press('Enter');
  await sleep(150);
  const sentText = await evaluate(`window.__sent.map((m) => m.text)`);
  check('Enter без выбора отправляет набранное', sentText.join('|') === '/clear', sentText);

  // Ответ на отправку — подсказка самого чата: в копии для списка сессий её
  // нет, иначе на телефоне «Доставлено агенту.» висело бы внизу списка.
  const delivered = await evaluate(`(async () => {
    await new Promise((r) => setTimeout(r, 50));
    window.__sendReply = false;
    return {
      hint: document.getElementById('hint').textContent,
      list: document.getElementById('hint-list').textContent,
      listShown: getComputedStyle(document.getElementById('hint-list')).display !== 'none',
    };
  })()`);
  check('доставка сообщения видна в чате', delivered.hint === 'Доставлено агенту.', delivered);
  check('в списке сессий доставки сообщения нет', delivered.list === '' && !delivered.listShown, delivered);

  // Ответ, которого никто не ждёт (так приходит тихая подписка на push при
  // запуске панели), тоже не должен превращаться в подсказку.
  const stranger = await evaluate(`(() => {
    const before = document.getElementById('hint').textContent;
    window.__sock.emit({ t: 'result', id: 'no-one-waits', ok: true });
    return {
      before,
      after: document.getElementById('hint').textContent,
      list: document.getElementById('hint-list').textContent,
    };
  })()`);
  check('ответ без ожидающего его не даёт подсказки', stranger.after === stranger.before && stranger.list === '', stranger);

  // Список относится только к началу сообщения: дальше «/» — обычный текст.
  await typeSlash('путь /init');
  await sleep(100);
  check('посреди текста список не открывается', !(await slashState()).open);

  // Параметры команды печатаются после имени: выбор из списка подставляет
  // «/имя » и оставляет курсор за пробелом, дальше текст просто набирается.
  await typeSlash('/rev');
  await waitFor(`(() => { const l = document.querySelector('#composer .sg-list'); return l && !l.hidden && l.querySelectorAll('.sg-item').length === 1; })()`);
  await press('ArrowDown');
  await press('Enter');
  await sleep(100);
  const withSpace = await slashState();
  check('выбор команды оставляет место для параметров', withSpace.text === '/review ' && !withSpace.open, withSpace);

  await evaluate(`(() => {
    const t = document.getElementById('text');
    t.focus();
    t.value += '24';
    t.setSelectionRange(t.value.length, t.value.length);
    t.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await sleep(100);
  const withArgs = await slashState();
  check('параметры не снова открывают список', withArgs.text === '/review 24' && !withArgs.open, withArgs);

  await press('Enter');
  await sleep(150);
  const argsSent = await evaluate(`window.__sent.map((m) => m.text)`);
  check('команда с параметрами уходит целиком', argsSent.at(-1) === '/review 24', argsSent);

  // Escape закрывает список и не трогает набранное.
  await typeSlash('/i');
  await waitFor(`(() => { const l = document.querySelector('#composer .sg-list'); return l && !l.hidden; })()`);
  await press('Escape');
  await sleep(100);
  const escaped = await slashState();
  check('Escape закрывает список', !escaped.open && escaped.text === '/i', escaped);

  // ------------------------------------------------------------ выбор модели
  // `/models` — действие панели: не подставляется в текст, а открывает окно
  // со списком моделей opencode.

  const modelsState = () => evaluate(`(() => {
    const dlg = document.getElementById('models');
    const rows = [...dlg.querySelectorAll('.model')];
    return {
      open: dlg.open,
      text: document.getElementById('text').value,
      rows: rows.map((b) => b.querySelector('.model-name').textContent),
      current: (dlg.querySelector('.model.current') || {}).title || '',
      hint: document.getElementById('models-hint').textContent,
      hintError: document.getElementById('models-hint').classList.contains('error'),
    };
  })()`);

  await typeSlash('/mod');
  await waitFor(`(() => { const l = document.querySelector('#composer .sg-list'); return l && !l.hidden && l.querySelectorAll('.sg-item').length === 1; })()`);
  await press('ArrowDown');
  await press('Enter');
  await sleep(150);
  const dialog = await modelsState();
  check('пункт /models открывает окно выбора модели', dialog.open && dialog.rows.length === 3, dialog);
  check('в окне все модели с названиями', dialog.rows.join(',') === 'DeepSeek V4.1 Flash,GPT 6,Qwen 3.5', dialog);
  check('текущая модель отмечена', dialog.current === 'opencode-go/deepseek-v4.1-flash', dialog);
  check('/models не подставляется в текст', dialog.text === '', dialog);

  await evaluate(`(() => {
    const f = document.getElementById('models-filter');
    f.value = 'gpt';
    f.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const filteredModels = await modelsState();
  check('фильтр в окне моделей', filteredModels.rows.join(',') === 'GPT 6', filteredModels);

  await evaluate(`document.querySelector('#models .model').click()`);
  const switched = await waitFor(`!document.getElementById('models').open`, 3000);
  const chosenModel = await evaluate(`window.__setModels`);
  check(
    'выбор модели уходит агенту',
    chosenModel.length === 1 && chosenModel[0].provider === 'openai' && chosenModel[0].model === 'gpt-6',
    chosenModel,
  );
  const afterModel = await modelsState();
  check('окно закрылось после смены модели', switched && !afterModel.open && afterModel.text === '', afterModel);
  const modelHint = await evaluate(`document.getElementById('hint').textContent`);
  check('панель сообщила о выбранной модели', modelHint.includes('GPT 6'), modelHint);

  // ------------------------------------------------------------ новая сессия
  // В окне запуска модель выбирается списком для выбранного инструмента.
  // Недавние модели окно запуска показывает первыми: их запоминает браузер.
  await evaluate(`localStorage.setItem('aipanel.model-recents:opencode', JSON.stringify(['openai/gpt-6']))`);

  await evaluate(`document.getElementById('new-session').click()`);
  await waitFor(`document.getElementById('launch').open`);
  const launchInit = await evaluate(`({
    tool: document.getElementById('launch-tool').value,
    cwd: document.getElementById('launch-cwd').value,
    model: document.getElementById('launch-model').value,
  })`);
  check('окно запуска открылось с инструментом сессии', launchInit.tool === 'opencode', launchInit);
  check('поле модели пустое до выбора', launchInit.model === '', launchInit);

  // Дерево выбора папки: «Обзор…» раскрывает диски и каталоги машины агента,
  // показывает в дереве каталог сессии и заполняет поле выбранной папкой.
  await evaluate(`document.getElementById('launch-browse').click()`);
  const treeOpened = await waitFor(`(() => {
    const t = document.getElementById('launch-tree');
    return !t.hidden && t.querySelectorAll('.tree-row').length === 4;
  })()`);
  const treeOpenState = await evaluate(`(() => {
    const t = document.getElementById('launch-tree');
    return {
      expanded: document.getElementById('launch-browse').getAttribute('aria-expanded'),
      paths: [...t.querySelectorAll('.tree-row')].map((r) => r.dataset.path),
      selected: t.querySelector('.tree-row.selected')?.dataset.path || '',
      roots: [...t.querySelectorAll(':scope > .tree-node > .tree-row .tree-name')].map((n) => n.textContent),
      agent: window.__dirs[0]?.agent,
    };
  })()`);
  check(
    '«Обзор…» раскрывает дерево и показывает диски',
    treeOpened && treeOpenState.expanded === 'true' && treeOpenState.roots.length === 2,
    treeOpenState,
  );
  check(
    'дерево раскрывается до каталога сессии и выделяет его',
    treeOpenState.paths.includes('C:\\ui-test') && treeOpenState.selected === 'C:\\ui-test',
    treeOpenState,
  );
  check('дерево спрашивает папки у выбранного агента', treeOpenState.agent === 'ui-test', treeOpenState);

  // Папка раскрывается треугольником, вложенная — тоже, и её можно выбрать.
  await evaluate(`[...document.querySelectorAll('#launch-tree .tree-row')]
    .find((r) => r.querySelector('.tree-name').textContent === 'Projects')
    .querySelector('.tree-twist').click()`);
  const projectsOpen = await waitFor(`[...document.querySelectorAll('#launch-tree .tree-row')]
    .some((r) => r.querySelector('.tree-name').textContent === 'AiPanel')`);
  check('треугольник раскрывает вложенную папку', projectsOpen, await evaluate(`window.__dirs.map((m) => m.path)`));

  await evaluate(`[...document.querySelectorAll('#launch-tree .tree-row')]
    .find((r) => r.querySelector('.tree-name').textContent === 'AiPanel').click()`);
  const treePicked = await evaluate(`({
    cwd: document.getElementById('launch-cwd').value,
    selected: document.querySelector('#launch-tree .tree-row.selected')?.dataset.path || '',
    focused: document.activeElement?.dataset?.path || '',
  })`);
  check(
    'выбор папки в дереве заполняет поле каталога',
    treePicked.cwd === 'C:\\Projects\\demo' && treePicked.selected === 'C:\\Projects\\demo',
    treePicked,
  );
  check('выбранная строка получает фокус', treePicked.focused === 'C:\\Projects\\demo', treePicked);

  // Стрелки ходят по видимым строкам, как в проводнике: вверх — к родителю.
  await evaluate(`(() => {
    const row = document.activeElement;
    row.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, cancelable: true }));
  })()`);
  const treeArrow = await evaluate(`({
    cwd: document.getElementById('launch-cwd').value,
    selected: document.querySelector('#launch-tree .tree-row.selected')?.dataset.path || '',
  })`);
  check(
    'стрелка вверх переводит выбор на предыдущую папку',
    treeArrow.cwd === 'C:\\Projects' && treeArrow.selected === 'C:\\Projects',
    treeArrow,
  );

  // Влево сворачивает раскрытую папку, вправо раскрывает снова — уже без запроса.
  await evaluate(`document.activeElement.dispatchEvent(
    new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true, cancelable: true }))`);
  const collapsed = await waitFor(`![...document.querySelectorAll('#launch-tree .tree-row')]
    .some((r) => r.querySelector('.tree-name').textContent === 'AiPanel')`);
  await evaluate(`document.activeElement.dispatchEvent(
    new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }))`);
  const expandedAgain = await waitFor(`[...document.querySelectorAll('#launch-tree .tree-row')]
    .some((r) => r.querySelector('.tree-name').textContent === 'AiPanel')`);
  const dirRequests = await evaluate(`window.__dirs.map((m) => m.path)`);
  check('влево сворачивает, вправо раскрывает папку', collapsed && expandedAgain, { collapsed, expandedAgain });
  check('повторное раскрытие не дублирует запросы', new Set(dirRequests).size === dirRequests.length, dirRequests);

  // Недоступный диск: ошибка агента видна в дереве, а не теряется молча.
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('#launch-tree .tree-row')]
      .find((r) => r.dataset.path === 'D:\\\\');
    row.click();
    row.querySelector('.tree-twist').click();
  })()`);
  const treeError = await waitFor(`(() => {
    const note = document.querySelector('#launch-tree .tree-note.error');
    return note && note.textContent.includes('нет доступа');
  })()`);
  check('ошибка чтения каталога видна в дереве', treeError, await evaluate(`document.getElementById('launch-tree').textContent`));

  await evaluate(`(() => {
    document.getElementById('launch-browse').click();
    const c = document.getElementById('launch-cwd');
    c.value = 'C:/ui-test';
    c.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  const treeClosed = await evaluate(`({
    hidden: document.getElementById('launch-tree').hidden,
    expanded: document.getElementById('launch-browse').getAttribute('aria-expanded'),
  })`);
  check('повторное нажатие «Обзор…» прячет дерево', treeClosed.hidden && treeClosed.expanded === 'false', treeClosed);


  // Клик по пустому полю открывает весь список — набирать имя не обязательно.
  await evaluate(`document.getElementById('launch-model').click()`);
  const launchAll = await waitFor(`(() => {
    const l = document.querySelector('#launch-model ~ .sg-list');
    return l && !l.hidden && l.querySelectorAll('.sg-item').length === 3;
  })()`);
  const launchOrder = await evaluate(
    `[...document.querySelectorAll('#launch-model ~ .sg-list .sg-item b')].map((n) => n.textContent)`,
  );
  check('список моделей открывается без набора', launchAll, launchOrder);
  check('недавняя модель выше остальных', launchOrder[0] === 'GPT 6', launchOrder);

  // Пункта «по умолчанию» больше нет: без выбранной модели запуск не уходит.
  await evaluate(`document.getElementById('launch-form').requestSubmit()`);
  await sleep(100);
  const noModel = await evaluate(`({
    starts: window.__starts.length,
    hint: document.getElementById('launch-hint').textContent,
  })`);
  check('без выбранной модели запуск не уходит', noModel.starts === 0 && noModel.hint.includes('Выберите модель'), noModel);

  // Список моделей opencode приходит от агента и открывается при вводе.
  await evaluate(`(() => {
    const m = document.getElementById('launch-model');
    m.focus();
    m.value = 'deep';
    m.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const launchListed = await waitFor(`(() => {
    const l = document.querySelector('#launch-model ~ .sg-list');
    return l && !l.hidden && l.querySelectorAll('.sg-item').length === 1;
  })()`);
  const launchItem = await evaluate(`(() => {
    const l = document.querySelector('#launch-model ~ .sg-list');
    const item = l.querySelector('.sg-item');
    return { name: item.querySelector('b').textContent, hint: item.querySelector('small')?.textContent || '' };
  })()`);
  check(
    'в окне запуска есть модели opencode',
    launchListed && launchItem.name === 'DeepSeek V4.1 Flash' && launchItem.hint === 'opencode-go/deepseek-v4.1-flash',
    launchItem,
  );

  await evaluate(`document.querySelector('#launch-model ~ .sg-list .sg-item').click()`);
  await sleep(100);
  const modelValue = await evaluate(`document.getElementById('launch-model').value`);
  check('выбор из списка подставляет провайдер/модель', modelValue === 'opencode-go/deepseek-v4.1-flash', modelValue);

  // Своя строка не принимается: запуск уходит только со значением из списка.
  await evaluate(`(() => {
    const m = document.getElementById('launch-model');
    m.value = 'my-own-model';
    m.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await evaluate(`document.getElementById('launch-form').requestSubmit()`);
  await sleep(100);
  const rejected = await evaluate(`({
    starts: window.__starts.length,
    hint: document.getElementById('launch-hint').textContent,
  })`);
  check('произвольную модель запуск не принимает', rejected.starts === 0 && rejected.hint.includes('нет в списке'), rejected);

  await evaluate(`(() => {
    const m = document.getElementById('launch-model');
    m.value = 'opencode-go/deepseek-v4.1-flash';
    m.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await evaluate(`document.getElementById('launch-form').requestSubmit()`);
  await waitFor(`window.__starts.length === 1`);
  const start = await evaluate(`window.__starts[0]`);
  check(
    'запуск уходит с выбранной моделью',
    start.tool === 'opencode' && start.cwd.endsWith('ui-test') && start.model === 'opencode-go/deepseek-v4.1-flash',
    start,
  );
  await evaluate(`document.getElementById('launch').close()`);
  await sleep(100);

  // Каталога нет на машине агента: панель сначала спрашивает, создавать ли его,
  // и только по ответу повторяет запуск — уже с разрешением создать.
  await evaluate(`window.__noDir = true`);
  await evaluate(`document.getElementById('new-session').click()`);
  await waitFor(`document.getElementById('launch').open`);
  await evaluate(`(() => {
    const c = document.getElementById('launch-cwd');
    c.value = 'D:/Проекты/нет-такого';
    c.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await evaluate(`document.getElementById('launch-form').requestSubmit()`);
  const asked = await waitFor(`document.getElementById('mkdir').open`);
  const mkdirText = await evaluate(`document.getElementById('mkdir-text').textContent`);
  check('отсутствующий каталог — вопрос о создании', asked && mkdirText.includes('нет-такого'), mkdirText);
  await evaluate(`document.getElementById('mkdir-form').requestSubmit()`);
  await waitFor(`window.__starts.length === 3`);
  const retried = await evaluate(`window.__starts[2]`);
  check(
    'создание каталога разрешено повторным запуском',
    retried.create === true && retried.cwd.endsWith('нет-такого'),
    retried,
  );
  await evaluate(`(() => { window.__noDir = false; document.getElementById('launch').close(); })()`);
  await sleep(100);

  // Claude Code: список — алиасы, которые принимает --model.
  await evaluate(`document.getElementById('new-session').click()`);
  await waitFor(`document.getElementById('launch').open`);
  await evaluate(`(() => {
    const t = document.getElementById('launch-tool');
    t.value = 'claude';
    t.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  const claudeDefault = await evaluate(`document.getElementById('launch-model').value`);
  check('при смене инструмента своя модель (у Claude Code пустая)', claudeDefault === '', claudeDefault);
  await evaluate(`(() => {
    const m = document.getElementById('launch-model');
    m.focus();
    m.value = 'son';
    m.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const claudeListed = await waitFor(`(() => {
    const l = document.querySelector('#launch-model ~ .sg-list');
    return l && !l.hidden && l.querySelectorAll('.sg-item').length === 1;
  })()`);
  await evaluate(`document.querySelector('#launch-model ~ .sg-list .sg-item').click()`);
  await sleep(100);
  const claudeValue = await evaluate(`document.getElementById('launch-model').value`);
  check(
    'у Claude Code подставляется алиас модели',
    claudeListed && claudeValue === 'sonnet',
    { claudeListed, claudeValue },
  );
  await evaluate(`document.getElementById('launch').close()`);
  await sleep(100);

  // На телефоне список не должен уезжать за экран: поле ввода у нижней кромки,
  // поэтому список раскрывается вверх.
  await send('Emulation.setDeviceMetricsOverride', {
    width: 390,
    height: 740,
    deviceScaleFactor: 1,
    mobile: true,
  });
  // Кнопки шапки (со звездой избранного) не должны вылезти за экран.
  const narrowHeader = await evaluate(`(() => {
    const ids = ['back', 'favorites', 'status', 'interrupt', 'more'];
    const boxes = ids
      .map((id) => document.getElementById(id))
      .filter((n) => n && !n.hidden)
      .map((n) => ({ id: n.id, left: Math.round(n.getBoundingClientRect().left), right: Math.round(n.getBoundingClientRect().right) }));
    return { width: innerWidth, boxes, overflow: boxes.some((b) => b.right > innerWidth + 1 || b.left < -1) };
  })()`);
  check('на телефоне шапка со звездой избранного помещается в экран', !narrowHeader.overflow && narrowHeader.boxes.length >= 4, narrowHeader);
  await typeSlash('/');
  await waitFor(`(() => { const l = document.querySelector('#composer .sg-list'); return l && !l.hidden && l.querySelectorAll('.sg-item').length > 0; })()`);
  const narrow = await evaluate(`(() => {
    const list = document.querySelector('#composer .sg-list').getBoundingClientRect();
    const input = document.getElementById('text').getBoundingClientRect();
    return {
      listTop: list.top, listBottom: list.bottom, listLeft: list.left, listRight: list.right,
      height: list.height, inputTop: input.top, width: innerWidth, height0: innerHeight,
    };
  })()`);
  check(
    'на телефоне список целиком на экране и над полем',
    narrow.height > 0 &&
      narrow.listTop >= 0 &&
      narrow.listBottom <= narrow.height0 + 1 &&
      narrow.listLeft >= 0 &&
      narrow.listRight <= narrow.width + 1 &&
      narrow.listBottom <= narrow.inputTop + 1,
    narrow,
  );

  // На телефоне механика та же: выбор подставляет «/имя », параметр печатается дальше.
  await press('ArrowDown');
  await press('Enter');
  await sleep(100);
  const mobilePick = await slashState();
  await evaluate(`(() => {
    const t = document.getElementById('text');
    t.focus();
    t.value += '24';
    t.setSelectionRange(t.value.length, t.value.length);
    t.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await sleep(100);
  const mobileArgs = await slashState();
  check(
    'на телефоне команда с параметрами набирается так же',
    mobilePick.text === '/clear ' && !mobilePick.open && mobileArgs.text === '/clear 24' && !mobileArgs.open,
    { mobilePick, mobileArgs },
  );

  // Окно выбора модели на телефоне тоже должно помещаться в экран.
  await typeSlash('/mod');
  await waitFor(`(() => { const l = document.querySelector('#composer .sg-list'); return l && !l.hidden && l.querySelectorAll('.sg-item').length === 1; })()`);
  await press('ArrowDown');
  await press('Enter');
  await sleep(150);
  const narrowDialog = await evaluate(`(() => {
    const dlg = document.getElementById('models');
    const box = dlg.getBoundingClientRect();
    return {
      open: dlg.open,
      rows: dlg.querySelectorAll('.model').length,
      top: box.top,
      bottom: box.bottom,
      vh: innerHeight,
    };
  })()`);
  check(
    'на телефоне окно моделей целиком на экране',
    narrowDialog.open && narrowDialog.rows === 3 && narrowDialog.top >= 0 && narrowDialog.bottom <= narrowDialog.vh + 1,
    narrowDialog,
  );
  await evaluate(`document.getElementById('models').close()`);

// ------------------------------------------------------------ картинки по ссылкам
  // Ссылка на картинку: по клику под ней раскрывается картинка, повторный клик
  // сворачивает; при перестройке сообщения раскрытая картинка остаётся открытой.
  const img = await evaluate(`(async () => {
    const src = location.origin + '/icon-192.png';
    const make = () => {
      const box = document.createElement('div');
      box.className = 'body md';
      box.append(renderMarkdown('Вот фото: ' + src + ' и ещё ![схема](' + src + '?v=2), и [страница](' + location.origin + '/)'));
      return box;
    };
    const box = make();
    document.getElementById('feed').append(box);
    const links = [...box.querySelectorAll('a.img-link')];
    const before = box.querySelectorAll('.img-preview').length;
    const click = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 });
    links[0].dispatchEvent(click);
    const preview = box.querySelector('.img-preview img');
    await new Promise((r) => (preview.complete ? r() : preview.addEventListener('load', r, { once: true })));
    const shown = preview.getBoundingClientRect().height > 0 && preview.naturalWidth > 0;
    const prevented = click.defaultPrevented;
    const rebuilt = make();
    renderMarkdown.keepOpenImages(box, rebuilt);
    const kept = rebuilt.querySelector('a.img-link').getAttribute('aria-expanded') === 'true' &&
      !rebuilt.querySelector('.img-preview').hidden;
    links[0].dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 }));
    const hidden = box.querySelector('.img-preview').hidden;
    box.remove();
    return { links: links.length, before, shown, prevented, kept, hidden };
  })()`);
  check('ссылки на картинки распознаны, обычная ссылка нет', img.links === 2, img);
  check('картинка не грузится до клика', img.before === 0, img);
  check('клик раскрывает картинку и не уходит по ссылке', img.shown && img.prevented, img);
  check('при перестройке сообщения картинка остаётся раскрытой', img.kept, img);
  check('повторный клик сворачивает картинку', img.hidden, img);

  // Раскрытая картинка открывается на весь экран тем же просмотром, что и
  // снимок экрана: один кадр, подпись с размером и без кнопки «Обновить».
  const fullImage = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const src = location.origin + '/icon-192.png';
    const box = document.createElement('div');
    box.className = 'body md';
    box.append(renderMarkdown('![схема](' + src + ')'));
    document.getElementById('feed').append(box);
    box.querySelector('a.img-link').dispatchEvent(
      new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 }));
    const preview = box.querySelector('.img-preview img');
    await new Promise((r) => (preview.complete ? r() : preview.addEventListener('load', r, { once: true })));
    const click = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 });
    preview.dispatchEvent(click);
    const dialog = document.getElementById('shot');
    // Размер картинки появляется в подписи после её загрузки уже в просмотре.
    const until = Date.now() + 2000;
    while (Date.now() < until && !/192×192/.test(document.getElementById('shot-label').textContent)) await wait(20);
    const viewer = document.querySelector('#shot-strip .shot-page img');
    const rect = dialog.getBoundingClientRect();
    const state = {
      open: dialog.open,
      full: Math.round(rect.width) === innerWidth && Math.round(rect.height) === innerHeight,
      prevented: click.defaultPrevented,
      label: document.getElementById('shot-label').textContent,
      refresh: document.getElementById('shot-refresh').hidden,
      pages: document.querySelectorAll('#shot-strip .shot-page').length,
    };
    // В просмотре картинки колесо приближает без Ctrl: прокручивать нечего.
    const r = document.getElementById('shot-stage').getBoundingClientRect();
    document.getElementById('shot-stage').dispatchEvent(new WheelEvent('wheel', {
      bubbles: true, cancelable: true, deltaY: -200,
      clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
    }));
    state.transform = viewer.style.transform;
    state.zoomed = document.getElementById('shot-label').textContent;
    document.getElementById('shot-close').click();
    state.closed = !dialog.open;
    box.remove();
    return state;
  })()`);
  check('картинка открывается на весь экран поверх ленты',
    fullImage.open && fullImage.full && fullImage.prevented, fullImage);
  check('просмотр картинки: один кадр, размер в подписи и без «Обновить»',
    fullImage.pages === 1 && fullImage.label === 'схема · 192×192' && fullImage.refresh, fullImage);
  check('просмотр картинки: колесо приближает без Ctrl',
    /scale\(/.test(fullImage.transform) && /· \d+%$/.test(fullImage.zoomed), fullImage);
  check('просмотр картинки закрывается крестиком', fullImage.closed, fullImage);

  // Пути к картинкам на машине агента: в тексте, в `коде` и в ссылке. Картинку
  // отдаёт /api/image с агентом и сессией ленты; ошибка видна текстом.
  const local = await evaluate(`(async () => {
    const opts = { localImage: (p) => '/api/image?' + new URLSearchParams({ agent: 'ui-test', session: 's1', path: p }) };
    const box = document.createElement('div');
    box.className = 'body md';
    box.append(renderMarkdown([
      'Скриншот: D:\\\\Projects\\\\demo\\\\shot.png.',
      'В коде: \`C:\\\\My Pictures\\\\shot.png\` и [тут](./out/missing.png).',
      'Не путь: \`convert a.png b.png\`, адрес https://example.com/x.html и слово png.',
    ].join('\\n\\n'), opts));
    document.getElementById('feed').append(box);
    const links = [...box.querySelectorAll('a.img-link')];
    const paths = links.map((a) => new URL(a.href).searchParams.get('path'));
    const plain = renderMarkdown('D:\\\\x\\\\shot.png').querySelectorAll('a').length;
    const open = async (a) => {
      a.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 }));
      const until = Date.now() + 4000;
      while (Date.now() < until) {
        const p = a.nextElementSibling;
        const img = p && p.querySelector('img');
        if (img && img.complete && img.naturalWidth > 0) return 'img';
        if (p && /Не удалось/.test(p.textContent)) return p.textContent;
        await new Promise((r) => setTimeout(r, 50));
      }
      return 'timeout';
    };
    const first = await open(links[0]);
    const missing = await open(links[2]);
    const codeWrapped = !!links[1].querySelector('code');
    box.remove();
    return { paths, plain, first, missing, codeWrapped };
  })()`);
  check(
    'пути к картинкам распознаны в тексте, коде и ссылке',
    JSON.stringify(local.paths) ===
      JSON.stringify(['D:\\Projects\\demo\\shot.png', 'C:\\My Pictures\\shot.png', './out/missing.png']),
    local,
  );
  check('без адреса агента путь остаётся текстом', local.plain === 0, local);
  check('путь в коде остаётся кодом внутри ссылки', local.codeWrapped, local);
  check('клик по пути показывает картинку с агента', local.first === 'img', local);
  check('ошибка загрузки видна текстом', /файл недоступен/.test(local.missing), local);
  check(
    'запрос картинки идёт с агентом и сессией',
    imageRequests.length >= 1 && imageRequests[0].agent === 'ui-test' && imageRequests[0].session === 's1',
    imageRequests,
  );

  // Картинки из вызова и вывода инструмента: путь в JSON аргументов (там он
  // экранирован) и в тексте вывода видны ссылками под строкой инструмента.
  const tool = await evaluate(`(() => {
    const opts = { localImage: (p) => '/api/image?' + new URLSearchParams({ agent: 'ui-test', session: 's1', path: p }) };
    const paths = (box) => box ? [...box.querySelectorAll('a.img-link')].map((a) =>
      a.href.includes('/api/image') ? new URL(a.href).searchParams.get('path') : a.href) : null;
    const input = renderMarkdown.imageLinks(JSON.stringify({ file_path: 'C:\\\\My Pictures\\\\shot.png', limit: 5 }), opts);
    const output = renderMarkdown.imageLinks(
      'Saved screenshot to D:\\\\tmp\\\\a.png\\nsee https://example.com/b.jpg and D:\\\\tmp\\\\a.png again', opts);
    const none = renderMarkdown.imageLinks('{"command":"ls -la"}', opts);
    return { input: paths(input), output: paths(output), none: none === null };
  })()`);
  check('путь из аргументов инструмента стал ссылкой', JSON.stringify(tool.input) === JSON.stringify(['C:\\My Pictures\\shot.png']), tool);
  check(
    'пути и адреса из вывода инструмента без повторов',
    JSON.stringify(tool.output) === JSON.stringify(['D:\\tmp\\a.png', 'https://example.com/b.jpg']),
    tool,
  );
  check('без картинок под инструментом ничего не добавляется', tool.none, tool);

  // Строже на Windows: `/icon.svg` — путь сайта, а не файла; имя без каталога в
  // листинге — не ссылка. Путь целиком (JSON-аргумент) и путь с каталогом — ссылки.
  const strict = await evaluate(`(() => {
    const localImage = (p) => '/api/image?' + new URLSearchParams({ agent: 'ui-test', session: 's1', path: p });
    const opts = { windows: true, localImage };
    const paths = (box) => box ? [...box.querySelectorAll('a.img-link')].map((a) =>
      new URL(a.href).searchParams.get('path')) : null;
    const prose = document.createElement('div');
    prose.append(renderMarkdown('Сайт: /icon.svg, файл: docs/screen-feed.png, имя: shot.png.', opts));
    const listing = renderMarkdown.imageLinks(
      'docs:\\nscreen-feed.png\\nicon.svg\\nC:\\\\tmp\\\\shot.png', opts);
    const arg = renderMarkdown.imageLinks(JSON.stringify({ file_path: 'aipanel-sessions.png' }), opts);
    const linux = document.createElement('div');
    linux.append(renderMarkdown('/icon.svg', { localImage }));
    return { prose: paths(prose), listing: paths(listing), arg: paths(arg), linux: paths(linux) };
  })()`);
  check(
    'на Windows сайтовый путь и имя без каталога не ссылки',
    JSON.stringify(strict.prose) === JSON.stringify(['docs/screen-feed.png']),
    strict,
  );
  check(
    'в листинге ссылка только на путь с каталогом',
    JSON.stringify(strict.listing) === JSON.stringify(['C:\\tmp\\shot.png']),
    strict,
  );
  check('путь целиком из аргумента инструмента — ссылка', JSON.stringify(strict.arg) === JSON.stringify(['aipanel-sessions.png']), strict);
  check('на Linux путь с ведущим / по-прежнему ссылка', JSON.stringify(strict.linux) === JSON.stringify(['/icon.svg']), strict);

  const toolItem = await evaluate(`(() => {
    const node = buildItem({ id: 'img-tool', ts: 0, role: 'assistant', kind: 'tool_use', tool: 'Read',
      text: JSON.stringify({ file_path: 'D:\\\\tmp\\\\shot.png' }) }, null);
    document.getElementById('feed').append(node);
    const link = node.querySelector('.tool-images a.img-link');
    const r = {
      collapsed: !node.querySelector('details').open,
      visible: !!link && link.getBoundingClientRect().height > 0,
      session: link ? new URL(link.href).searchParams.get('session') : '',
    };
    node.remove();
    return r;
  })()`);
  check(
    'картинка инструмента видна под свёрнутым вызовом',
    toolItem.collapsed && toolItem.visible && toolItem.session === session.key,
    toolItem,
  );

  // Вопрос Claude с вариантами: в ленте текст вопросов, а не JSON аргументов;
  // обрезанный JSON показывается как обычный вызов.
  const askItem = await evaluate(`(() => {
    const input = { questions: [
      { question: 'Где хранить корни?', header: 'Доверие', multiSelect: false,
        options: [{ label: 'В репо', description: 'прозрачно' }, { label: 'Из PFX' }] },
      { question: 'Какие тесты?', header: 'Тесты', multiSelect: true, options: [{ label: 'Юнит' }] },
    ] };
    const node = buildItem({ id: 'ask', ts: 0, role: 'assistant', kind: 'tool_use', tool: 'AskUserQuestion',
      text: JSON.stringify(input) }, null);
    const cut = buildItem({ id: 'ask2', ts: 0, role: 'assistant', kind: 'tool_use', tool: 'AskUserQuestion',
      text: '{"questions":[{"question":"Обре' }, null);
    return {
      label: node.querySelector('summary').textContent,
      body: node.querySelector('.body').textContent,
      cut: cut.querySelector('summary').textContent,
    };
  })()`);
  check('вопрос Claude: строка ленты', askItem.label === 'Вопрос: Где хранить корни? (и ещё 1)', askItem);
  check(
    'вопрос Claude: варианты текстом',
    askItem.body.includes('Доверие: Где хранить корни?') && askItem.body.includes('  • В репо — прозрачно')
      && askItem.body.includes('(можно выбрать несколько)') && !askItem.body.includes('"questions"'),
    askItem,
  );
  check('вопрос Claude: обрезанный JSON — обычный вызов', askItem.cut.startsWith('AskUserQuestion: {'), askItem);

  // Уведомление Claude Code о фоновой задаче — служебная строка, не своё сообщение.
  const notice = await evaluate(`(() => {
    const text = '<task-notification>\\n<status>completed</status>\\n<summary>Background command "x" completed (exit code 0)</summary>\\n</task-notification>';
    const node = buildItem({ id: 'n1', ts: 0, role: 'system', kind: 'notice', text }, null);
    const failed = buildItem({ id: 'n2', ts: 0, role: 'system', kind: 'notice',
      text: '<task-notification><status>failed</status><summary>Сборка</summary></task-notification>' }, null);
    document.getElementById('feed').append(node);
    const details = node.querySelector('details');
    const collapsed = !details.open;
    details.open = true;
    details.dispatchEvent(new Event('toggle'));
    const body = node.querySelector('.body');
    const r = {
      cls: node.className,
      label: node.querySelector('summary').textContent,
      collapsed,
      highlighted: body.classList.contains('hl') && body.querySelectorAll('span').length > 0,
      sameText: body.textContent === text,
      failed: failed.querySelector('summary').textContent,
      okClass: node.querySelector('summary').className,
      failedClass: failed.querySelector('summary').className,
    };
    node.remove();
    return r;
  })()`);
  check(
    'уведомление: плашка с итогом, свёрнута',
    notice.cls === 'item system notice' && notice.label === 'Background command "x" completed (exit code 0)'
      && notice.collapsed,
    notice,
  );
  check('уведомление: при раскрытии XML подсвечен, текст тот же', notice.highlighted && notice.sameText, notice);
  check('уведомление: неуспешный статус виден', notice.failed === 'Сборка (failed)', notice);
  check('уведомление: неуспешное — красная строка, успешное — обычная',
    notice.failedClass === 'failed' && notice.okClass === '', notice);

  // Своё сообщение из ленты попадает в историю запросов, уведомление — нет.
  // Раньше одноимённая функция истории отправленного затирала эту.
  const requests = await evaluate(`(() => {
    const own = rememberRequest({ id: 'rq1', role: 'user', kind: 'text', text: 'мой запрос' });
    const note = rememberRequest({ id: 'rq2', role: 'system', kind: 'notice', text: '<task-notification/>' });
    const r = { own, note, stored: feedRequests.get('rq1') };
    feedRequests.delete('rq1');
    return r;
  })()`);
  check('история запросов: своё сообщение из ленты, без уведомлений',
    requests.own === true && requests.note === false && requests.stored === 'мой запрос', requests);

  // Подсветка: результат Read Claude Code (вызов и результат — разные id),
  // результат bash opencode (`id:r`), дифф правки и блок кода в ответе.
  const hl = await evaluate(`(async () => {
    const extra = [
      { id: 'h1:0', role: 'assistant', kind: 'tool_use', tool: 'Read', text: 'D:\\\\p\\\\Main.kt' },
      { id: 'h2:0', role: 'tool', kind: 'tool_result', text: '     1\\tfun main() {}\\n     2\\tval x = "s"\\n' },
      { id: 'h3:1', role: 'assistant', kind: 'tool_use', tool: 'bash', text: 'git diff' },
      { id: 'h3:1:r', role: 'tool', kind: 'tool_result',
        text: 'diff --git a/a.rs b/a.rs\\n--- a/a.rs\\n+++ b/a.rs\\n@@ -1 +1 @@\\n-fn a() {}\\n+fn b() {}\\n' },
      { id: 'h4:diff', role: 'assistant', kind: 'diff', text: '--- a/x.py\\n+++ b/x.py\\n@@ -1,1 +1,1 @@\\n-def a(): pass\\n+def b(): pass\\n' },
      { id: 'h5:0', role: 'assistant', kind: 'text', text: 'Код:\\n\\n\`\`\`kotlin\\nval x = 1\\n\`\`\`' },
    ];
    applyHistory({ items: extra });
    const item = (id) => document.querySelector('#feed .item[data-id="' + id + '"]');
    const read = item('h2:0').querySelector('details');
    const plainBefore = !read.querySelector('.hl-row');
    read.open = true;
    const cmd = item('h3:1').querySelector('details');
    cmd.open = true;
    const gitDiff = item('h3:1:r').querySelector('details');
    gitDiff.open = true;
    await new Promise((r) => setTimeout(r, 50));
    const words = (root, cls) => [...root.querySelectorAll('.hl-' + cls)].map((s) => s.textContent);
    const out = {
      plainBefore,
      readRows: read.querySelectorAll('.hl-row').length,
      readNums: [...read.querySelectorAll('.hl-ln')].map((s) => s.textContent),
      readKw: words(read, 'k'),
      readText: read.querySelector('.body').textContent,
      cmdFn: words(cmd, 'f'),
      gitDiffKw: words(gitDiff, 'k'),
      gitDiffAdd: gitDiff.querySelectorAll('.diff-line.add').length,
      diffKw: words(item('h4:diff'), 'k'),
      diffRow: item('h4:diff').querySelector('.diff-line.add').textContent,
      mdKw: words(item('h5:0'), 'k'),
    };
    applyHistory({ items: [], drop: extra.map((x) => x.id) });
    return out;
  })()`);
  check('результат подсвечивается только при раскрытии', hl.plainBefore, hl);
  check('Read: строки с номерами по языку файла', hl.readRows === 2 && hl.readNums.join() === '1,2' &&
    JSON.stringify(hl.readKw) === '["fun","val"]', hl);
  check('Read: текст кода без номеров', hl.readText === '1fun main() {}2val x = "s"', hl);
  check('вызов bash подсвечен как команда', hl.cmdFn.includes('git'), hl);
  check('git diff в результате — дифф с подсветкой', hl.gitDiffAdd === 1 && hl.gitDiffKw.join() === 'fn,fn', hl);
  check('дифф правки подсвечен по языку файла', hl.diffKw.join() === 'def,pass,def,pass' && hl.diffRow === '+def b(): pass', hl);
  check('блок кода в ответе подсвечен', hl.mdKw.join() === 'val', hl);

  // Время вызова инструмента: пока команда идёт, под её строкой тикает таймер
  // (справа снизу), а после результата он пропадает и длительность показывается
  // справа от строки — только пока на неё наведена мышь.
  const toolTime = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const load = (patch) => window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test',
      sessions: [{ ...window.__session, ...patch }] }] });
    const start = Date.now() - 12000;
    const call = { id: 't1', ts: start, role: 'assistant', kind: 'tool_use', tool: 'Bash', text: 'npm run build' };
    // Идущая команда: сессия занята, результата ещё нет.
    load({ status: 'shell' });
    applyHistory({ items: [call] });
    await wait(30);
    const node = document.querySelector('#feed .item[data-id="t1"]');
    const timer = node && node.querySelector('.call-timer');
    const time = node && node.querySelector('.call-time');
    const s = node && node.querySelector('summary');
    const tb = timer && timer.getBoundingClientRect();
    const sb = s && s.getBoundingClientRect();
    const running = {
      timer: !!timer,
      text: timer ? timer.textContent : '',
      under: !!(tb && sb) && tb.top >= sb.bottom - 1,
      right: !!(tb && sb) && Math.abs(tb.right - node.getBoundingClientRect().right) < 12,
      timeHidden: time ? time.hidden : null,
    };
    // Результат пришёл: команда шла 45 секунд.
    applyHistory({ items: [{ ...call }, { id: 't1:r', ts: start + 45000, role: 'tool', kind: 'tool_result', text: 'готово' }] });
    await wait(30);
    const after = document.querySelector('#feed .item[data-id="t1"]');
    const done = {
      timer: after.querySelector('.call-timer') === null,
      time: after.querySelector('.call-time').textContent,
      timeHidden: after.querySelector('.call-time').hidden,
    };
    // Вопрос Claude не измеряется: он ждёт человека, а не работает.
    applyHistory({ items: [{ id: 'q1', ts: start, role: 'assistant', kind: 'tool_use', tool: 'AskUserQuestion',
      text: JSON.stringify({ questions: [{ question: 'Что делать?' }] }) }] });
    await wait(30);
    const ask = document.querySelector('#feed .item[data-id="q1"]');
    const askTimed = !!(ask.querySelector('.call-time') || ask.querySelector('.call-timer'));
    return { running, done, askTimed };
  })()`);
  check('идущая команда: таймер под строкой справа', toolTime.running.timer && toolTime.running.under && toolTime.running.right, toolTime.running);
  check('идущая команда: таймер считает её время', /^1[1-3]с$/.test(toolTime.running.text), toolTime.running);
  check('идущая команда: времени справа от строки ещё нет', toolTime.running.timeHidden, toolTime.running);
  check('команда закончилась: таймер пропал, время появилось', toolTime.done.timer && !toolTime.done.timeHidden && toolTime.done.time === '45с', toolTime.done);
  check('вопрос Claude временем не измеряется', !toolTime.askTimed, toolTime.askTimed);

  const opacityOf = () => evaluate(`getComputedStyle(document.querySelector('#feed .item[data-id="t1"] .call-time')).opacity`);
  const hoverAt = await evaluate(`(() => {
    const node = document.querySelector('#feed .item[data-id="t1"]');
    node.scrollIntoView({ block: 'center' });
    const b = node.querySelector('summary').getBoundingClientRect();
    return { x: Math.round(b.left + b.width / 2), y: Math.round(b.top + b.height / 2) };
  })()`);
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: hoverAt.x, y: hoverAt.y });
  await sleep(300);
  const hoveredOpacity = await opacityOf();
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 5, y: 5 });
  await sleep(300);
  const awayOpacity = await opacityOf();
  check('наведение на строку показывает время команды', hoveredOpacity === '1', hoveredOpacity);
  check('без мыши время пропадает', awayOpacity === '0', awayOpacity);

  await evaluate(`(() => {
    applyHistory({ items: [], drop: ['t1', 't1:r', 'q1'] });
    window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [window.__session] }] });
  })()`);

  // Уведомления: конец хода и ожидание выбора. Вторая сессия нужна, чтобы
  // уведомления не гасились как «пользователь смотрит именно эту сессию».
  const notices = await evaluate(`(() => {
    const roster = (status) => [{ name: 'ui-test', online: true, os: 'test', version: 'test',
      sessions: [{ ...window.__session, key: 'opencode:notice', title: 'Уведомления', status }] }];
    const load = (status) => window.__sock.emit({ t: 'agents', agents: roster(status) });
    const count = () => window.__notices.length;
    load('busy');
    const busy = count();
    load('idle');
    const done = window.__notices.slice(busy).map((n) => n.body || '');
    load('waiting');
    const waitingDot = document.querySelector('#sessions .session .dot').className;
    const waiting = window.__notices.slice(busy + done.length).map((n) => n.body || '');
    load('idle');
    const afterAnswer = count() - busy - done.length - waiting.length;
    window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [window.__session] }] });
    return { busy, done, waitingDot, waiting, afterAnswer };
  })()`);
  check('занятая сессия уведомлений не шлёт', notices.busy === 0, notices);
  check('конец хода — уведомление браузера', notices.done.length === 1 && notices.done[0].includes('ход завершён'), notices);
  check('ожидание выбора — красный кружок, не жёлтый', notices.waitingDot === 'dot waiting', notices);
  check('ожидание выбора — уведомление браузера', notices.waiting.length === 1 && notices.waiting[0].includes('ждёт ответа'), notices);
  check('от ответа на выбор уведомления нет', notices.afterAnswer === 0, notices);

  // Открытие сессии гасит её уведомление в строке состояния: и страничное, и
  // push, который показал service worker. Чужие уведомления при этом остаются.
  const closedNotices = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const pushed = [
      { tag: 'aipanel:ui-test:opencode:notice', closed: false, close() { this.closed = true; } },
      { tag: 'aipanel:ui-test:opencode:other', closed: false, close() { this.closed = true; } },
    ];
    Object.defineProperty(navigator, 'serviceWorker', {
      configurable: true,
      value: { ready: Promise.resolve({ getNotifications: () => Promise.resolve(pushed) }) },
    });
    const before = window.__notices.length;
    showNotice({ name: 'ui-test' }, { key: 'opencode:notice', title: 'Уведомления' }, 'done');
    window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test',
      sessions: [{ ...window.__session, key: 'opencode:notice', title: 'Уведомления', status: 'idle' }] }] });
    const shown = window.__notices.length - before;
    document.querySelector('#sessions .session').click();
    await wait(20);
    const result = { shown, closed: window.__closed.slice(), mine: pushed[0].closed, other: pushed[1].closed };
    closeView();
    window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [window.__session] }] });
    return result;
  })()`);
  check('открытие сессии убирает её уведомление',
    closedNotices.shown === 1 && closedNotices.closed.includes('aipanel:ui-test:opencode:notice')
      && closedNotices.mine, closedNotices);
  check('уведомление другой сессии остаётся', !closedNotices.other, closedNotices);

  // Шапка: после версии панели через слеш — версия агентов в сети (major.minor).
  const whoLine = await evaluate(`(() => {
    const roster = (list) => window.__sock.emit({ t: 'agents', agents: list.map(([name, version, online]) =>
      ({ name, online, os: 'test', version, sessions: [window.__session] })) });
    who = { user: 'tester', version: '9.15' };
    roster([['ui-test', '9.15.0', true]]);
    const one = document.getElementById('user').textContent;
    roster([['ui-test', '9.15.0', true], ['laptop', '9.14.0', true], ['old', '1.0.0', false]]);
    const two = document.getElementById('user').textContent;
    const tip = document.getElementById('user').title;
    roster([['ui-test', 'test', true]]);
    return { one, two, tip };
  })()`);
  check('шапка: версия агента через слеш', whoLine.one === 'tester · 9.15 / 9.15', whoLine);
  check('шапка: разные версии агентов через запятую, не в сети — не считаются',
    whoLine.two === 'tester · 9.15 / 9.15, 9.14' && whoLine.tip.includes('Агент laptop: 9.14') && !whoLine.tip.includes('old'), whoLine);

  // Строка хода: показываем только то, что прислал агент.
  const turnLine = await evaluate(`(() => {
    const load = (patch) => window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test',
      sessions: [{ ...window.__session, ...patch }] }] });
    const read = () => { const b = document.getElementById('turn'); return b.hidden ? null : b.textContent; };
    // Блок уведомлений закрыл открытую сессию: открываем её снова.
    load({});
    document.querySelector('#sessions .session').click();
    load({ status: 'busy', turn: { started_at: Date.now() - 34000, input: 59319, output: 1234 } });
    const full = read();
    load({ status: 'busy', turn: { started_at: Date.now() - 5000 } });
    const bare = read();
    load({ status: 'busy' });
    const noData = read();
    load({ status: 'idle', turn: { started_at: Date.now() - 5000, input: 10, output: 10 } });
    const idle = read();
    load({});
    return { full, bare, noData, idle };
  })()`);
  check('строка хода: время и оба счётчика', /^✻3[3-6]с↑ 59\.3k↓ 1\.23k$/.test(turnLine.full || ''), turnLine);
  check('строка хода: до первого ответа токенов нет', /^✻[4-6]с$/.test(turnLine.bare || ''), turnLine);
  const elapsed = await evaluate(`[fmtElapsed(5000), fmtElapsed(125000), fmtElapsed(3725000), fmtElapsed(61285000)]`);
  check('строка хода: секунды, минуты и часы', elapsed.join('|') === '5с|2м 05с|1ч 02м|17ч 01м', elapsed);
  check('строка хода: нет данных агента — строки нет', turnLine.noData === null, turnLine);
  check('строка хода: сессия не работает — строки нет', turnLine.idle === null, turnLine);

  // Каталог сессии: для git — репозиторий и ветка, путь — в подсказке.
  const place = await evaluate(`(() => {
    const load = (patch) => window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test',
      sessions: [{ ...window.__session, ...patch }] }] });
    const meta = () => document.querySelector('#sessions .session .meta');
    load({ cwd: 'C:\\\\wt\\\\refactor-split', repo: { name: 'ai-panel', url: 'git.example.ru/root/ai-panel', branch: 'refactor-master', worktree: true, root: 'D:\\\\Projects\\\\demo' } });
    document.querySelector('#sessions .session').click();
    const out = {
      list: meta().textContent,
      listTip: meta().title,
      sub: document.getElementById('subtitle').textContent,
      subTip: document.getElementById('subtitle').title,
    };
    load({ cwd: 'D:\\\\Записная книжка\\\\Баланс' });
    out.plain = meta().textContent;
    const node = buildItem({ id: 'x:cwd', ts: 1, role: 'system', kind: 'cwd', text: 'C:\\\\wt\\\\refactor-split' }, null);
    out.mark = node.textContent;
    out.markClass = node.querySelector('.cwd-switch') !== null;
    load({});
    return out;
  })()`);
  check('каталог: в списке репозиторий и ветка', place.list === 'opencode · ai-panel ⎇ refactor-master', place);
  check('каталог: путь и основная копия — в подсказке',
    place.listTip.includes('C:\\wt\\refactor-split') && place.listTip.includes('worktree репозитория D:\\Projects\\demo'), place);
  check('каталог: в шапке адрес репозитория', place.sub.endsWith('git.example.ru/root/ai-panel ⎇ refactor-master') && place.subTip.includes('refactor-split'), place);
  check('каталог: без git — имя папки', place.plain === 'opencode · Баланс', place);
  check('лента: отметка о смене каталога', place.markClass && place.mark.includes('Каталог сменился') && place.mark.includes('refactor-split'), place);

  // Значок панели в шапке: снимок экрана машины агента поверх панели. Агент
  // отдаёт два экрана — панель показывает первый, остальные листаются.
  const shotPng =
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==';
  // Закрытый просмотр не должен занимать экран: у полноэкранного <dialog> стиль
  // автора перебивает браузерное `display: none`, и тёмный экран оставался бы
  // поверх панели. Проверяем до открытия — панель должна быть видна.
  const shotClosed = await evaluate(`(() => {
    const dialog = document.getElementById('shot');
    const hit = document.elementFromPoint(Math.round(innerWidth / 2), Math.round(innerHeight / 2));
    return {
      open: dialog.open,
      display: getComputedStyle(dialog).display,
      covers: dialog.contains(hit),
      center: hit ? (hit.id || hit.className || hit.tagName) : '',
    };
  })()`);
  check('значок: закрытый снимок не закрывает панель',
    !shotClosed.open && shotClosed.display === 'none' && !shotClosed.covers, shotClosed);
  const shotFlow = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    window.__screens = [];
    window.__screenShots = [
      { label: 'Экран 1 · основной', width: 3840, height: 2160, data: '${shotPng}' },
      { label: 'Экран 2', width: 2560, height: 1440, data: '${shotPng}' },
    ];
    document.getElementById('shots').click();
    await wait(60);
    const dialog = document.getElementById('shot');
    const rect = dialog.getBoundingClientRect();
    const label = () => document.getElementById('shot-label').textContent;
    const dots = () => [...document.querySelectorAll('#shot-dots .shot-dot')];
    const first = {
      open: dialog.open,
      agent: window.__screens.map((m) => m.agent).join(),
      pages: document.querySelectorAll('#shot-strip .shot-page').length,
      imgs: document.querySelectorAll('#shot-strip img').length,
      label: label(),
      dots: dots().length,
      prev: document.getElementById('shot-prev').hidden,
      next: document.getElementById('shot-next').hidden,
      full: Math.round(rect.width) === innerWidth && Math.round(rect.height) === innerHeight,
    };
    document.getElementById('shot-next').click();
    const second = {
      label: label(),
      prev: document.getElementById('shot-prev').hidden,
      next: document.getElementById('shot-next').hidden,
      active: dots().findIndex((d) => d.classList.contains('active')),
      shift: document.getElementById('shot-strip').style.transform,
    };
    // Стрелка влево с клавиатуры возвращает на первый экран.
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    const back = label();
    return { first, second, back };
  })()`);
  check('значок: снимок открывается поверх панели на весь экран', shotFlow.first.open && shotFlow.first.full, shotFlow.first);
  check('значок: снимает компьютер открытой сессии', shotFlow.first.agent === 'ui-test', shotFlow.first);
  check('значок: страниц, картинок и точек столько же, сколько экранов',
    shotFlow.first.pages === 2 && shotFlow.first.imgs === 2 && shotFlow.first.dots === 2, shotFlow.first);
  check('значок: подпись экрана с размером', shotFlow.first.label === 'Экран 1 · основной · 3840×2160', shotFlow.first);
  check('значок: на первом экране «назад» скрыто', shotFlow.first.prev && !shotFlow.first.next, shotFlow.first);
  check('значок: «вперёд» листает и ведёт точку',
    shotFlow.second.label === 'Экран 2 · 2560×1440' && !shotFlow.second.prev && shotFlow.second.next &&
      shotFlow.second.active === 1 && shotFlow.second.shift === 'translateX(-100%)', shotFlow.second);
  check('значок: стрелка влево с клавиатуры возвращает назад', shotFlow.back === shotFlow.first.label, shotFlow);
  checkIcons(await iconOffsets(['#shot-close', '#shot-next']));

  // Свайп влево по снимку — следующий экран.
  const stageBox = await evaluate(`(() => {
    const r = document.getElementById('shot-stage').getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  })()`);
  const swipeY = Math.round(stageBox.y + stageBox.h / 2);
  const swipeFrom = Math.round(stageBox.x + stageBox.w * 0.8);
  const swipeTo = Math.round(stageBox.x + stageBox.w * 0.2);
  await send('Input.dispatchMouseEvent', { type: 'mousePressed', x: swipeFrom, y: swipeY, button: 'left', clickCount: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: swipeTo, y: swipeY, button: 'left', buttons: 1 });
  await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: swipeTo, y: swipeY, button: 'left', clickCount: 1 });
  await sleep(80);
  const swipeLabel = await evaluate(`document.getElementById('shot-label').textContent`);
  check('значок: свайп влево листает экраны', swipeLabel === 'Экран 2 · 2560×1440', swipeLabel);
  checkIcons(await iconOffsets(['#shot-prev']));

  // Щипок двумя пальцами приближает снимок, приближённый кадр двигается одним
  // пальцем, а соседний экран снова показывается целиком. Картинка нужна
  // настоящего размера: у пиксельной нечего приближать и некуда двигать.
  const zoomBox = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const canvas = document.createElement('canvas');
    canvas.width = 1200;
    canvas.height = 800;
    const ctx = canvas.getContext('2d');
    ctx.fillStyle = '#2f4a6d';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    const data = canvas.toDataURL('image/png').split(',')[1];
    window.__screenShots = [
      { label: 'Экран 1', width: 1200, height: 800, data },
      { label: 'Экран 2', width: 800, height: 600, data },
    ];
    document.getElementById('shot-refresh').click();
    await wait(120);
    const r = document.getElementById('shot-stage').getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  })()`);
  await waitFor(`(() => {
    const img = document.querySelector('#shot-strip .shot-page img');
    return !!img && img.complete && img.naturalWidth > 0;
  })()`);
  const zoomAt = {
    x: Math.round(zoomBox.x + zoomBox.w / 2),
    y: Math.round(zoomBox.y + zoomBox.h / 2),
  };
  const touch = (type, points) => send('Input.dispatchTouchEvent', { type, touchPoints: points });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  // Два пальца расходятся вдвое: 80 пикселей между ними → 160.
  await touch('touchStart', [
    { x: zoomAt.x - 40, y: zoomAt.y, id: 1 },
    { x: zoomAt.x + 40, y: zoomAt.y, id: 2 },
  ]);
  await touch('touchMove', [
    { x: zoomAt.x - 80, y: zoomAt.y, id: 1 },
    { x: zoomAt.x + 80, y: zoomAt.y, id: 2 },
  ]);
  await sleep(80);
  const pinch = await evaluate(`(() => {
    const img = document.querySelector('#shot-strip .shot-page img');
    return { transform: img.style.transform, label: document.getElementById('shot-label').textContent };
  })()`);
  await touch('touchEnd', []);
  await sleep(80);
  check('значок: щипок приближает снимок и пишет масштаб в подписи',
    /scale\(2\)$/.test(pinch.transform) && pinch.label === 'Экран 1 · 1200×800 · 200%', pinch);

  // Приближённый кадр палец двигает, а не листает.
  await touch('touchStart', [{ x: zoomAt.x, y: zoomAt.y, id: 3 }]);
  await touch('touchMove', [{ x: zoomAt.x + 60, y: zoomAt.y + 20, id: 3 }]);
  await sleep(80);
  const panned = await evaluate(`(() => ({
    transform: document.querySelector('#shot-strip .shot-page img').style.transform,
    label: document.getElementById('shot-label').textContent,
  }))()`);
  await touch('touchEnd', []);
  await send('Emulation.setTouchEmulationEnabled', { enabled: false });
  await sleep(80);
  check('значок: приближённый снимок двигается пальцем и не листает',
    /^translate\(60px, /.test(panned.transform) && panned.label === 'Экран 1 · 1200×800 · 200%', panned);

  // Соседний экран показывается целиком: приближение относится к прежнему кадру.
  const afterZoom = await evaluate(`(() => {
    document.getElementById('shot-next').click();
    return {
      label: document.getElementById('shot-label').textContent,
      transforms: [...document.querySelectorAll('#shot-strip .shot-page img')].map((i) => i.style.transform),
    };
  })()`);
  check('значок: после листания снимок снова целый',
    afterZoom.label === 'Экран 2 · 800×600' && afterZoom.transforms.every((t) => !t), afterZoom);

  // Ошибку агента видно в просмотре, Esc закрывает и очищает страницы.
  const shotError = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    window.__screenShots = [];
    window.__screensError = 'снимок экрана есть только на Windows';
    document.getElementById('shot-refresh').click();
    await wait(60);
    const hint = document.getElementById('shot-hint').textContent;
    const dialog = document.getElementById('shot');
    dialog.dispatchEvent(new Event('cancel', { cancelable: true }));
    return { hint, open: dialog.open, pages: document.querySelectorAll('#shot-strip .shot-page').length };
  })()`);
  check('значок: ошибка агента видна в просмотре', shotError.hint.includes('только на Windows'), shotError);
  check('значок: Esc закрывает и очищает просмотр', !shotError.open && shotError.pages === 0, shotError);

  // Без открытой сессии и при двух компьютерах панель спрашивает, чей экран.
  const shotPicker = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    closeView();
    window.__screensError = '';
    window.__screenShots = [{ label: 'Экран 1', width: 1920, height: 1080, data: '${shotPng}' }];
    window.__sock.emit({ t: 'agents', agents: [
      { name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [window.__session] },
      { name: 'laptop', online: true, os: 'test', version: 'test', sessions: [] },
    ]});
    window.__screens = [];
    document.getElementById('shots').click();
    await wait(20);
    const picker = document.getElementById('shot-agent');
    const names = [...picker.querySelectorAll('button.model')].map((b) => b.textContent);
    const open = picker.open;
    picker.querySelector('button.model').click();
    await wait(60);
    const asked = window.__screens.map((m) => m.agent).join();
    const shotOpen = document.getElementById('shot').open;
    document.getElementById('shot').dispatchEvent(new Event('cancel', { cancelable: true }));
    window.__sock.emit({ t: 'agents', agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: [window.__session] }] });
    return { open, names, asked, shotOpen };
  })()`);
  check('значок: без сессии и двух компьютерах — выбор компьютера',
    shotPicker.open && shotPicker.names.includes('ui-test') && shotPicker.names.includes('laptop'), shotPicker);
  check('значок: выбранный компьютер снимает экран', shotPicker.asked === 'ui-test' && shotPicker.shotOpen, shotPicker);

  // Сообщение в очереди: открываем сессию и показываем его, чтобы проверить
  // крестик снятия (до этого тест работал со списком сессий).
  await evaluate(`document.querySelector('#sessions .session').click()`);
  if (!(await waitFor(`document.querySelectorAll('#feed .item').length === ${items.length}`))) {
    throw new Error(`в ленте не ${items.length} элементов`);
  }
  await evaluate(`window.__sock.emit({ t: 'history', agent: 'ui-test', session: ${JSON.stringify(session.key)}, reset: true,
    items: [{ id: 'queue:1', ts: Date.now(), role: 'assistant', kind: 'queued', text: 'ждёт ответа' }] })`);
  if (await waitFor(`document.querySelectorAll('.queue-cancel').length === 1`)) {
    checkIcons(await iconOffsets(['.queue-cancel']));
  } else {
    check('очередь: сообщение в очереди показано', false);
  }
  // Push-уведомления: колокольчик в шапке списка подписывает это устройство.
  // Сервис-воркер и push-службу подменяем: настоящей подписке нужна FCM.
  const push = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const btn = document.getElementById('push');
    const hiddenWithoutKey = btn.hidden;
    const key = 'BOn9ac1a5Ffn8m7RmprRtBR1oT1tH4FKN2NXdvh8NjzfCyUFz8zPpTnS5QrmDtIk_x2vvqRoTn5pHTOsrMzyD0E';
    const calls = { subscribe: 0, unsubscribe: 0, options: null };
    const sub = {
      endpoint: 'https://fcm.googleapis.com/fcm/send/test-device',
      options: { applicationServerKey: null },
      toJSON() { return { endpoint: this.endpoint, keys: { p256dh: 'BPk', auth: 'AAA' } }; },
      unsubscribe() { calls.unsubscribe += 1; return Promise.resolve(true); },
    };
    const reg = {
      pushManager: {
        getSubscription: () => Promise.resolve(null),
        subscribe: (opts) => {
          calls.subscribe += 1;
          calls.options = opts;
          sub.options.applicationServerKey = opts.applicationServerKey.buffer;
          return Promise.resolve(sub);
        },
      },
    };
    Object.defineProperty(navigator, 'serviceWorker', {
      configurable: true,
      value: { register: () => Promise.resolve(reg), ready: Promise.resolve(reg), addEventListener() {} },
    });
    who = { user: 'stas', version: '11.0', push_key: key };
    await initPush();
    const shown = !btn.hidden && !btn.classList.contains('on');
    window.__push = [];
    btn.click();
    await wait(50);
    const subscribed = btn.classList.contains('on') && btn.getAttribute('aria-pressed') === 'true';
    const sent = window.__push[0] || {};
    // Пока устройство подписано, уведомления страницы не дублируют push.
    const before = window.__notices.length;
    showNotice({ name: 'ui-test' }, { key: 's', title: 'T' }, 'done');
    const pageNotice = window.__notices.length - before;
    btn.click();
    await wait(50);
    const off = !btn.classList.contains('on');
    const unsub = window.__push[1] || {};
    // Сервер отказал (нет агента в сети) — подписка браузера снимается.
    window.__pushError = 'нет агента в сети: подписку негде сохранить';
    btn.click();
    await wait(50);
    const refused = !btn.classList.contains('on') && calls.unsubscribe === 2;
    const refusedHint = document.getElementById('hint-list').textContent;
    window.__pushError = '';
    return {
      hiddenWithoutKey, shown, subscribed, pageNotice, off, refused, refusedHint,
      userVisibleOnly: calls.options && calls.options.userVisibleOnly,
      keyLen: calls.options && calls.options.applicationServerKey.length,
      sentType: sent.t, sentSub: sent.sub,
      unsubType: unsub.t, unsubEndpoint: unsub.endpoint,
    };
  })()`);
  check('push: без ключа сервера колокольчика нет', push.hiddenWithoutKey, push);
  check('push: колокольчик виден и выключен', push.shown, push);
  check(
    'push: подписка уходит на сервер с ключом VAPID',
    push.subscribed && push.userVisibleOnly === true && push.keyLen === 65 && push.sentType === 'push_subscribe'
      && push.sentSub.endpoint === 'https://fcm.googleapis.com/fcm/send/test-device'
      && push.sentSub.p256dh === 'BPk' && push.sentSub.auth === 'AAA',
    push,
  );
  check('push: уведомления страницы не дублируют push', push.pageNotice === 0, push);
  check('push: повторное нажатие отписывает устройство',
    push.off && push.unsubType === 'push_unsubscribe' && push.unsubEndpoint === 'https://fcm.googleapis.com/fcm/send/test-device', push);
  check('push: отказ сервера снимает подписку и объясняет причину',
    push.refused && push.refusedHint.includes('нет агента в сети'), push);

  // Нажатие на уведомление открывает сессию по ссылке `#agent=…&session=…`.
  const opened = await evaluate(`(async () => {
    const link = '#' + new URLSearchParams({ agent: 'ui-test', session: window.__session.key });
    closeView();
    history.replaceState(null, '', link);
    takeOpenFromHash();
    tryPendingOpen();
    await new Promise((r) => setTimeout(r, 20));
    return { selected: selected && selected.session, hash: location.hash, link };
  })()`);
  check('ссылка: сессия из уведомления открывается, адрес становится её ссылкой',
    opened.selected === session.key && opened.hash === opened.link, opened);

  // Подагенты: их вызовы в ленте не показываются, а список над счётчиком хода
  // открывает ленту подагента вкладкой в блоке ленты.
  const subs = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const session = window.__session.key;
    const msg = { agent: 'ui-test', session };
    const emit = (m) => window.__sock.emit(m);
    window.__subWatched = [];
    const mainBefore = document.querySelectorAll('#feed .item').length;
    emit({ t: 'subagents', ...msg, subagents: [
      { id: 'a1', title: 'Найти причину', agent: 'explore', running: true, started_at: 1 },
      { id: 'a2', title: 'Написать отчёт', agent: 'general-purpose', running: false, started_at: 2 },
    ] });
    await wait(20);
    const strip = document.getElementById('subagents');
    const chips = [...strip.querySelectorAll('.subagent')];
    const shown = !strip.hidden;
    const chipTitle = chips[0] ? chips[0].textContent : '';
    chips[0].click();
    await wait(20);
    const watch = window.__subWatched[0] || {};
    const tabsShown = !document.getElementById('feed-tabs').hidden;
    const tabCount = document.querySelectorAll('#feed-tabs .feed-tab').length;
    const mainGone = document.querySelectorAll('#feed .item').length === 0;
    emit({ t: 'sub_history', ...msg, sub: 'a1', reset: true, more: false, items: [
      { id: 's1', ts: 1, role: 'user', kind: 'text', text: 'Что нашёл?' },
      { id: 's2', ts: 2, role: 'assistant', kind: 'text', text: 'Причину нашёл в кэше.' },
    ] });
    await wait(20);
    const subItems = document.querySelectorAll('#feed .item').length;
    const subText = document.getElementById('feed').textContent;
    // Избранное относится к сессии: у подагента звёзд нет.
    const favs = document.querySelectorAll('#feed .fav').length;
    document.querySelectorAll('#feed-tabs .feed-tab-btn')[0].click();
    await wait(20);
    const backItems = document.querySelectorAll('#feed .item').length;
    const unwatched = window.__subWatched.some((m) => m.t === 'unwatch_sub' && m.sub === 'a1');
    // Закрываем вкладку подагента: открытых больше нет — полоса вкладок уходит.
    document.querySelector('#feed-tabs .feed-tab-close').click();
    await wait(20);
    return {
      shown, chips: chips.length, chipTitle, watch, tabsShown, tabCount, mainGone,
      mainBefore, subItems, subText, favs, backItems, unwatched,
      tabsAfter: document.querySelectorAll('#feed-tabs .feed-tab').length,
      tabsHidden: document.getElementById('feed-tabs').hidden,
      stripChips: document.querySelectorAll('#subagents .subagent').length,
    };
  })()`);
  check('подагенты: список виден, вызовы в ленте не нужны', subs.shown && subs.chips === 2 && subs.stripChips === 2, subs);
  check('подагенты: подпись и запуск ленты по нажатию',
    subs.chipTitle.includes('Найти причину') && subs.watch.t === 'watch_sub' && subs.watch.sub === 'a1', subs);
  check('подагенты: лента открывается вкладкой, основная уступает место',
    subs.tabsShown && subs.tabCount === 2 && subs.mainGone, subs);
  check('подагенты: пришедшие элементы видны, звёзд избранного нет',
    subs.subItems === 2 && subs.subText.includes('Причину нашёл') && subs.favs === 0, subs);
  check('подагенты: возврат в основную ленту восстанавливает её целиком',
    subs.backItems === subs.mainBefore && subs.mainBefore > 0 && subs.unwatched, subs);
  check('подагенты: закрытая вкладка убирает полосу вкладок',
    subs.tabsAfter === 0 && subs.tabsHidden, subs);

  // Ссылка на чат: открытая сессия и лента подагента видны в адресе, ссылка
  // открывается заново, а возврат к списку убирает её.
  const links = await evaluate(`(async () => {
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const base = '#' + new URLSearchParams({ agent: 'ui-test', session: window.__session.key });
    closeView();
    select('ui-test', window.__session.key);
    await wait(20);
    const opened = location.hash;
    window.__sock.emit({ t: 'subagents', agent: 'ui-test', session: window.__session.key, subagents: [
      { id: 'a1', title: 'Найти причину', agent: 'explore', running: false, started_at: 1 },
      { id: 'a2', title: 'Написать отчёт', agent: 'general-purpose', running: false, started_at: 2 },
    ] });
    await wait(20);
    // В списке подагентов второй — a2: у его ленты должен быть свой адрес.
    document.querySelectorAll('#subagents .subagent')[1].click();
    await wait(20);
    const subHash = location.hash;
    document.querySelector('#feed-tabs .feed-tab-btn').click();
    await wait(20);
    const mainHash = location.hash;
    // Так адрес приходит при открытии ссылки: без события hashchange.
    history.replaceState(history.state, '', base + '&sub=a2');
    syncFromHash();
    await wait(20);
    const linked = { subTab, hash: location.hash };
    closeView();
    const closed = location.hash;
    return { base, opened, subHash, mainHash, linked, closed };
  })()`);
  check('ссылка: открытая сессия записана в адрес',
    links.opened === links.base && links.mainHash === links.base, links);
  check('ссылка: у ленты подагента свой адрес',
    links.subHash === links.base + '&sub=a2', links);
  check('ссылка: адрес с подагентом открывает его ленту',
    links.linked.subTab === 'a2' && links.linked.hash === links.base + '&sub=a2', links);
  check('ссылка: возврат к списку убирает адрес сессии', links.closed === '', links);

  // Вывод инструментов приходит из терминала с управляющими последовательностями
  // ANSI: в ленте они не нужны, а у прогресс-строк остаётся итоговое состояние.
  const cleaned = await evaluate(`(() => {
    applyHistory({ items: [{ id: 'ansi1', ts: Date.now(), role: 'tool', kind: 'tool_result', tool: 'bash',
      text: 'k1\\u001b[31;1m: ошибка\\u001b[0m\\nитог 1%\\rитог 100%' }] });
    const node = document.querySelector('.item[data-id="ansi1"]');
    const raw = node ? node.textContent : '';
    const body = node ? (node.querySelector('.body') || {}).textContent || '' : '';
    const summary = node ? (node.querySelector('summary') || {}).textContent || '' : '';
    return { raw, body, summary, hasEsc: raw.includes('\\u001b') || raw.includes('[31;1m') };
  })()`);
  check('лента: управляющие последовательности не показываются',
    !cleaned.hasEsc && cleaned.body.includes('итог 100%') && !cleaned.body.includes('итог 1%') && cleaned.summary.includes('ошибка'),
    cleaned);

  check('ошибок на странице нет', pageErrors.length === 0, pageErrors);
} catch (error) {
  failures.push(String(error));
  console.error('Ошибка теста:', error);
  if (pageErrors.length) console.error('Ошибки страницы:', pageErrors);
  try {
    const shot = await send('Page.captureScreenshot', { format: 'png' });
    const file = path.join(os.tmpdir(), 'aipanel-ui-failure.png');
    fs.writeFileSync(file, Buffer.from(shot.data, 'base64'));
    console.error('Снимок страницы:', file);
  } catch {}
} finally {
  try {
    await send('Browser.close');
  } catch {}
  // На Windows процесс не всегда успевает выйти до удаления профиля.
  await new Promise((resolve) => {
    if (chrome.exitCode !== null) return resolve();
    const timer = setTimeout(resolve, 3000);
    chrome.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
  });
  try {
    chrome.kill();
  } catch {}
  server.close();
  try {
    fs.rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  } catch {}
}

if (failures.length) {
  console.error(`\nНе прошло проверок: ${failures.length}`);
  process.exit(1);
}
console.log('\nВсе проверки прошли.');
process.exit(0);
