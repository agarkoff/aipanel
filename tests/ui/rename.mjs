// Проверка переименования сессии в настоящем браузере: index.html, style.css и
// app.js берутся из crates/server/static, а сервер и агент заменены заглушкой
// (WebSocket и /api/me подменяются до загрузки app.js). Тест проверяет, что
// нажатие на имя открытой сессии в списке и на заголовок в шапке открывает
// поле, Enter и уход из поля отправляют команду агенту, Esc отменяет, а имя
// неактивной строки по-прежнему открывает сессию. На узком экране — что поле
// не выходит за границы и не приближает страницу при фокусе.
//
// Запуск: node tests/ui/rename.mjs
// Нужен установленный Chrome (или Edge); путь можно задать в CHROME_PATH.
// Без браузера тест пропускается с кодом 0.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
// Каталог статики можно подменить: так тест проверяет и отдельную копию файлов.
const STATIC = process.env.AIPANEL_STATIC || path.join(ROOT, 'crates/server/static');

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
  console.log('Chrome не найден: тест переименования пропущен (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- данные сессий

const now = Date.now();
const session = (key, title, cwd) => ({
  key,
  tool: 'opencode',
  title,
  cwd,
  status: 'idle',
  live: true,
  updated_at: now,
  delivery: 'api',
  model: 'opencode-go/deepseek-v4.1-flash',
});

const sessions = [
  session('opencode:a', 'Старое имя', 'C:\\one'),
  session('opencode:b', 'Вторая сессия', 'C:\\two'),
];

// ---------------------------------------------------------------- заглушка

const stub = `
(() => {
  window.__sessions = ${JSON.stringify(sessions)};
  window.__renames = [];
  window.__renameFail = false;
  window.__sock = null;
  class FakeSocket {
    static OPEN = 1;
    constructor() {
      window.__sock = this;
      this.readyState = 0;
      setTimeout(() => {
        this.readyState = 1;
        if (this.onopen) this.onopen();
        this.agents();
      }, 0);
    }
    agents() {
      this.emit({
        t: 'agents',
        agents: [{ name: 'ui-test', online: true, os: 'test', version: 'test', sessions: window.__sessions }],
      });
    }
    send(text) {
      const msg = JSON.parse(text);
      if (msg.t === 'watch') {
        setTimeout(() => this.emit({ t: 'history', agent: msg.agent, session: msg.session, reset: true, items: [], more: false }), 0);
      } else if (msg.t === 'rename') {
        window.__renames.push(msg);
        const fail = window.__renameFail;
        const target = window.__sessions.find((s) => s.key === msg.session);
        setTimeout(() => {
          if (!fail && target) target.title = msg.title;
          this.emit({ t: 'result', id: msg.id, ok: !fail, error: fail ? 'транскрипт сессии не найден' : null });
          // Агент после переименования присылает список сессий заново.
          if (!fail) this.agents();
        }, 0);
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

const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'aipanel-rename-'));
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

// Печатает имя в открытое поле и нажимает Enter; `blur` — уход из поля вместо Enter.
// Headless Chrome не рассылает focus/blur при программном `input.blur()`, поэтому
// уход из поля проверяется событием `blur` — на нём же держится сохранение.
const typeName = (selector, value, how = 'enter') =>
  evaluate(`(() => {
    const input = document.querySelector('${selector}');
    if (!input) return false;
    input.value = ${JSON.stringify(value)};
    input.dispatchEvent(new Event('input', { bubbles: true }));
    ${
      how === 'blur'
        ? "input.dispatchEvent(new FocusEvent('blur'));"
        : "input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));"
    }
    return true;
  })()`);

const renameInput = (selector) =>
  evaluate(`(() => {
    const input = document.querySelector('${selector}');
    if (!input) return null;
    const box = input.getBoundingClientRect();
    const row = input.closest('.session-row');
    const rowBox = row && row.getBoundingClientRect();
    const style = getComputedStyle(input);
    return {
      value: input.value,
      focused: document.activeElement === input,
      selection: [input.selectionStart, input.selectionEnd],
      width: box.width,
      height: box.height,
      left: box.left,
      right: box.right,
      font: style.fontSize,
      zIndex: style.zIndex,
      rowInside: rowBox ? box.left >= rowBox.left - 1 && box.right <= rowBox.right + 1 : null,
      viewport: window.innerWidth,
    };
  })()`);

const click = (selector, index = 0) => evaluate(`document.querySelectorAll('${selector}')[${index}].click()`);
const text = (selector) => evaluate(`document.querySelector('${selector}').textContent`);

try {
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await send('Page.navigate', { url: pageUrl });

  if (!(await waitFor(`document.querySelectorAll('#sessions .session').length === 2`))) {
    throw new Error('сессии не появились в списке');
  }

  // --- шапка: нажатие на заголовок открывает поле с выделенным именем
  await click('#sessions .session');
  if (!(await waitFor(`document.getElementById('title').textContent === 'Старое имя'`))) {
    throw new Error('сессия не открылась');
  }
  await click('#title');
  let field = await renameInput('.rename-header');
  check('заголовок в шапке открывает поле', field !== null);
  check('поле показывает текущее имя и выделяет его', field && field.value === 'Старое имя' && field.focused, field);
  check(
    'поле в шапке выделено целиком',
    field && field.selection[0] === 0 && field.selection[1] === 'Старое имя'.length,
    field,
  );

  // Enter отправляет команду агенту; новый список приходит сразу после ответа.
  await typeName('.rename-header', 'Новое имя');
  if (!(await waitFor(`window.__renames.length === 1`))) throw new Error('команда переименования не ушла');
  const sent = await evaluate(`window.__renames[0]`);
  check('переименование уходит одной командой', sent.t === 'rename' && sent.agent === 'ui-test' && sent.session === 'opencode:a', sent);
  check('в команде новое имя', sent.title === 'Новое имя', sent);
  if (!(await waitFor(`document.getElementById('title').textContent === 'Новое имя'`))) {
    throw new Error('новое имя не попало в шапку');
  }
  check('после ответа агента шапка показывает новое имя', true);
  check('поле в шапке закрылось', (await evaluate(`document.querySelector('.rename-header') === null`)) === true);

  // --- список: имя открытой сессии правится нажатием (слева панель видна, пока
  // сессия открыта). Нажатие на имя другой строки по-прежнему открывает её.
  const marked = await evaluate(
    `document.querySelector('#sessions .session.active .name').classList.contains('rename-target')`,
  );
  check('у открытой сессии имя помечено как правимое', marked === true);
  await click('#sessions .session.active .name');
  field = await renameInput('.rename-list');
  check('нажатие на имя открытой сессии в списке открывает поле', field !== null);
  check('поле в списке стоит поверх всей строки', field && field.rowInside && field.zIndex === '3', field);
  check('в списке поле показывает текущее имя', field && field.value === 'Новое имя', field);

  // Esc — отмена, ничего не отправляем.
  await evaluate(`document.querySelector('.rename-list').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  check('Esc закрывает поле', (await evaluate(`document.querySelector('.rename-list') === null`)) === true);
  check('после Esc команда не ушла', (await evaluate(`window.__renames.length`)) === 1);

  // Уход из поля сохраняет, как и Enter.
  await click('#sessions .session.active .name');
  await typeName('.rename-list', 'Имя из списка', 'blur');
  if (!(await waitFor(`window.__renames.length === 2`))) throw new Error('уход из поля не сохранил имя');
  check('уход из поля сохраняет имя', (await evaluate(`window.__renames[1].title`)) === 'Имя из списка');
  if (!(await waitFor(`document.querySelector('#sessions .session.active .name').textContent === 'Имя из списка'`))) {
    throw new Error('список не показал новое имя');
  }
  check('список показывает новое имя', true);

  // После выхода имя последней сессии снова открывает её, а не правится:
  // иначе на телефоне нельзя было бы вернуться в только что закрытую сессию.
  await click('#back');
  if (!(await waitFor(`!document.getElementById('app').classList.contains('viewing')`))) {
    throw new Error('не вернулись к списку');
  }
  const targetAfterBack = await evaluate(
    `document.querySelector('#sessions .session.active .name').classList.contains('rename-target')`,
  );
  check('после выхода имя не считается правимым', targetAfterBack === false);
  await click('#sessions .session .name', 0);
  if (!(await waitFor(`document.getElementById('title').textContent === 'Имя из списка'`))) {
    throw new Error('нажатие на имя последней сессии не открыло её');
  }
  check('нажатие на имя последней сессии открывает её', (await evaluate(`document.querySelector('.rename') === null`)) === true);
  check('поле при этом не открылось', (await evaluate(`window.__renames.length`)) === 2);

  // Имя другой сессии тоже открывает её, а не правится.
  await click('#sessions .session .name', 1);
  if (!(await waitFor(`document.getElementById('title').textContent === 'Вторая сессия'`))) {
    throw new Error('нажатие на имя другой сессии не открыло её');
  }
  check('имя неоткрытой сессии открывает сессию', (await evaluate(`window.__renames.length`)) === 2);
  check('поле при этом не открылось', (await evaluate(`document.querySelector('.rename') === null`)) === true);

  // --- ошибка агента: имя остаётся прежним, ошибка видна
  await evaluate(`window.__renameFail = true`);
  await click('#title');
  await typeName('.rename-header', 'Не сохранится');
  if (!(await waitFor(`window.__renames.length === 3`))) throw new Error('ошибочное переименование не ушло');
  if (!(await waitFor(`document.getElementById('hint').textContent.includes('Ошибка')`))) {
    throw new Error('ошибка агента не показана');
  }
  check('ошибка агента видна в подсказке', (await text('#hint')).includes('транскрипт сессии не найден'));
  check('после ошибки имя прежнее', (await text('#title')) === 'Вторая сессия');
  await evaluate(`window.__renameFail = false`);

  // --- телефон: поле не выходит за экран и не даёт Safari приблизить страницу
  await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
  await sleep(100);
  await click('#title');
  let mobile = await renameInput('.rename-header');
  check('на телефоне заголовок тоже правится', mobile !== null);
  check('поле в шапке не шире экрана', mobile && mobile.left >= 0 && mobile.right <= mobile.viewport, mobile);
  check('поле на телефоне не приближает страницу (16px)', mobile && mobile.font === '16px', mobile);
  await evaluate(`document.querySelector('.rename-header').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);

  await click('#back');
  if (!(await waitFor(`!document.getElementById('app').classList.contains('viewing')`))) {
    throw new Error('на телефоне не вернулись к списку');
  }
  // В списке на телефоне имя последней сессии открывает её: поле правки живёт
  // в шапке открытой сессии, куда и ведёт нажатие.
  await click('#sessions .session.active .name');
  if (!(await waitFor(`document.getElementById('app').classList.contains('viewing')`))) {
    throw new Error('на телефоне нажатие на имя не открыло сессию');
  }
  check(
    'на телефоне имя в списке открывает сессию, а не поле',
    (await evaluate(`document.querySelector('.rename') === null`)) === true &&
      (await text('#title')) === 'Вторая сессия',
  );
  await click('#title');
  mobile = await renameInput('.rename-header');
  check('на телефоне поле в шапке на месте заголовка', mobile && mobile.left >= 0 && mobile.right <= mobile.viewport, mobile);
  await evaluate(`document.querySelector('.rename-header').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);

  check('ошибок на странице нет', pageErrors.length === 0, pageErrors);
} catch (e) {
  failures.push(String(e));
  console.error(`\nТест упал: ${e}`);
  console.error('Ошибки страницы:', pageErrors);
  try {
    const shot = await send('Page.captureScreenshot', { format: 'png' });
    const file = path.join(os.tmpdir(), 'aipanel-rename-failure.png');
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
