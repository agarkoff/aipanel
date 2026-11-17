// Сквозная проверка на живом стенде: настоящий сервер, настоящий агент и
// настоящий браузер. Сессия Claude Code — синтетический транскрипт во временном
// каталоге, поэтому ничего из машины не читается и не меняется.
//
// Проверяется путь, которого не видят тесты со заглушками: вход по паролю,
// обнаружение сессии агентом, лента из транскрипта, избранное и переименование
// (браузер -> сервер -> агент -> файл транскрипта -> обратно в список).
//
// Запуск: node tests/e2e/real.mjs
// Нужны cargo, Chrome (или Edge; путь можно задать в CHROME_PATH) и свободный
// порт. Без них тест пропускается с кодом 0.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const BIN = path.join(ROOT, 'target/debug');
const EXE = process.platform === 'win32' ? '.exe' : '';

const USER = 'e2e';
const PASS = 'e2e-pass-1234';
const TOKEN = 'e2e-token-1234';
const SESSION_ID = '11111111-2222-3333-4444-555555555555';
const CWD = 'D:\\Projects\\demo';
const USER_TEXT = 'привет из e2e';
const REPLY_TEXT = 'ответ из e2e';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function findChrome() {
  return [
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
  ]
    .filter(Boolean)
    .find((p) => fs.existsSync(p));
}

const chromePath = findChrome();
if (!chromePath) {
  console.log('Chrome не найден: e2e пропущен (задайте путь в CHROME_PATH).');
  process.exit(0);
}

// ---------------------------------------------------------------- сборка

function build() {
  return new Promise((resolve, reject) => {
    const child = spawn('cargo', ['build', '-q', '-p', 'aipanel-server', '-p', 'aipanel-agent'], {
      cwd: ROOT,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (out += d));
    child.on('error', reject);
    child.on('close', (code) => (code === 0 ? resolve() : reject(new Error(out))));
  });
}

try {
  await build();
} catch (e) {
  console.log(`Не удалось собрать агент и сервер: e2e пропущен.\n${e.message}`);
  process.exit(0);
}

// ---------------------------------------------------------------- данные

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'aipanel-e2e-'));
const claudeHome = path.join(tmp, 'claude');
const stateHome = path.join(tmp, 'state');
const project = path.join(claudeHome, 'projects', 'demo');
fs.mkdirSync(project, { recursive: true });
fs.mkdirSync(stateHome, { recursive: true });

// Транскрипт в том же виде, в каком его пишет Claude Code: у каждой строки свой
// uuid (по нему панель различает элементы), каталог для заголовка и токены хода.
const transcript = path.join(project, `${SESSION_ID}.jsonl`);
fs.writeFileSync(
  transcript,
  [
    JSON.stringify({
      type: 'user',
      uuid: 'u1',
      timestamp: '2026-11-17T09:00:00.000Z',
      cwd: CWD,
      message: { role: 'user', content: USER_TEXT },
    }),
    JSON.stringify({
      type: 'assistant',
      uuid: 'a1',
      timestamp: '2026-11-17T09:00:05.000Z',
      cwd: CWD,
      message: {
        id: 'msg_1',
        role: 'assistant',
        content: [{ type: 'text', text: REPLY_TEXT }],
        usage: { input_tokens: 12, cache_creation_input_tokens: 0, cache_read_input_tokens: 0, output_tokens: 34 },
      },
    }),
  ].join('\n') + '\n',
  'utf8',
);

// ---------------------------------------------------------------- процессы

const children = [];
function start(name, file, args, env) {
  const child = spawn(file, args, { env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.log = '';
  child.stdout.on('data', (d) => (child.log += d));
  child.stderr.on('data', (d) => (child.log += d));
  children.push({ name, child });
  return child;
}

function stopAll() {
  for (const { child } of children) {
    try {
      child.kill();
    } catch {}
  }
}

function logs() {
  return children.map(({ name, child }) => `[${name}]\n${child.log}`.trim()).join('\n\n');
}

const freePort = () =>
  new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.on('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const { port } = probe.address();
      probe.close(() => resolve(port));
    });
  });

