'use strict';

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};

const STATUS = {
  busy: 'работает',
  idle: 'ждёт ввода',
  waiting: 'ждёт разрешения',
  shell: 'выполняет команду',
  stopped: 'не запущена',
  ready: 'не активна',
};
const DELIVERY = {
  console: 'Сессия открыта в терминале: текст будет набран в её консоли и отправлен клавишей Enter.',
  window: 'Сессия не открыта: на компьютере откроется окно консоли с этой сессией, сообщение уйдёт в него первым запросом.',
  headless: 'Сессия не открыта: сообщение выполнит фоновый процесс claude -p --resume в каталоге сессии.',
  api: 'Сообщение уйдёт через локальный API opencode.',
  none: 'Отправка в эту сессию недоступна на этой платформе.',
};

let ws = null;
let agents = [];
let selected = null; // { agent, session }
let cmdSeq = 0;
const nodes = new Map(); // id элемента ленты -> DOM-узел

// Запуск новой сессии: id команды, её параметры и ожидаемая сессия.
let launchId = null;
let launchRequest = null;
let pendingLaunch = null;
let copyIdTimer = null;
// Каталоги, показанные в подсказках для текущего выбранного компьютера.
let launchDirCache = { agent: '', dirs: [] };

// История запросов для стрелок вверх/вниз в поле ввода.
// Основной источник — лента сессии, поэтому в историю попадают и сообщения,
// отправленные из окна консоли или с другого устройства.
const feedRequests = new Map(); // id элемента ленты -> текст запроса
// Отправленные из этого браузера, но ещё не пришедшие в ленту.
const sentRequests = new Map(); // "агент\nсессия" -> [текст, ...]
let histPos = 0; // 0 — черновик, 1 — последний запрос, 2 — предыдущий…
let histDraft = ''; // что было в поле до начала просмотра истории

// ------------------------------------------------------------ вход

async function boot() {
  const res = await fetch('/api/me');
  if (res.status === 204) start();
  else $('login').hidden = false;
}

$('login').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('login-error').textContent = '';
  const res = await fetch('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password: $('password').value }),
  });
  if (res.ok) {
    $('password').value = '';
    $('login').hidden = true;
    start();
  } else {
    $('login-error').textContent = (await res.text()) || 'Ошибка входа';
  }
});

$('logout').addEventListener('click', async () => {
  await fetch('/api/logout', { method: 'POST' });
  location.reload();
});

// ------------------------------------------------------------ соединение

function start() {
  $('app').hidden = false;
  connect();
  setInterval(renderSessions, 30000); // обновляет «N мин назад»
  setInterval(checkPendingLaunch, 3000); // на случай, если список сессий не изменился
}

