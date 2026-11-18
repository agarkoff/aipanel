// Проверка списка сессий в настоящем браузере: index.html, style.css и части
// панели берутся из crates/server/static, а сервер и агент заменены заглушкой
// (WebSocket и /api/me подменяются до загрузки панели). Тест смотрит на порядок
// строк: сессии одного каталога собраны в проекты и не меняются местами от
// нового времени активности, закреплённые стоят выше проектов, а обновление
// списка переиспользует узлы и не выбивает поле правки имени.
//
// Запуск: node tests/ui/sessions.mjs
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
  console.log('Chrome не найден: тест списка сессий пропущен (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- данные списка

const now = Date.now();
const HOUR = 3600e3;

// Порядок как у агента: по последней активности, самая свежая сверху.
// Две сессии живут в одном каталоге — это один проект.
const WORK = 'C:\\work\\api';
const DOCS = 'C:\\work\\docs';
const sessions = () => [
  { key: 'opencode:a', tool: 'opencode', title: 'Бегунок', cwd: WORK, status: 'busy', live: true, updated_at: now, delivery: 'api' },
  { key: 'opencode:b', tool: 'opencode', title: 'Второй', cwd: WORK, status: 'busy', live: true, updated_at: now - 1000, delivery: 'api' },
  { key: 'opencode:w', tool: 'opencode', title: 'Спросит', cwd: DOCS, status: 'waiting', live: true, updated_at: now - 2000, delivery: 'api' },
  { key: 'claude:i', tool: 'claude', title: 'Спит', cwd: DOCS, status: 'idle', live: true, updated_at: now - 3 * HOUR, delivery: 'console' },
  { key: 'claude:s', tool: 'claude', title: 'Старая', cwd: DOCS, status: 'stopped', live: false, updated_at: now - 4 * HOUR, delivery: 'none' },
];

// ---------------------------------------------------------------- заглушка

const stub = `
(() => {
  // Заглушка агента хранит состояние списка (закреплённые и свёрнутые проекты)
  // за панель: в жизни это файл list.json на машине агента, здесь sessionStorage,
  // чтобы состояние переживало перезагрузку страницы.
  const state = JSON.parse(sessionStorage.getItem('aipanel-test-list') || '{"pins":[],"closed":[]}');
  const save = () => sessionStorage.setItem('aipanel-test-list', JSON.stringify(state));
  let sessions = [];
  class FakeSocket {
    static OPEN = 1;
    constructor() {
      this.readyState = 0;
      window.__socket = this;
      setTimeout(() => {
        this.readyState = 1;
        if (this.onopen) this.onopen();
      }, 0);
    }
    send(text) {
      const msg = JSON.parse(text);
      if (msg.t === 'pin_session') {
        state.pins = msg.on
          ? [msg.session, ...state.pins.filter((k) => k !== msg.session)]
          : state.pins.filter((k) => k !== msg.session);
      } else if (msg.t === 'collapse_project') {
        state.closed = msg.on
          ? [...state.closed.filter((k) => k !== msg.project), msg.project]
          : state.closed.filter((k) => k !== msg.project);
      } else {
        return;
      }
      save();
      this.emitAgents();
    }
    close() { this.readyState = 3; }
    emit(msg) { if (this.onmessage) this.onmessage({ data: JSON.stringify(msg) }); }
    emitAgents() {
      this.emit({
        t: 'agents',
        agents: [
          { name: 'pc', online: true, os: 'win', version: 'test', sessions, pins: state.pins, closed: state.closed },
        ],
      });
    }
  }
  window.WebSocket = FakeSocket;
  // Список агента целиком: тест подменяет его между проверками.
  window.__send = (list) => {
    sessions = list;
    window.__socket.emitAgents();
  };
  window.__state = state;
  const realFetch = window.fetch.bind(window);
  window.fetch = (url, opts) =>
    String(url) === '/api/me'
      ? Promise.resolve(new Response(JSON.stringify({ user: 'owner', version: 'test' }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }))
      : realFetch(url, opts);
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

// ---------------------------------------------------------------- проверки

const failures = [];
function check(name, ok, details) {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${ok || details === undefined ? '' : ` — ${JSON.stringify(details)}`}`);
  if (!ok) failures.push(name);
}

