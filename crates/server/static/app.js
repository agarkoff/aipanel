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
  } else if (msg.t === 'history') {
    if (!selected || msg.agent !== selected.agent || msg.session !== selected.session) return;
    applyHistory(msg);
  } else if (msg.t === 'result') {
    setHint(msg.ok ? 'Доставлено агенту.' : `Ошибка: ${msg.error || 'неизвестная'}`, !msg.ok);
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

function select(agent, session) {
  selected = { agent, session };
  nodes.clear();
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
    badge.hidden = true;
    $('interrupt').hidden = true;
    $('composer').hidden = true;
    return;
  }
  const { agent, session } = cur;
  $('title').textContent = session.title || session.key;
  $('subtitle').textContent = `${agent.name} · ${session.tool} · ${session.cwd}`;
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
    feed.replaceChildren();
    if (!msg.items.length) feed.append(el('div', 'empty', 'Лента пуста.'));
  } else if (msg.items.length && !nodes.size) {
    feed.replaceChildren();
  }
  for (const item of msg.items) {
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

// ------------------------------------------------------------ команды

$('composer').addEventListener('submit', (e) => {
  e.preventDefault();
  const text = $('text').value;
  if (!selected || !text.trim()) return;
  const ok = send({ t: 'send', id: String(++cmdSeq), agent: selected.agent, session: selected.session, text });
  if (ok) {
    $('text').value = '';
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
$('text').addEventListener('input', autoGrow);
if (!matchMedia('(pointer: coarse)').matches) $('text').placeholder += '. Ctrl+Enter — отправить';

$('text').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    $('composer').requestSubmit();
  }
});

$('interrupt').addEventListener('click', () => {
  if (!selected) return;
  send({ t: 'interrupt', id: String(++cmdSeq), agent: selected.agent, session: selected.session });
  setHint('Запрошено прерывание…');
});

boot();