const failures = [];
function check(name, ok, details) {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${name}${ok || details === undefined ? '' : ` — ${JSON.stringify(details)}`}`);
  if (!ok) failures.push(name);
}

let browser = null;

try {
  // ---------------------------------------------------------------- сервер
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const server = start(
    'сервер',
    path.join(BIN, `aipanel-server${EXE}`),
    [],
    { PANEL_USERS: JSON.stringify({ [USER]: { password: PASS, token: TOKEN } }), BIND: `127.0.0.1:${port}` },
  );

  let up = false;
  for (let i = 0; i < 200 && !up; i++) {
    if (server.exitCode !== null) break;
    try {
      up = (await fetch(`${base}/healthz`)).ok;
    } catch {}
    if (!up) await sleep(100);
  }
  if (!up) throw new Error('сервер не поднялся:\n' + logs());
  console.log(`стенд на ${port}`);

  // Части панели отдаются таблицей SCRIPTS из assets.rs, а не отдельным маршрутом.
  const assets = await Promise.all(
    ['/app.core.js', '/app.main.js', '/style.css'].map((p) => fetch(`${base}${p}`).then((r) => r.text().then((t) => [p, r.status, t.length]))),
  );
  check('сервер отдаёт части панели', assets.every(([, status, size]) => status === 200 && size > 0), assets);

  // ---------------------------------------------------------------- агент
  const cfg = path.join(tmp, 'agent.toml');
  fs.writeFileSync(
    cfg,
    `server = "ws://127.0.0.1:${port}/agent"
token = "${TOKEN}"
name = "e2e"

[claude]
enabled = true
bin = "claude"
home = "${claudeHome.replace(/\\/g, '/')}"
recent_days = 7
max_recent = 30
resume_args = []
trust_dirs = false

[opencode]
enabled = false
`,
    'utf8',
  );
  start('агент', path.join(BIN, `aipanel-agent${EXE}`), ['--config', cfg, '--home', stateHome], {});

  // ---------------------------------------------------------------- браузер
  const profile = path.join(tmp, 'chrome');
  browser = spawn(
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

  let devPort = 0;
  for (let i = 0; i < 100 && !devPort; i++) {
    const file = path.join(profile, 'DevToolsActivePort');
    if (fs.existsSync(file)) devPort = Number(fs.readFileSync(file, 'utf8').split('\n')[0]);
    if (!devPort) await sleep(100);
  }
  let page = null;
  for (let i = 0; i < 50 && !page; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${devPort}/json/list`)).json();
      page = list.find((p) => p.type === 'page');
    } catch {}
    if (!page) await sleep(100);
  }
  if (!page) throw new Error('браузер не открыл вкладку');

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
  const evaluate = async (expression) => {
    const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
    return r.result.value;
  };
  const waitFor = async (expression, ms = 10000) => {
    const until = Date.now() + ms;
    while (Date.now() < until) {
      try {
        if (await evaluate(expression)) return true;
      } catch {}
      await sleep(50);
    }
    return false;
  };

  // ---------------------------------------------------------------- проверки
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Page.navigate', { url: `${base}/` });

  check('страница отдаётся сервером', await waitFor(`!!document.querySelector('#login') && !document.querySelector('#login').hidden`));
  await evaluate(`(() => {
    document.querySelector('#password').value = ${JSON.stringify(PASS)};
    document.querySelector('#login').requestSubmit();
    return true;
  })()`);
  check('вход по паролю', await waitFor(`!document.querySelector('#app').hidden`, 15000));

  // Агент сканирует транскрипты раз в две секунды: ждём, пока сессия доедет.
  check(
    'сессия Claude Code из транскрипта видна в списке',
    await waitFor(
      `!!document.querySelector('.session-row') && document.querySelector('#sessions').innerText.includes('demo')`,
      30000,
    ),
    await evaluate(`document.querySelector('#sessions')?.innerText || ''`),
  );

  await evaluate(`(() => { document.querySelector('.session-row .session').click(); return true; })()`);
  check(
    'лента показывает оба сообщения транскрипта',
    await waitFor(
      `(() => {
         const t = document.querySelector('#feed')?.innerText || '';
         return t.includes(${JSON.stringify(USER_TEXT)}) && t.includes(${JSON.stringify(REPLY_TEXT)});
       })()`,
      15000,
    ),
    await evaluate(`document.querySelector('#feed')?.innerText || ''`),
  );

  // Избранное: звезда в браузере -> агент пишет favorites.json в своём каталоге.
  await evaluate(`(() => { const star = document.querySelector('.fav'); if (star) star.click(); return true; })()`);
  check('звезда избранного нажалась', await waitFor(`!!document.querySelector('.fav[aria-pressed="true"]')`, 10000));
  const favorites = path.join(stateHome, 'favorites.json');
  let favoritesText = '';
  for (let i = 0; i < 100 && !favoritesText; i++) {
    if (fs.existsSync(favorites)) favoritesText = fs.readFileSync(favorites, 'utf8');
    if (!favoritesText) await sleep(100);
  }
  check(
    'агент записал избранное в свой каталог',
    favoritesText.includes(SESSION_ID) && favoritesText.includes(USER_TEXT),
    favoritesText.slice(0, 200),
  );

  // Переименование: браузер -> агент -> строка custom-title в транскрипте.
  await evaluate(`(() => { document.querySelector('#rename').click(); return true; })()`);
  const typed = await evaluate(`(() => {
    const input = document.querySelector('#title input.rename');
    if (!input) return false;
    input.value = 'E2E сессия';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    return true;
  })()`);
  check('поле переименования открылось', typed);
  check(
    'агент подтвердил переименование',
    await waitFor(`document.querySelector('#hint').innerText.includes('переименована')`, 15000),
    await evaluate(`document.querySelector('#hint')?.innerText || ''`),
  );
  const renamed = fs.readFileSync(transcript, 'utf8');
  check('в транскрипте появился custom-title', renamed.includes('"custom-title"') && renamed.includes('E2E сессия'));
  check(
    'новое имя вернулось в список сессий',
    await waitFor(`document.querySelector('#sessions').innerText.includes('E2E сессия')`, 15000),
    await evaluate(`document.querySelector('#sessions')?.innerText || ''`),
  );

  check('ошибок на странице нет', pageErrors.length === 0, pageErrors);
} catch (e) {
  failures.push('исключение');
  console.log(`Ошибка теста: ${e.message}`);
} finally {
  try {
    browser?.kill();
  } catch {}
  stopAll();
  await sleep(300);
  try {
    fs.rmSync(tmp, { recursive: true, force: true });
  } catch {}
}

if (failures.length) {
  console.log('\nлог стенда:\n' + logs());
  console.log(`\nНе прошло проверок: ${failures.length}`);
  process.exit(1);
}
console.log('\nВсе проверки прошли.');