// Список так, как его видно: закреплённые строки, проекты и строки внутри них.
const view = `(() => {
  const pinned = [];
  const projects = [];
  let current = null;
  for (const node of document.getElementById('sessions').children) {
    if (node.classList.contains('list-group')) {
      current = null;
      continue;
    }
    if (node.classList.contains('project')) {
      current = {
        name: node.querySelector('.project-name').textContent,
        expanded: node.getAttribute('aria-expanded'),
        count: node.querySelector('.project-count').textContent,
        rows: [],
      };
      projects.push(current);
      continue;
    }
    if (node.classList.contains('session-row')) {
      const name = node.querySelector('.name').textContent;
      if (current) current.rows.push(name);
      else pinned.push(name);
    }
  }
  return {
    pinned,
    projects,
    marked: [...document.querySelectorAll('#sessions .session-row')].filter((r) => r.dataset.mark === 'keep').length,
  };
})()`;

const sendList = (list) => evaluate(`window.__send(${JSON.stringify(list)})`);

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
  const booted = await waitFor(`!!window.__socket && !!document.getElementById('sessions')`);
  check('панель загрузилась', booted);
  await sendList(sessions());
  const ready = await waitFor(`document.querySelectorAll('#sessions .session-row').length === 5`);
  check('список сессий отрисован', ready);

  const first = await evaluate(view);
  check(
    'каталоги стали проектами: сначала ждущий ответа',
    first.projects.map((p) => p.name).join(',') === 'docs,api',
    first.projects,
  );
  check(
    'внутри проекта ждущая сверху, затем по активности',
    JSON.stringify(first.projects[0].rows) === JSON.stringify(['Спросит', 'Спит', 'Старая']) &&
      JSON.stringify(first.projects[1].rows) === JSON.stringify(['Бегунок', 'Второй']),
    first.projects,
  );
  check(
    'проекты раскрыты и показывают число сессий',
    first.projects.every((p) => p.expanded === 'true') &&
      first.projects.map((p) => p.count).join(',') === '3,2',
    first.projects,
  );
  check(
    'значок раскрытия нарисован, а не набран текстом',
    await evaluate(
      `[...document.querySelectorAll('#sessions .project-toggle')].every((t) => t.querySelectorAll('svg path').length === 2)`,
    ),
  );

  // Кнопки закрепа, закрытия и удаления нарисованы поверх строки: текст не
  // должен под них заходить.
  const layout = await evaluate(`(() => {
    const row = document.querySelector('#sessions .session-row');
    const when = row.querySelector('.when').getBoundingClientRect();
    const pin = row.querySelector('.session-pin').getBoundingClientRect();
    return { whenRight: Math.round(when.right), pinLeft: Math.round(pin.left) };
  })()`);
  check('время сессии не заходит под кнопку закрепа', layout.whenRight <= layout.pinLeft, layout);

  // Агент присылает новый порядок: у «Второго» последняя активность стала свежее.
  // Места внутри проекта уже выданы, поэтому строки не переставляются.
  await evaluate(`document.querySelectorAll('#sessions .session-row')[1].dataset.mark = 'keep'`);
  const churn = sessions().map((s) => ({ ...s }));
  churn[0].updated_at = now - 5000;
  churn[1].updated_at = now + 5000;
  await sendList(churn);
  await sleep(120);
  const second = await evaluate(view);
  check(
    'работающие сессии не меняются местами от нового времени активности',
    JSON.stringify(second.projects) === JSON.stringify(first.projects),
    second.projects,
  );
  check('строка переиспользована, а не создана заново', second.marked === 1, second);

  // Название открытой сессии правится на месте: обновление списка не должно
  // выбить поле из строки, иначе набираемое имя пропадёт.
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Бегунок');
    row.querySelector('.session').click();
  })()`);
  await sleep(150);
  await evaluate(`[...document.querySelectorAll('#sessions .name')]
    .find((n) => n.textContent === 'Бегунок').click()`);
  await sleep(100);
  const editing = await evaluate(`!!document.querySelector('#sessions .rename-list')`);
  await sendList(churn);
  await sleep(120);
  const kept = await evaluate(`!!document.querySelector('#sessions .rename-list')`);
  check('обновление списка не выбивает поле правки имени', editing && kept, { editing, kept });
  await evaluate(`document.querySelector('#sessions .rename-list')
    .dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  await sleep(120);

  // Сворачивание: плюсик на заголовке проекта прячет его строки и запоминается.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'api').click();
  })()`);
  await sleep(80);
  const collapsed = await evaluate(view);
  check(
    'свёрнутый проект прячет свои строки',
    collapsed.projects[1].expanded === 'false' && collapsed.projects[1].rows.length === 0,
    collapsed.projects,
  );
  check(
    'свёрнутость сохранена у агента',
    await evaluate(`window.__state.closed.length === 1`),
  );

  // Обновление списка от агента не раскрывает свёрнутый проект обратно.
  await sendList(churn);
  await sleep(120);
  const stillCollapsed = await evaluate(view);
  check('свёрнутый проект остаётся свёрнутым', stillCollapsed.projects[1].expanded === 'false', stillCollapsed.projects);

  // Закрепление в раскрытом проекте: строка остаётся на своём месте и только
  // помечается кнопкой — наверху её нет, иначе она была бы в списке дважды.
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Спит');
    row.querySelector('.session-pin').click();
  })()`);
  await sleep(80);
  const pinnedInPlace = await evaluate(view);
  const docsProject = pinnedInPlace.projects.find((p) => p.name === 'docs');
  check(
    'закреплённая сессия остаётся в своём проекте',
    pinnedInPlace.pinned.length === 0 && docsProject.rows.includes('Спит') && docsProject.count === '3',
    pinnedInPlace,
  );
  check(
    'кнопка закрепа помечает сессию',
    await evaluate(`!!document.querySelector('#sessions .session-pin.on[aria-pressed="true"]')`),
  );
  check('закреп сохранён у агента', await evaluate(`window.__state.pins.includes('claude:i')`));

  // Сворачиваем проект с закреплённой: теперь она поднимается наверх, чтобы
  // не пропасть за плюсиком.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(80);
  const lifted = await evaluate(view);
  check(
    'в свёрнутом проекте закреплённая уходит наверх',
    lifted.projects.every((p) => p.rows.length === 0) &&
      JSON.stringify(lifted.pinned) === JSON.stringify(['Спит']),
    lifted,
  );

  // Раскрываем обратно: строка возвращается в проект, наверху её больше нет.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(80);
  const back = await evaluate(view);
  check(
    'после раскрытия проекта строка возвращается на место',
    back.pinned.length === 0 && back.projects.find((p) => p.name === 'docs').rows.includes('Спит'),
    back,
  );

  // Сессия начинает ждать ответа — её свёрнутый проект раскрывается сам.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(80);
  const ask = churn.map((s) => ({ ...s }));
  ask[4].status = 'waiting';
  await sendList(ask);
  await sleep(120);
  const revealed = await evaluate(view);
  const docs = revealed.projects.find((p) => p.name === 'docs');
  check('проект раскрывается, когда в нём появился вопрос', docs.expanded === 'true', revealed.projects);

  // Свежая загрузка без адреса открытой сессии: в адресе (`#agent=…`) она бы
  // снова открылась и раскрыла свой проект — это не про состояние списка.
  await send('Page.navigate', { url: pageUrl });
  await waitFor(`!!window.__socket && !!document.getElementById('sessions')`, 8000);
  await sendList(sessions());
  await waitFor(`document.querySelectorAll('#sessions .project').length === 2`, 8000);
  const reloaded = await evaluate(view);
  check(
    'закреп и свёрнутый проект приходят после перезагрузки',
    reloaded.pinned.length === 0 &&
      reloaded.projects.find((p) => p.name === 'docs').rows.includes('Спит') &&
      reloaded.projects.find((p) => p.name === 'api').expanded === 'false',
    reloaded,
  );

  // Открепление: строка пропадает и из проекта, и сверху.
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Спит');
    row.querySelector('.session-pin').click();
  })()`);
  await sleep(80);
  const unpinned = await evaluate(view);
  check('после открепления строка больше не поднимается наверх', unpinned.pinned.length === 0, unpinned);
  check(
    'закреп убран из хранилища агента',
    await evaluate(`window.__state.pins.length === 0`),
  );

  check('ошибок на странице нет', pageErrors.length === 0, pageErrors);
} catch (error) {
  failures.push(String(error));
  console.error('Ошибка теста:', error);
  try {
    const shot = await send('Page.captureScreenshot', { format: 'png' });
    const file = path.join(os.tmpdir(), 'aipanel-ui-sessions-failure.png');
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
