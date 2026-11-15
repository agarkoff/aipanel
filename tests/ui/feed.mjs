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

// ---------------------------------------------------------------- заглушка

const stub = `
(() => {
  const session = ${JSON.stringify(session)};
  const items = ${JSON.stringify(items)};
  const commands = ${JSON.stringify(commands)};
  const models = ${JSON.stringify(models)};
  const claudeModels = ${JSON.stringify(claudeModels)};
  window.__sent = [];
  window.__setModels = [];
  window.__starts = [];
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
      if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items }), 0);
      } else if (msg.t === 'commands') {
        setTimeout(() => this.emit({ t: 'commands', agent: msg.agent, session: msg.session, commands }), 0);
      } else if (msg.t === 'models') {
        setTimeout(() => this.emit({ t: 'models', agent: msg.agent, session: msg.session, models }), 0);
      } else if (msg.t === 'tool_models') {
        const list = msg.tool === 'claude' ? claudeModels : models;
        setTimeout(() => this.emit({ t: 'tool_models', id: msg.id, models: list }), 0);
      } else if (msg.t === 'start') {
        window.__starts.push(msg);
        setTimeout(() => this.emit({ t: 'result', id: msg.id, ok: false, error: 'тест' }), 0);
      } else if (msg.t === 'send') {
        window.__sent.push(msg);
      } else if (msg.t === 'set_model') {
        window.__setModels.push(msg);
        setTimeout(() => this.emit({ t: 'result', id: msg.id, ok: true }), 0);
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

  // Регрессия: общее CSS-правило `.user` (имя в шапке) сжимало свои сообщения
  // в ленте в ноль — они оставались в DOM, но были не видны.
  const mine = await evaluate(`(() => {
    const users = [...document.querySelectorAll('#feed .item.user')];
    const heights = users.map((n) => Math.round(n.getBoundingClientRect().height));
    return { count: users.length, heights, min: heights.length ? Math.min(...heights) : 0 };
  })()`);
  check('свои сообщения видны в ленте', mine.count === 3 && mine.min > 0, mine);

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
  await typeSlash('/clear');
  await sleep(100);
  await press('Enter');
  await sleep(150);
  const sentText = await evaluate(`window.__sent.map((m) => m.text)`);
  check('Enter без выбора отправляет набранное', sentText.join('|') === '/clear', sentText);

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

  await evaluate(`document.getElementById('new-session').click()`);
  await waitFor(`document.getElementById('launch').open`);
  const launchInit = await evaluate(`({
    tool: document.getElementById('launch-tool').value,
    cwd: document.getElementById('launch-cwd').value,
    model: document.getElementById('launch-model').value,
  })`);
  check('окно запуска открылось с инструментом сессии', launchInit.tool === 'opencode', launchInit);
  check('модель по умолчанию пустая', launchInit.model === '', launchInit);

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

  check('ошибок на странице нет', pageErrors.length === 0, pageErrors);
} catch (error) {
  failures.push(String(error));
  console.error('Ошибка теста:', error);
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