function connect() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/ws`);
  ws.onopen = () => {
    $('conn').classList.add('on');
    if (selected) send({ t: 'watch', agent: selected.agent, session: selected.session });
  };
  ws.onmessage = (e) => onMessage(JSON.parse(e.data));
  ws.onclose = async () => {
    $('conn').classList.remove('on');
    const res = await fetch('/api/me').catch(() => null);
    if (res && res.status === 401) location.reload();
    else setTimeout(connect, 2000);
  };
}

function send(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(msg));
    return true;
  }
  return false;
}

function onMessage(msg) {
  if (msg.t === 'agents') {
    agents = msg.agents;
    renderSessions();
    renderHeader();
    checkPendingLaunch();
  } else if (msg.t === 'history') {
    if (!selected || msg.agent !== selected.agent || msg.session !== selected.session) return;
    applyHistory(msg);
  } else if (msg.t === 'result') {
    if (launchId && msg.id === launchId) onLaunchResult(msg);
    else setHint(msg.ok ? 'Доставлено агенту.' : `Ошибка: ${msg.error || 'неизвестная'}`, !msg.ok);
  }
}

// ------------------------------------------------------------ список сессий

function ago(ts) {
  if (!ts) return '';
  const s = Math.max(0, (Date.now() - ts) / 1000);
  if (s < 60) return 'сейчас';
  if (s < 3600) return `${Math.floor(s / 60)} мин`;
  if (s < 86400) return `${Math.floor(s / 3600)} ч`;
  return `${Math.floor(s / 86400)} дн`;
}

function dotClass(s, online) {
  if (!online) return 'dot';
  if (s.status === 'busy' || s.status === 'shell') return 'dot busy';
  if (s.status === 'waiting') return 'dot waiting';
  if (s.status === 'idle') return 'dot idle';
  return 'dot';
}

function renderSessions() {
  const box = $('sessions');
  const q = $('filter').value.trim().toLowerCase();
  box.replaceChildren();
  if (!agents.length) {
    box.append(el('div', 'empty', 'Нет подключённых агентов. Запустите aipanel-agent на своей машине.'));
    return;
  }
  for (const a of agents) {
    const head = el('div', 'agent');
    head.append(el('span', a.online ? 'dot idle' : 'dot'), el('span', '', `${a.name}${a.online ? '' : ' — не в сети'}`));
    box.append(head);
    const list = a.sessions.filter((s) => !q || `${s.title} ${s.cwd}`.toLowerCase().includes(q));
    if (!list.length) box.append(el('div', 'empty', 'Сессий нет.'));
    for (const s of list) {
      const btn = el('button', 'session');
      btn.type = 'button';
      if (selected && selected.agent === a.name && selected.session === s.key) btn.classList.add('active');
      btn.append(
        el('span', dotClass(s, a.online)),
        el('span', 'name', s.title || s.key),
        el('span', 'when', ago(s.updated_at)),
        el('span', 'meta', `${s.tool} · ${s.cwd}`),
      );
      btn.addEventListener('click', () => select(a.name, s.key));
      box.append(btn);
    }
  }
}

$('filter').addEventListener('input', renderSessions);

function current() {
  if (!selected) return null;
  const agent = agents.find((a) => a.name === selected.agent);
  const session = agent && agent.sessions.find((s) => s.key === selected.session);
  return agent && session ? { agent, session } : null;
}

// Ключ сессии — `tool:id`; пользователю нужен сам id (для `--resume`, `-s` и т.п.).
function sessionId(key) {
  const colon = key.indexOf(':');
  return colon < 0 ? key : key.slice(colon + 1);
}

function select(agent, session) {
  selected = { agent, session };
  nodes.clear();
  feedRequests.clear();
  resetHistoryNav();
  clearTimeout(copyIdTimer);
  $('copy-id').classList.remove('copied');
  $('feed').replaceChildren(el('div', 'empty', 'Загрузка ленты…'));
  setHint('');
  $('app').classList.add('viewing');
  // Запись в истории нужна, чтобы системная кнопка «Назад» на телефоне
  // возвращала к списку сессий, а не уводила со страницы.
  if (!(history.state && history.state.viewing)) history.pushState({ viewing: true }, '');
  send({ t: 'watch', agent, session });
  renderSessions();
  renderHeader();
}

function closeView() {
  $('app').classList.remove('viewing');
  selected = null;
  send({ t: 'unwatch' });
  renderSessions();
  renderHeader();
}

$('back').addEventListener('click', () => {
  if (history.state && history.state.viewing) history.back();
  else closeView();
});
window.addEventListener('popstate', () => {
  if (selected) closeView();
});

// ------------------------------------------------------------ шапка и лента

function renderHeader() {
  const cur = current();
  const badge = $('status');
  if (!cur) {
    $('title').textContent = selected ? selected.session : 'Выберите сессию';
    $('subtitle').textContent = '';
    $('session-id').hidden = true;
    $('copy-id').hidden = true;
    badge.hidden = true;
    $('interrupt').hidden = true;
    $('composer').hidden = true;
    return;
  }
  const { agent, session } = cur;
  $('title').textContent = session.title || session.key;
  $('subtitle').textContent = `${agent.name} · ${session.tool} · ${session.cwd}`;
  $('session-id').hidden = false;
  $('session-id').textContent = sessionId(session.key);
  $('copy-id').hidden = false;
  badge.hidden = false;
  badge.className = `badge ${agent.online ? session.status : ''}`;
  badge.textContent = agent.online ? STATUS[session.status] || session.status : 'агент не в сети';
  const busy = agent.online && (session.status === 'busy' || session.status === 'shell');
  $('interrupt').hidden = !busy;
  const canSend = agent.online && session.delivery !== 'none';
  $('composer').hidden = false;
  $('text').disabled = !canSend;
  $('composer').querySelector('button').disabled = !canSend;
  if (!$('hint').dataset.sticky) {
    $('hint').className = 'hint';
    $('hint').textContent = agent.online ? DELIVERY[session.delivery] || '' : '';
  }
}

function setHint(text, isError) {
  const hint = $('hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
  if (text) {
    hint.dataset.sticky = '1';
    clearTimeout(setHint.timer);
    setHint.timer = setTimeout(() => {
      delete hint.dataset.sticky;
      renderHeader();
    }, isError ? 15000 : 4000);
  } else {
    delete hint.dataset.sticky;
  }
}

$('copy-id').addEventListener('click', async () => {
  const cur = current();
  if (!cur) return;
  const ok = await copyToClipboard(sessionId(cur.session.key));
  $('copy-id').classList.toggle('copied', ok);
  clearTimeout(copyIdTimer);
  if (ok) copyIdTimer = setTimeout(() => $('copy-id').classList.remove('copied'), 1500);
  setHint(ok ? 'Идентификатор скопирован.' : 'Не удалось скопировать идентификатор.', !ok);
});

async function copyToClipboard(text) {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {}
  // Запасной путь: Clipboard API нет на странице без HTTPS (панель в локальной сети).
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.opacity = '0';
  document.body.append(area);
  area.select();
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch {}
  area.remove();
  return ok;
}

function firstLine(text) {
  const line = text.split('\n', 1)[0];
  return line.length > 140 ? `${line.slice(0, 140)}…` : line;
}

function buildItem(item) {
  const node = el('div', `item ${item.role} ${item.kind}`);
  if (item.kind === 'text') {
    const body = el('div', 'body md');
    body.append(renderMarkdown(item.text));
    node.append(body);
  } else if (item.kind === 'error') {
    node.append(el('div', 'body', item.text));
  } else {
    const details = el('details');
    const label =
      item.kind === 'thinking' ? 'Размышление'
      : item.kind === 'tool_use' ? `${item.tool || 'инструмент'}: ${firstLine(item.text)}`
      : `Результат: ${firstLine(item.text)}`;
    details.append(el('summary', '', label), el('div', 'body', item.text));
    // Размышления раскрыты по умолчанию; вызовы инструментов и результаты свёрнуты.
    if (item.kind === 'thinking') details.open = true;
    node.append(details);
  }
  if (item.ts && (item.kind === 'text' || item.kind === 'error')) {
    node.append(el('div', 'time', new Date(item.ts).toLocaleString()));
  }
  return node;
}

function applyHistory(msg) {
  const feed = $('feed');
  const stick = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
  if (msg.reset) {
    nodes.clear();
    feedRequests.clear();
    feed.replaceChildren();
    if (!msg.items.length) feed.append(el('div', 'empty', 'Лента пуста.'));
  } else if (msg.items.length && !nodes.size) {
    feed.replaceChildren();
  }
  for (const item of msg.items) {
    // Запросы из ленты — история для стрелок в поле ввода.
    if (item.role === 'user' && (item.kind === 'text' || item.kind === 'queued')) {
      feedRequests.set(item.id, item.text);
      consumeSentRequest(item.text);
    }
    const node = buildItem(item);
    const old = nodes.get(item.id);
    if (old) {
      const oldDetails = old.querySelector('details');
      const newDetails = node.querySelector('details');
      if (oldDetails && newDetails) newDetails.open = oldDetails.open;
      old.replaceWith(node);
    } else {
      feed.append(node);
    }
    nodes.set(item.id, node);
  }
  if (msg.reset || stick) feed.scrollTop = feed.scrollHeight;
}

// ------------------------------------------------------------ история запросов

function historyKey() {
  return selected ? `${selected.agent}\n${selected.session}` : '';
}

function historyEntries() {
  // Сначала запросы из ленты (в хронологическом порядке), затем отправленные
  // из этого браузера, которые агент ещё не успел показать в ленте.
  return [...feedRequests.values(), ...(sentRequests.get(historyKey()) || [])];
}

// Возврат к обычному вводу: текущее содержимое поля становится черновиком.
function resetHistoryNav() {
  histPos = 0;
  histDraft = $('text').value;
}

function rememberRequest(text) {
  const key = historyKey();
  if (!key) return;
  let list = sentRequests.get(key);
  if (!list) sentRequests.set(key, (list = []));
  list.push(text);
}

// Запрос дошёл до ленты: убираем его временную копию, иначе в истории был бы дубль.
function consumeSentRequest(text) {
  const key = historyKey();
  const list = sentRequests.get(key);
  if (!list) return;
  const i = list.indexOf(text);
  if (i < 0) return;
  list.splice(i, 1);
  if (!list.length) sentRequests.delete(key);
}

// step = 1 — к более старому запросу, -1 — к более новому.
// false означает, что событие не наше: стрелка должна двигать каретку как обычно.
function navigateHistory(step) {
  const list = historyEntries();
  const area = $('text');
  if (step > 0) {
    if (!list.length || histPos >= list.length) return false;
    // Просмотр начинаем только с начала поля, иначе Up листает строки текста.
    // В однострочном черновике двигать каретку вверх некуда, поэтому там Up
    // всегда вызывает историю — как в оболочке.
    // Дальше Up продолжает листать, даже если каретка в конце: текст не менялся.
    if (histPos === 0) {
      const atStart = area.selectionStart === 0 && area.selectionEnd === 0;
      if (!atStart && area.value.includes('\n')) return false;
      histDraft = area.value;
    }
    histPos += 1;
    area.value = list[list.length - histPos];
  } else {
    if (histPos === 0) return false;
    histPos -= 1;
    area.value = histPos === 0 ? histDraft : list[list.length - histPos];
  }
  // Каретка в конец, чтобы сразу править запрос.
  area.setSelectionRange(area.value.length, area.value.length);
  autoGrow();
  return true;
}

// ------------------------------------------------------------ новая сессия

// Пути с разных компьютеров могут отличаться регистром и разделителями.
function normCwd(path) {
  return path.replace(/[\\/]+$/, '').replace(/\\/g, '/').toLowerCase();
}

// Каталоги всех сессий агента — и Claude Code, и opencode — без повторов,
// сначала недавно использованные.
function sessionDirs(agent) {
  const map = new Map();
  for (const s of agent ? agent.sessions : []) {
    if (!s.cwd) continue;
    const key = normCwd(s.cwd);
    const seen = map.get(key);
    if (seen) {
      seen.tools.add(s.tool);
      seen.updated = Math.max(seen.updated, s.updated_at || 0);
    } else {
      map.set(key, { cwd: s.cwd, tools: new Set([s.tool]), updated: s.updated_at || 0 });
    }
  }
  return [...map.values()].sort((a, b) => b.updated - a.updated);
}

function fillLaunchAgents() {
  const online = agents.filter((a) => a.online);
  const sel = $('launch-agent');
  const prev = sel.value;
  sel.replaceChildren();
  for (const a of online) {
    const opt = el('option', '', a.name);
    opt.value = a.name;
    sel.append(opt);
  }
  sel.value = online.some((a) => a.name === prev) ? prev : online[0].name;
  // Один компьютер — выбирать не из чего.
  $('launch-agent-row').hidden = online.length < 2;
}

function fillLaunchDirs() {
  const agent = agents.find((a) => a.name === $('launch-agent').value);
  const dirs = sessionDirs(agent);
  launchDirCache = { agent: agent ? agent.name : '', dirs };
  return dirs;
}

// Подсказки каталогов в поле ввода: список открывается и при наборе текста.
// <datalist> для этого не годится — он показывает варианты, только пока ввод
// совпадает с началом пути, и при наборе «D:\Proj\…» список пропадает.
const cwdSuggest = Suggest.attach($('launch-cwd'), {
  // Порядок — как в sessionDirs(): от недавно использованных к старым.
  items: () => launchDirCache.dirs.map((d) => ({ value: d.cwd })),
  // При открытии окна список не распахивается сам: фокус ставится программно.
  openOnFocus: false,
  // Поле уже заполнено каталогом открытой сессии: по клику показываем весь список.
  openAllWhenExact: true,
  emptyText: 'Такого каталога ещё не было. Можно ввести путь вручную.',
});

function openLaunch() {
  const online = agents.filter((a) => a.online);
  if (!online.length) {
    setHint('Нет подключённых агентов: новую сессию запускать негде.', true);
    return;
  }
  fillLaunchAgents();
  if (selected && online.some((a) => a.name === selected.agent)) $('launch-agent').value = selected.agent;
  const dirs = fillLaunchDirs();
  const cur = current();
  const sameAgent = cur && cur.agent.name === $('launch-agent').value;
  if (sameAgent && (cur.session.tool === 'claude' || cur.session.tool === 'opencode')) {
    $('launch-tool').value = cur.session.tool;
  }
  // Каталог открытой сессии — самый вероятный выбор, иначе самый свежий из списка.
  $('launch-cwd').value = sameAgent && cur.session.cwd ? cur.session.cwd : (dirs[0]?.cwd ?? '');
  $('launch-ok').disabled = false;
  launchHint('');
  $('launch').showModal();
  $('launch-cwd').focus();
}

function launchHint(text, isError) {
  const hint = $('launch-hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
}

$('new-session').addEventListener('click', openLaunch);
$('launch-cancel').addEventListener('click', () => $('launch').close());

// Клик по пустому месту вокруг окна (затемнение) закрывает его: у <dialog>
// затемнение — часть самого элемента, поэтому цель события — сам dialog.
// Пока запуск идёт, окно не закрываем: ошибку запуска должно быть видно.
$('launch').addEventListener('click', (e) => {
  if (e.target === e.currentTarget && launchId === null) e.currentTarget.close();
});

$('launch-agent').addEventListener('change', () => {
  // Если в поле был путь из подсказок прошлого компьютера, заменяем его на путь
  // нового: чужие абсолютные пути на другой машине обычно не существуют.
  const prev = launchDirCache;
  const cwd = $('launch-cwd').value.trim();
  const wasSuggested = !cwd || prev.dirs.some((d) => normCwd(d.cwd) === normCwd(cwd));
  const dirs = fillLaunchDirs();
  if (wasSuggested) $('launch-cwd').value = dirs[0]?.cwd ?? '';
});

$('launch-form').addEventListener('submit', (e) => {
  e.preventDefault();
  const agent = $('launch-agent').value;
  const tool = $('launch-tool').value;
  const cwd = $('launch-cwd').value.trim();
  if (!agent) {
    launchHint('Нет подключённых агентов.', true);
    return;
  }
  if (!cwd) {
    launchHint('Укажите каталог.', true);
    $('launch-cwd').focus();
    return;
  }
  const a = agents.find((x) => x.name === agent);
  const keys = new Set(a ? a.sessions.map((s) => s.key) : []);
  const id = String(++cmdSeq);
  if (!send({ t: 'start', id, agent, tool, cwd })) {
    launchHint('Нет соединения с сервером.', true);
    return;
  }
  launchId = id;
  launchRequest = { agent, tool, cwd, keys };
  $('launch-ok').disabled = true;
  launchHint('Запускаем…');
});

function onLaunchResult(msg) {
  const req = launchRequest;
  launchId = null;
  launchRequest = null;
  $('launch-ok').disabled = false;
  if (!msg.ok) {
    launchHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  $('launch').close();
  pendingLaunch = {
    agent: req.agent,
    tool: req.tool,
    cwd: req.cwd,
    key: msg.session || null,
    keys: req.keys,
    until: Date.now() + 90000,
  };
  setHint(req.tool === 'claude' ? 'Открываем окно Claude Code…' : 'Открываем окно opencode…');
  checkPendingLaunch();
}

// Новая сессия появляется в списке после сканирования; когда она нашлась,
// открываем её ленту, чтобы сразу можно было писать.
function checkPendingLaunch() {
  if (!pendingLaunch) return;
  const agent = agents.find((a) => a.name === pendingLaunch.agent);
  if (agent) {
    let hit = null;
    if (pendingLaunch.key) {
      hit = agent.sessions.find((s) => s.key === pendingLaunch.key);
    } else {
      hit = agent.sessions
        .filter((s) => s.tool === pendingLaunch.tool && !pendingLaunch.keys.has(s.key))
        .filter((s) => normCwd(s.cwd) === normCwd(pendingLaunch.cwd))
        .sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0))[0];
    }
    if (hit) {
      pendingLaunch = null;
      setHint('');
      select(agent.name, hit.key);
      return;
    }
  }
  if (Date.now() > pendingLaunch.until) {
    pendingLaunch = null;
    setHint('Новая сессия не появилась в списке. Откройте её вручную в терминале.', true);
  }
}

// ------------------------------------------------------------ команды

$('composer').addEventListener('submit', (e) => {
  e.preventDefault();
  const text = $('text').value;
  if (!selected || !text.trim()) return;
  const ok = send({ t: 'send', id: String(++cmdSeq), agent: selected.agent, session: selected.session, text });
  if (ok) {
    rememberRequest(text);
    $('text').value = '';
    resetHistoryNav();
    autoGrow();
    setHint('Отправляется…');
  } else {
    setHint('Нет соединения с сервером.', true);
  }
});

// Поле ввода растёт вместе с текстом, до 40% высоты экрана.
function autoGrow() {
  const text = $('text');
  text.style.height = 'auto';
  text.style.height = `${Math.min(text.scrollHeight + 2, window.innerHeight * 0.4)}px`;
}
$('text').addEventListener('input', () => {
  // Правка текста завершает просмотр истории: восстановленный запрос становится
  // черновиком, и следующая стрелка вверх начинает с последнего запроса.
  resetHistoryNav();
  autoGrow();
});
// На компьютере Enter отправляет, Shift+Enter переносит строку (как в Claude Code и opencode).
// На телефоне Enter переносит строку, а отправляет кнопка рядом с полем.
const touchInput = matchMedia('(pointer: coarse)').matches;
if (!touchInput) $('text').placeholder += '. Enter — отправить, Shift+Enter — новая строка';

$('text').addEventListener('keydown', (e) => {
  // isComposing: Enter при выборе слова в IME не должен ни отправлять сообщение,
  // ни листать историю.
  if (e.isComposing) return;
  if (e.key === 'Enter') {
    if (e.shiftKey || touchInput) return;
    e.preventDefault();
    $('composer').requestSubmit();
    return;
  }
  // Вверх/вниз — предыдущие запросы. Shift+стрелки оставляем выделению текста,
  // Ctrl/Alt/Meta — системным и браузерным сочетаниям.
  if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
  if (e.shiftKey || e.altKey || e.ctrlKey || e.metaKey) return;
  if (navigateHistory(e.key === 'ArrowUp' ? 1 : -1)) e.preventDefault();
});

$('interrupt').addEventListener('click', () => {
  if (!selected) return;
  send({ t: 'interrupt', id: String(++cmdSeq), agent: selected.agent, session: selected.session });
  setHint('Запрошено прерывание…');
});

boot();
