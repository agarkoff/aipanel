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
};

// Команды, которые «пришлёт агент» в ответ на запрос списка по «/».
const commands = [
  { name: 'init', description: 'guided AGENTS.md setup' },
  { name: 'review', description: 'review changes' },
  { name: 'clear', description: 'start a new session' },
];

// ---------------------------------------------------------------- заглушка

const stub = `
(() => {
  const session = ${JSON.stringify(session)};
  const items = ${JSON.stringify(items)};
  const commands = ${JSON.stringify(commands)};
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
      if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items }), 0);
      } else if (msg.t === 'commands') {
        setTimeout(() => this.emit({ t: 'commands', agent: msg.agent, session: msg.session, commands }), 0);
      } else if (msg.t === 'send') {
        window.__sent.push(msg);
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
  ['/suggest.js', 'text/javascript; charset=utf-8'],
  ['/style.css', 'text/css; charset=utf-8'],
  ['/icon.svg', 'image/svg+xml'],
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
      return list && !list.hidden && list.querySelectorAll('.sg-item').length === ${commands.length};
    })()`,
  );
  const all = await slashState();
  check('«/» открывает список команд', listed && all.open, all);
  check('в списке команды с описаниями', all.items.join(',') === 'init,review,clear', all);

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
    mobilePick.text === '/init ' && !mobilePick.open && mobileArgs.text === '/init 24' && !mobileArgs.open,
    { mobilePick, mobileArgs },
  );

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
