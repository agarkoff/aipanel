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
      } else if (msg.t === 'move') {
        // Переезд сессии в другой каталог: у настоящего opencode меняется её
        // location.directory, и следующий список приходит уже с новым местом.
        const moved = sessions.find((s) => s.key === msg.session);
        if (moved) moved.cwd = msg.cwd;
        window.__moves.push({ session: msg.session, cwd: msg.cwd });
        this.emit({ t: 'result', id: msg.id, ok: true });
        save();
        this.emitAgents();
        return;
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
          { name: 'pc', online: true, os: 'win', version: window.__agentVersion || '17.4.0', sessions, pins: state.pins, closed: state.closed },
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
  window.__moves = [];
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

  // Закрепление: строка уходит из проекта в блок «Закреплённые» наверху, чтобы
  // её было видно всегда, в том числе когда проект свёрнут.
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Спит');
    row.querySelector('.session-pin').click();
  })()`);
  await sleep(80);
  const pinnedState = await evaluate(view);
  const docsPinned = pinnedState.projects.find((p) => p.name === 'docs');
  check(
    'закреплённая сессия уходит наверх и не повторяется в проекте',
    JSON.stringify(pinnedState.pinned) === JSON.stringify(['Спит']) && !docsPinned.rows.includes('Спит'),
    pinnedState,
  );
  check(
    'счётчик проекта считает и закреплённую сессию',
    docsPinned.count === '3',
    docsPinned,
  );
  check(
    'кнопка закрепа помечает сессию',
    await evaluate(`!!document.querySelector('#sessions .session-pin.on[aria-pressed="true"]')`),
  );
  check('закреп сохранён у агента', await evaluate(`window.__state.pins.includes('claude:i')`));

  // Сворачиваем проект с закреплённой: наверху она остаётся, строки проекта прячутся.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(80);
  const collapsedDocs = await evaluate(view);
  check(
    'в свёрнутом проекте закреплённая тоже видна',
    collapsedDocs.projects.find((p) => p.name === 'docs').expanded === 'false' &&
      JSON.stringify(collapsedDocs.pinned) === JSON.stringify(['Спит']),
    collapsedDocs,
  );

  // Раскрываем обратно: закреплённая остаётся наверху, в проекте её нет.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(80);
  const expandedDocs = await evaluate(view);
  check(
    'после раскрытия проекта закреплённая остаётся наверху',
    JSON.stringify(expandedDocs.pinned) === JSON.stringify(['Спит']) &&
      !expandedDocs.projects.find((p) => p.name === 'docs').rows.includes('Спит'),
    expandedDocs,
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
  // Перед ней сворачиваем проект, в котором уже есть ждущая сессия: при загрузке
  // он не должен раскрыться сам.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(120);
  await send('Page.navigate', { url: pageUrl });
  await waitFor(`!!window.__socket && !!document.getElementById('sessions')`, 8000);
  await sendList(sessions());
  await waitFor(`document.querySelectorAll('#sessions .project').length === 2`, 8000);
  const reloaded = await evaluate(view);
  check(
    'закреп и свёрнутый проект приходят после перезагрузки',
    JSON.stringify(reloaded.pinned) === JSON.stringify(['Спит']) &&
      reloaded.projects.find((p) => p.name === 'api').expanded === 'false',
    reloaded,
  );
  check(
    'уже ждущая сессия не раскрывает свёрнутый проект при загрузке',
    reloaded.projects.find((p) => p.name === 'docs').expanded === 'false',
    reloaded.projects,
  );

  // Раскрываем проект, чтобы после открепления строка вернулась на видное место.
  await evaluate(`(() => {
    [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs').click();
  })()`);
  await sleep(120);

  // Открепление: строка возвращается в свой проект.
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Спит');
    row.querySelector('.session-pin').click();
  })()`);
  await sleep(80);
  const unpinned = await evaluate(view);
  check(
    'после открепления строка вернулась в проект',
    unpinned.pinned.length === 0 &&
      unpinned.projects.find((p) => p.name === 'docs').rows.includes('Спит'),
    unpinned,
  );
  check(
    'закреп убран из хранилища агента',
    await evaluate(`window.__state.pins.length === 0`),
  );

  // Перенос строки в другой проект: у opencode сессия меняет каталог, а с ним и
  // проект в списке. На компьютере перенос начинается движением мыши.
  await evaluate(`sessionStorage.removeItem('aipanel-test-list')`);
  await send('Page.navigate', { url: pageUrl });
  await waitFor(`!!window.__socket && !!document.getElementById('sessions')`, 8000);
  await sendList(sessions());
  await waitFor(`document.querySelectorAll('#sessions .project').length === 2`, 8000);

  const dragBox = await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Бегунок');
    const project = [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs');
    const from = row.getBoundingClientRect();
    const to = project.getBoundingClientRect();
    return {
      x1: Math.round(from.left + from.width / 2),
      y1: Math.round(from.top + from.height / 2),
      x2: Math.round(to.left + to.width / 2),
      y2: Math.round(to.top + to.height / 2),
    };
  })()`);
  const mouse = (type, x, y, buttons) =>
    send('Input.dispatchMouseEvent', { type, x, y, button: 'left', buttons, clickCount: 1 });
  await mouse('mousePressed', dragBox.x1, dragBox.y1, 0);
  await mouse('mouseMoved', dragBox.x1 + 24, dragBox.y1 + 8, 1);
  await mouse('mouseMoved', dragBox.x2, dragBox.y2, 1);
  await sleep(80);
  const carrying = await evaluate(`({
    ghost: !!document.querySelector('.drag-ghost'),
    target: !!document.querySelector('#sessions .project.drop-target'),
  })`);
  check('при переносе видна копия строки и подсвечен проект', carrying.ghost && carrying.target, carrying);
  await mouse('mouseReleased', dragBox.x2, dragBox.y2, 0);
  await sleep(250);
  const moves = await evaluate(`window.__moves`);
  check(
    'строка, брошенная на проект, переезжает в его каталог',
    moves.length === 1 && moves[0].session === 'opencode:a' && moves[0].cwd === DOCS,
    moves,
  );
  const movedView = await evaluate(view);
  const docsAfter = movedView.projects.find((p) => p.name === 'docs');
  check(
    'переехавшая сессия видна в новом проекте',
    !!docsAfter && docsAfter.rows.includes('Бегунок'),
    movedView.projects,
  );

  // Строка другой сессии целью не считается: перенос — это смена каталога, а не
  // передача задачи соседней сессии. Бросок на неё ничего не делает.
  const rowBox = await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Спросит');
    const target = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Второй');
    const from = row.getBoundingClientRect();
    const to = target.getBoundingClientRect();
    return {
      x1: Math.round(from.left + from.width / 2),
      y1: Math.round(from.top + from.height / 2),
      x2: Math.round(to.left + to.width / 2),
      y2: Math.round(to.top + to.height / 2),
    };
  })()`);
  await mouse('mousePressed', rowBox.x1, rowBox.y1, 0);
  await mouse('mouseMoved', rowBox.x1 + 24, rowBox.y1 + 8, 1);
  await mouse('mouseMoved', rowBox.x2, rowBox.y2, 1);
  await sleep(80);
  const rowHover = await evaluate(`!!document.querySelector('#sessions .session-row.drop-target')`);
  await mouse('mouseReleased', rowBox.x2, rowBox.y2, 0);
  await sleep(200);
  check(
    'строка другой сессии не цель переноса',
    !rowHover && (await evaluate(`window.__moves.length`)) === 1,
    { rowHover, moves: await evaluate(`window.__moves`) },
  );

  // Claude Code так не умеет: его переписка привязана к каталогу проекта. Панель
  // говорит об этом сразу, а не после ответа агента.
  const claudeBox = await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Спит');
    const project = [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'api');
    const from = row.getBoundingClientRect();
    const to = project.getBoundingClientRect();
    return {
      x1: Math.round(from.left + from.width / 2),
      y1: Math.round(from.top + from.height / 2),
      x2: Math.round(to.left + to.width / 2),
      y2: Math.round(to.top + to.height / 2),
    };
  })()`);
  await mouse('mousePressed', claudeBox.x1, claudeBox.y1, 0);
  await mouse('mouseMoved', claudeBox.x1 + 24, claudeBox.y1 + 8, 1);
  await mouse('mouseMoved', claudeBox.x2, claudeBox.y2, 1);
  await sleep(80);
  const refused = await evaluate(`({
    ghost: !!document.querySelector('.drag-ghost'),
    hint: document.getElementById('hint-list').textContent,
  })`);
  await mouse('mouseReleased', claudeBox.x2, claudeBox.y2, 0);
  await sleep(120);
  check(
    'сессию Claude Code перенести нельзя — сказано почему',
    !refused.ghost && refused.hint.includes('opencode'),
    refused,
  );
  check('для Claude Code команда агенту не уходит', (await evaluate(`window.__moves.length`)) === 1);

  // Агент старой версии не знает команду `move` и не ответил бы на неё: такой
  // перенос панель не предлагает, а просит обновить агент.
  await evaluate(`window.__agentVersion = '17.3.0'`);
  await sendList(sessions());
  await waitFor(`document.querySelectorAll('#sessions .project').length === 2`);
  const oldBox = await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Бегунок');
    const project = [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs');
    const from = row.getBoundingClientRect();
    const to = project.getBoundingClientRect();
    return {
      x1: Math.round(from.left + from.width / 2),
      y1: Math.round(from.top + from.height / 2),
      x2: Math.round(to.left + to.width / 2),
      y2: Math.round(to.top + to.height / 2),
    };
  })()`);
  await mouse('mousePressed', oldBox.x1, oldBox.y1, 0);
  await mouse('mouseMoved', oldBox.x1 + 24, oldBox.y1 + 8, 1);
  await mouse('mouseMoved', oldBox.x2, oldBox.y2, 1);
  await sleep(80);
  const oldRefused = await evaluate(`({
    ghost: !!document.querySelector('.drag-ghost'),
    hint: document.getElementById('hint-list').textContent,
  })`);
  await mouse('mouseReleased', oldBox.x2, oldBox.y2, 0);
  check(
    'старый агент не обещает перенос — просит обновиться',
    !oldRefused.ghost && oldRefused.hint.includes('обновите агент'),
    oldRefused,
  );
  await evaluate(`window.__agentVersion = '17.4.0'`);
  await sendList(sessions());
  await sleep(80);

  // На телефоне перенос начинается долгим нажатием: обычное движение пальцем
  // по-прежнему прокручивает список, а горизонтальный жест удаляет строку.
  const touchBox = await evaluate(`(() => {
    const row = [...document.querySelectorAll('#sessions .session-row')]
      .find((r) => r.querySelector('.name').textContent === 'Второй');
    const project = [...document.querySelectorAll('#sessions .project')]
      .find((p) => p.querySelector('.project-name').textContent === 'docs');
    const from = row.getBoundingClientRect();
    const to = project.getBoundingClientRect();
    return {
      x1: Math.round(from.left + from.width / 2),
      y1: Math.round(from.top + from.height / 2),
      x2: Math.round(to.left + to.width / 2),
      y2: Math.round(to.top + to.height / 2),
    };
  })()`);
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 1 });
  const touchAt = (type, x, y) =>
    send('Input.dispatchTouchEvent', { type, touchPoints: type === 'touchEnd' ? [] : [{ x, y }] });
  await touchAt('touchStart', touchBox.x1, touchBox.y1);
  await sleep(600);
  const holding = await evaluate(`!!document.querySelector('.drag-ghost')`);
  await touchAt('touchMove', touchBox.x2, touchBox.y2);
  await sleep(60);
  await touchAt('touchEnd', touchBox.x2, touchBox.y2);
  await sleep(250);
  const touchMoves = await evaluate(`window.__moves`);
  check('долгое нажатие пальцем тоже переносит сессию', holding && touchMoves.length === 2, {
    holding,
    moves: touchMoves,
  });

  // Телефон: зона нажатия закрепа должна быть под палец, а не 24 точки.
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 800, deviceScaleFactor: 1, mobile: true });
  await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 1 });
  await sleep(250);
  const touch = await evaluate(`(() => {
    const row = document.querySelector('#sessions .session-row');
    const pin = row.querySelector('.session-pin').getBoundingClientRect();
    const when = row.querySelector('.when').getBoundingClientRect();
    return {
      width: Math.round(pin.width),
      height: Math.round(pin.height),
      pinLeft: Math.round(pin.left),
      whenRight: Math.round(when.right),
      viewport: window.innerWidth,
    };
  })()`);
  check(
    'на телефоне в кнопку закрепа легко попасть',
    touch.width >= 40 && touch.height >= 40 && touch.pinLeft >= touch.whenRight,
    touch,
  );
  await send('Emulation.setTouchEmulationEnabled', { enabled: false, maxTouchPoints: 1 });
  await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });

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
