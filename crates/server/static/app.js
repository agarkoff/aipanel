'use strict';

const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};

// Иконка из путей SVG: текстовый символ вроде «⏻» есть не во всех шрифтах.
const SVG_NS = 'http://www.w3.org/2000/svg';
const icon = (paths) => {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('width', '15');
  svg.setAttribute('height', '15');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.5');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('aria-hidden', 'true');
  for (const d of paths) {
    const path = document.createElementNS(SVG_NS, 'path');
    path.setAttribute('d', d);
    svg.append(path);
  }
  return svg;
};

const STATUS = {
  busy: 'работает',
  idle: 'ждёт ввода',
  waiting: 'ждёт ответа',
  shell: 'выполняет команду',
  stalled: 'принято, нет ответа',
  stopped: 'не запущена',
  ready: 'не активна',
};
// Подсказки к состояниям, в которых окно выглядит зависшим, хотя сессия занята.
const STALLED_HINT =
  'opencode принял сообщение, но ещё не начал ответ. «Прервать» остановит ход и растолкает очередь.';
// Подсказки только про необычную доставку. Обычные случаи (открытый терминал,
// API opencode) очевидны по самой сессии и строку не занимают.
const DELIVERY = {
  window: 'Сессия не открыта: на компьютере откроется окно консоли с этой сессией, сообщение уйдёт в него первым запросом.',
  headless: 'Сессия не открыта: сообщение выполнит фоновый процесс claude -p --resume в каталоге сессии.',
  none: 'Отправка в эту сессию недоступна на этой платформе.',
};

let ws = null;
let agents = [];
let selected = null; // { agent, session }
let lastViewed = null; // последняя открытая сессия: остаётся подсвеченной в списке после выхода
let cmdSeq = 0;
const nodes = new Map(); // id элемента ленты -> DOM-узел

// Закреп в шапке ленты: показываем последнее своё сообщение, по нажатию переходим
// к нему, а закреп переключается на предыдущее своё — как в Telegram. При обычной
// прокрутке закреп сам следует за лентой.
let pinIdx = -1; // индекс своего сообщения в ленте, на который ведёт закреп
let pinNewestId = ''; // id самого нового своего сообщения при прошлой отрисовке
// Пока закреп везёт ленту к сообщению, прокрутка не должна переключать закреп:
// после нажатия он уже показывает следующее сообщение, как в Telegram.
let pinJumping = false;
let pinJumpTimer = null;
let flashId = ''; // id сообщения, к которому сейчас подсвечен переход
let flashTimer = null;
// Пока идёт переход по закрепу, авто-прилипание к концу ленты выключено:
// иначе первый же ответ агента вернул бы ленту вниз, не дав дочитать сообщение.
let scrollHoldUntil = 0;

// Ранние сообщения: агент присылает только последние, остальные — по кнопке в
// начале ленты. Здесь её состояние: запрос отправлен, ответа ещё нет; есть ли
// что грузить дальше; страховка на случай, если агент не ответил (старая версия
// не знает команду).
let olderPending = false;
let olderMore = true;
let olderTimer = null;
// Ссылку на кнопку держим сами: очистка ленты выбрасывает её из документа, и
// поиск по id после этого её уже не находит.
const olderBtn = $('older');

// Запуск новой сессии: id команды, её параметры и ожидаемая сессия.
let launchId = null;
let launchRequest = null;
let pendingLaunch = null;
// Начало ошибки агента «такого каталога нет» (`crates/agent/src/util.rs`,
// `DIR_MISSING`): по нему панель предлагает создать каталог и повторить запуск.
const DIR_MISSING = 'каталог не найден: ';
// Запуск, который ждёт ответа на вопрос о создании каталога.
let mkdirRequest = null;
// Модели для окна «Новая сессия»: список по «компьютер + инструмент».
const launchModelCache = new Map();
let launchModelsRequest = null; // { id, key } — ждём ответ для этой пары
// Последняя выбранная в окне запуска модель, отдельно для каждого инструмента.
const launchModelChoice = { claude: '', opencode: '' };
// Продолжение сессии в другом инструменте: id команды и её параметры.
let handoffId = null;
let handoffRequest = null;
// Сессия, для которой открыта панель под стрелкой: при переходе к другой панель закрывается.
let menuKey = null;
let copyIdTimer = null;
let cancelQueuedId = null;
// Удаление сессии: id команды и удаляемая сессия. Диалог остаётся открытым,
// пока агент не ответит, чтобы ошибку было видно.
let deleteId = null;
let deleteRequest = null;
// Закрытие консоли сессии без подтверждения: id команд, ожидающих ответа.
const closeIds = new Set();
// Прерывание хода: id команды, чтобы показать результат именно для неё.
let interruptId = null;
// Запросы opencode, ожидающие ответа: разрешения и формы с вариантами.
let promptSig = '';
let promptUid = 0;
const promptState = new Map(); // id запроса -> значения полей
const promptControls = new Map(); // id запроса -> Map(ключ поля -> элемент)
const promptCards = new Map(); // id запроса -> карточка в DOM
const promptSending = new Set(); // id запросов, ответ на которые отправляется
const promptReplyIds = new Map(); // id команды -> id запроса
// Каталоги, показанные в подсказках для текущего выбранного компьютера.
let launchDirCache = { agent: '', dirs: [] };

// Команды сессий (`/init`, `/clear`) для списка, который открывается по `/`:
// ключ — «агент\nсессия». Список присылает агент, он же выполняет команду.
const sessionCommands = new Map();
// Когда команды уже запрошены: чтобы не спрашивать агента на каждое нажатие.
const commandsAsked = new Map();
// Модели сессий opencode для окна выбора в `/models`.
const sessionModels = new Map();
const modelsAsked = new Map();
// Для какой сессии открыто окно выбора модели и что в нём показано.
let modelsSession = null; // { agent, session, current }
// Отправленная смена модели: id команды и подпись выбранной модели.
let modelSet = null;

// История запросов для стрелок вверх/вниз в поле ввода.
// Основной источник — лента сессии, поэтому в историю попадают и сообщения,
// отправленные из окна консоли или с другого устройства.
const feedRequests = new Map(); // id элемента ленты -> текст запроса
// Вызов инструмента, к которому относится результат: по нему видно, что за
// вывод (файл из Read, команда Bash) и как его подсветить.
const toolCalls = new Map(); // id вызова -> элемент ленты tool_use
const resultCalls = new Map(); // id результата -> элемент ленты tool_use
let pendingCalls = []; // вызовы Claude Code, результата которых ещё не было
// Отправленные из этого браузера, но ещё не пришедшие в ленту.
const sentRequests = new Map(); // "агент\nсессия" -> [текст, ...]
let histPos = 0; // 0 — черновик, 1 — последний запрос, 2 — предыдущий…
let histDraft = ''; // что было в поле до начала просмотра истории

// ------------------------------------------------------------ вход

async function boot() {
  const res = await fetch('/api/me');
  if (!res.ok) {
    $('login').hidden = false;
    return;
  }
  const me = await res.json().catch(() => ({}));
  if (me.user) {
    $('user').textContent = me.version ? `${me.user} · v${me.version}` : me.user;
    $('user').title = me.version ? `Версия панели ${me.version}` : '';
    $('user').hidden = false;
  }
  start();
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
    boot();
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
    // После переподключения агент мог перезапуститься: списки команд берём заново.
    sessionCommands.clear();
    commandsAsked.clear();
    sessionModels.clear();
    modelsAsked.clear();
    launchModelCache.clear();
    launchModelsRequest = null;
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
    noteSessions(agents);
    renderSessions();
    renderHeader();
    // Сессию могли удалить в другой вкладке: её ленты больше нет.
    const open = selected && agents.find((a) => a.name === selected.agent);
    if (open && !open.sessions.some((s) => s.key === selected.session)) closeView();
    checkPendingLaunch();
  } else if (msg.t === 'history') {
    if (!selected || msg.agent !== selected.agent || msg.session !== selected.session) return;
    applyHistory(msg);
  } else if (msg.t === 'older') {
    if (!selected || msg.agent !== selected.agent || msg.session !== selected.session) return;
    applyOlder(msg);
  } else if (msg.t === 'prompts') {
    if (!selected || msg.agent !== selected.agent || msg.session !== selected.session) return;
    applyPrompts(msg.prompts || []);
  } else if (msg.t === 'commands') {
    const key = commandKey(msg.agent, msg.session);
    commandsAsked.delete(key);
    sessionCommands.set(key, Array.isArray(msg.commands) ? msg.commands : []);
    // Список уже открыт на «/»: подставляем пришедшие команды на место заглушки.
    if (selected && msg.agent === selected.agent && msg.session === selected.session) slash.refresh();
  } else if (msg.t === 'models') {
    const key = commandKey(msg.agent, msg.session);
    modelsAsked.delete(key);
    sessionModels.set(key, Array.isArray(msg.models) ? msg.models : []);
    // Окно выбора уже открыто и ждёт список.
    if (modelsSession && msg.agent === modelsSession.agent && msg.session === modelsSession.session) {
      renderModels(modelsSession);
    }
  } else if (msg.t === 'tool_models') {
    const req = launchModelsRequest;
    if (!req || msg.id !== req.id) return;
    launchModelsRequest = null;
    launchModelCache.set(req.key, Array.isArray(msg.models) ? msg.models : []);
    // Окно запуска открыто на этой же паре компьютер + инструмент.
    if ($('launch').open && launchModelKey() === req.key) launchModelSuggest.refresh();
  } else if (msg.t === 'result') {
    if (launchId && msg.id === launchId) onLaunchResult(msg);
    else if (handoffId && msg.id === handoffId) onHandoffResult(msg);
    else if (cancelQueuedId && msg.id === cancelQueuedId) onCancelQueuedResult(msg);
    else if (deleteId && msg.id === deleteId) onDeleteResult(msg);
    else if (closeIds.has(msg.id)) onCloseResult(msg);
    else if (interruptId && msg.id === interruptId) onInterruptResult(msg);
    else if (modelSet && msg.id === modelSet.id) onModelResult(msg);
    else if (promptReplyIds.has(msg.id)) onPromptResult(msg);
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
  if (s.status === 'busy' || s.status === 'shell' || s.status === 'stalled') return 'dot busy';
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
      const row = el('div', 'session-row');
      const btn = el('button', 'session');
      btn.type = 'button';
      // Подсвечивается открытая сессия, а после выхода из неё — та, где только что были.
      const mark = selected || lastViewed;
      if (mark && mark.agent === a.name && mark.session === s.key) btn.classList.add('active');
      btn.append(
        el('span', dotClass(s, a.online)),
        el('span', 'name', s.title || s.key),
        el('span', 'when', ago(s.updated_at)),
        el('span', 'meta', `${s.tool} · ${s.cwd}`),
      );
      btn.addEventListener('click', () => select(a.name, s.key));

      const action = el('span', 'swipe-action');
      action.setAttribute('aria-hidden', 'true');

      // На компьютере консоль закрывает кнопка рядом с крестиком, на телефоне — свайп.
      // У закрытой сессии закрывать нечего: кнопки нет, а свайп сразу ведёт к удалению.
      const stop = el('button', 'session-close');
      stop.type = 'button';
      stop.title = 'Закрыть консоль';
      stop.setAttribute('aria-label', 'Закрыть консоль');
      stop.append(icon(['M12.6 4.2a6 6 0 1 1-9.2 0', 'M8 1.5v6']));
      stop.addEventListener('click', (e) => {
        e.stopPropagation();
        askClose(a.name, s);
      });

      // На компьютере сессия удаляется крестиком, на телефоне — смахиванием влево.
      const del = el('button', 'session-delete', '×');
      del.type = 'button';
      del.title = 'Удалить сессию';
      del.setAttribute('aria-label', 'Удалить сессию');
      del.addEventListener('click', (e) => {
        e.stopPropagation();
        askDelete(a.name, s);
      });

      row.append(action, btn, del);
      if (s.live) row.append(stop);
      attachSwipe(row, btn, action, {
        closable: s.live,
        onClose: () => askClose(a.name, s),
        onDelete: () => askDelete(a.name, s),
      });
      box.append(row);
    }
  }
}

$('filter').addEventListener('input', renderSessions);

// Смахивание влево по строке сессии — жест телефона. Строка едет за пальцем
// и показывает, что будет при отпускании: до порога — синяя «Закрыть»
// (консоль сессии закрывается сразу, без подтверждения), дальше — красная
// «Удалить» с окном подтверждения. У закрытой сессии ступень одна: сразу удаление.
function attachSwipe(row, btn, action, actions) {
  const CLOSE_AT = 88;
  const DELETE_AT = 168;
  // Закрывать нечего — порог удаления как у единственной ступени.
  const delAt = actions.closable ? DELETE_AT : CLOSE_AT;
  const LIMIT = delAt + 16;
  let startX = 0;
  let startY = 0;
  let dx = 0;
  let dy = 0;
  let tracking = false;
  let horizontal = false;
  let swiped = false;
  const showAction = (dist) => {
    const del = !actions.closable || dist >= DELETE_AT;
    action.textContent = del ? 'Удалить' : 'Закрыть';
    action.className = del ? 'swipe-action delete' : 'swipe-action close';
  };
  showAction(0);
  row.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' || e.button !== 0) return;
    startX = e.clientX;
    startY = e.clientY;
    dx = 0;
    dy = 0;
    tracking = true;
    horizontal = false;
    swiped = false;
    showAction(0);
    row.classList.add('dragging');
  });
  row.addEventListener('pointermove', (e) => {
    if (!tracking) return;
    dx = e.clientX - startX;
    dy = e.clientY - startY;
    if (!horizontal) {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      // Вертикальное движение — это прокрутка списка, а не свайп.
      if (Math.abs(dx) <= Math.abs(dy) * 1.2) {
        tracking = false;
        row.classList.remove('dragging');
        return;
      }
      horizontal = true;
      row.setPointerCapture(e.pointerId);
    }
    const shift = Math.max(-LIMIT, Math.min(0, dx));
    btn.style.transform = `translateX(${shift}px)`;
    showAction(-shift);
    e.preventDefault();
  });
  const end = () => {
    if (!tracking) return;
    tracking = false;
    row.classList.remove('dragging');
    btn.style.transform = '';
    showAction(0);
    const dist = Math.min(LIMIT, Math.max(0, -dx));
    if (horizontal && dist > 8) {
      swiped = true;
      // Клик после свайпа не должен открыть сессию.
      setTimeout(() => {
        swiped = false;
      }, 400);
    }
    if (horizontal && dist >= delAt) {
      // Открываем окно после возможного click: иначе касание, завершившее
      // свайп, попало бы в затемнение и сразу закрыло подтверждение.
      setTimeout(actions.onDelete, 0);
    } else if (horizontal && actions.closable && dist >= CLOSE_AT) {
      actions.onClose();
    }
    horizontal = false;
  };
  row.addEventListener('pointerup', end);
  row.addEventListener('pointercancel', () => {
    tracking = false;
    horizontal = false;
    row.classList.remove('dragging');
    btn.style.transform = '';
    showAction(0);
  });
  row.addEventListener(
    'click',
    (e) => {
      if (!swiped) return;
      e.preventDefault();
      e.stopPropagation();
    },
    true,
  );
}

// ------------------------------------------------------------ закрытие консоли

// Закрывает окно консоли сессии на компьютере (или её фоновый процесс),
// не удаляя переписку. Подтверждения нет: жест уже показал, что произойдёт.
function askClose(agent, session) {
  const id = String(++cmdSeq);
  if (!send({ t: 'close', id, agent, session: session.key })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  closeIds.add(id);
  setHint(`Закрываем консоль «${session.title || session.key}»…`);
}

function onCloseResult(msg) {
  closeIds.delete(msg.id);
  setHint(msg.ok ? 'Консоль закрыта.' : `Ошибка: ${msg.error || 'неизвестная'}`, !msg.ok);
}

// ------------------------------------------------------------ удаление сессии

function deleteHint(text, isError) {
  const hint = $('delete-hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
}

function askDelete(agent, session) {
  if (deleteId !== null) return;
  deleteRequest = { agent, session: session.key, title: session.title || session.key };
  $('delete-text').textContent =
    `Сессия «${deleteRequest.title}» будет удалена с компьютера без возможности восстановления.`;
  deleteHint('');
  // Галочка включена при каждом открытии: открытую сессию иначе не удалить.
  $('delete-close').checked = true;
  $('delete-close').disabled = false;
  $('delete-ok').disabled = false;
  $('delete-cancel').disabled = false;
  if (!$('delete-session').open) $('delete-session').showModal();
}

function closeDeleteDialog() {
  if (deleteId !== null) return;
  deleteRequest = null;
  $('delete-session').close();
}

$('delete-form').addEventListener('submit', (e) => {
  e.preventDefault();
  if (!deleteRequest || deleteId !== null) return;
  const id = String(++cmdSeq);
  const close = $('delete-close').checked;
  if (!send({ t: 'delete', id, agent: deleteRequest.agent, session: deleteRequest.session, close })) {
    deleteHint('Нет соединения с сервером.', true);
    return;
  }
  deleteId = id;
  $('delete-close').disabled = true;
  $('delete-ok').disabled = true;
  $('delete-cancel').disabled = true;
  deleteHint('Удаляем…');
});

$('delete-cancel').addEventListener('click', closeDeleteDialog);
// Пока команда выполняется, окно не закрываем: ошибку должно быть видно.
$('delete-session').addEventListener('cancel', (e) => {
  if (deleteId !== null) e.preventDefault();
  else deleteRequest = null;
});
$('delete-session').addEventListener('click', (e) => {
  if (e.target === e.currentTarget) closeDeleteDialog();
});

function onDeleteResult(msg) {
  deleteId = null;
  $('delete-close').disabled = false;
  $('delete-ok').disabled = false;
  $('delete-cancel').disabled = false;
  if (!msg.ok) {
    deleteHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  const req = deleteRequest;
  deleteRequest = null;
  $('delete-session').close();
  if (!req) return;
  // Убираем строку сразу, не дожидаясь следующего списка от агента.
  const agent = agents.find((a) => a.name === req.agent);
  if (agent) agent.sessions = agent.sessions.filter((s) => s.key !== req.session);
  if (selected && selected.agent === req.agent && selected.session === req.session) closeView();
  else renderSessions();
  setHint('Сессия удалена.');
}

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
  lastViewed = selected;
  nodes.clear();
  feedRequests.clear();
  clearPrompts();
  resetHistoryNav();
  clearTimeout(copyIdTimer);
  $('copy-id').classList.remove('copied');
  $('sm-id').classList.remove('copied');
  $('feed').replaceChildren(el('div', 'empty', 'Загрузка ленты…'));
  resetPin();
  resetOlder();
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
  clearPrompts();
  resetPin();
  resetOlder();
  send({ t: 'unwatch' });
  // Если сессию закрыли не кнопкой «Назад», текущая запись истории всё ещё
  // помечена как просмотр: без этого следующая сессия не добавила бы новую,
  // и системная кнопка «Назад» увела бы со страницы.
  if (history.state && history.state.viewing) history.replaceState(null, '');
  // Подсказка относилась к открытой сессии; в списке её копия была бы уже не к месту.
  setHint('');
  renderSessions();
  renderHeader();
  // На телефоне список был скрыт; прокручиваем к сессии, из которой вышли.
  const mark = document.querySelector('#sessions .session.active');
  if (mark) mark.scrollIntoView({ block: 'nearest' });
}

$('back').addEventListener('click', () => {
  if (history.state && history.state.viewing) history.back();
  else closeView();
});
window.addEventListener('popstate', () => {
  if (selected) closeView();
});

// ------------------------------------------------------------ уведомления

// Панель обычно открыта в фоне, а ход идёт минутами. Браузерное уведомление
// сообщает о конце хода и о сессии, которая ждёт выбора, чтобы ответ не
// прождали впустую. Нажатие на уведомление открывает эту сессию.
const BUSY_STATUSES = new Set(['busy', 'shell', 'stalled']);
// Тот же признак, что у зелёного кружка в списке: ход кончился, сессия свободна.
const DONE_STATUSES = new Set(['idle', 'ready']);
const notifiedAsk = 'aipanel.notify-asked';
const sessionStatuses = new Map(); // "агент\nсессия" -> прошлый статус
let statusesSeeded = false; // первый список — точка отсчёта, по нему не уведомляем

function notifySupported() {
  return typeof Notification !== 'undefined';
}

// Браузеры спрашивают разрешение только по действию пользователя, поэтому
// первым нажатием на страницу и пользуемся. Отказ не переспрашиваем.
function askNotify() {
  if (!notifySupported() || Notification.permission !== 'default') return;
  if (localStorage.getItem(notifiedAsk)) return;
  localStorage.setItem(notifiedAsk, '1');
  // В старых браузерах requestPermission возвращает не промис, а принимает callback.
  const asked = Notification.requestPermission();
  if (asked && asked.catch) asked.catch(() => {});
}
document.addEventListener('pointerdown', askNotify, { once: true });

function showNotice(agent, session, kind) {
  if (!notifySupported() || Notification.permission !== 'granted') return;
  // Пользователь смотрит именно эту сессию — уведомление ему не нужно.
  const open = selected && selected.agent === agent.name && selected.session === session.key;
  if (open && document.visibilityState === 'visible') return;
  let n;
  try {
    n = new Notification(session.title || session.key, {
      body: kind === 'waiting' ? `${agent.name} · сессия ждёт ответа` : `${agent.name} · ход завершён`,
      // Одно уведомление на сессию: новое заменяет прежнее, а не копится стопкой.
      tag: `aipanel:${agent.name}:${session.key}`,
      icon: '/icon-192.png',
    });
  } catch {
    // На телефоне уведомление строит только service worker: без него молчим.
    return;
  }
  n.onclick = () => {
    window.focus();
    select(agent.name, session.key);
    n.close();
  };
}

// Сравнивает статусы сессий с прошлым списком и уведомляет о двух событиях:
// сессия закончила ход или просит выбор. Остальные переходы (запуск, закрытие,
// «принято, нет ответа») — не повод звать пользователя.
function noteSessions(list) {
  const prev = new Map(sessionStatuses);
  sessionStatuses.clear();
  for (const a of list) {
    if (!a.online) continue;
    for (const s of a.sessions) {
      const key = `${a.name}\n${s.key}`;
      sessionStatuses.set(key, s.status);
      const was = prev.get(key);
      if (!statusesSeeded || was === undefined || was === s.status) continue;
      if (s.status === 'waiting') showNotice(a, s, 'waiting');
      else if (BUSY_STATUSES.has(was) && DONE_STATUSES.has(s.status)) showNotice(a, s, 'done');
    }
  }
  statusesSeeded = true;
}

// ------------------------------------------------------------ шапка и лента

// Строка хода: сколько идёт запрос и сколько токенов записал транскрипт.
// Только записанное агентом: до первого ответа токенов нет, и строка их не показывает.
function fmtTokens(n) {
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(n < 10000 ? 2 : 1).replace(/\.?0+$/, '')}k`;
}

function fmtElapsed(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 60 ? `${s}с` : `${Math.floor(s / 60)}м ${String(s % 60).padStart(2, '0')}с`;
}

function renderTurn() {
  const box = $('turn');
  const cur = current();
  const turn = cur && cur.agent.online && BUSY_STATUSES.has(cur.session.status) ? cur.session.turn : null;
  if (!turn) {
    box.hidden = true;
    return;
  }
  const parts = [];
  const mark = el('span', 'turn-mark', '✻');
  parts.push(el('span', '', fmtElapsed(Date.now() - turn.started_at)));
  if (turn.input) parts.push(el('span', '', `↑ ${fmtTokens(turn.input)}`));
  if (turn.output) parts.push(el('span', '', `↓ ${fmtTokens(turn.output)}`));
  box.replaceChildren(mark, ...parts);
  box.title = 'Время с запроса и токены, записанные в транскрипт: ↑ контекст последнего ответа, ↓ выход за ход';
  box.hidden = false;
}
setInterval(renderTurn, 1000);

function renderHeader() {
  renderTurn();
  const cur = current();
  const badge = $('status');
  if (!cur) {
    $('title').textContent = selected ? selected.session : 'Выберите сессию';
    $('subtitle').textContent = '';
    $('session-id').hidden = true;
    $('copy-id').hidden = true;
    badge.hidden = true;
    $('handoff').hidden = true;
    $('interrupt').hidden = true;
    $('composer').hidden = true;
    $('more').hidden = true;
    setSessionMenu(false);
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
  const busy = agent.online && ['busy', 'shell', 'stalled'].includes(session.status);
  $('interrupt').hidden = !busy;
  const target = session.tool === 'claude' ? 'opencode' : session.tool === 'opencode' ? 'claude' : null;
  const targetName = target === 'claude' ? 'Claude Code' : 'opencode';
  $('handoff').hidden = !agent.online || !target;
  if (target) {
    $('handoff').querySelector('.label').textContent = `В ${targetName}`;
    $('handoff').setAttribute('aria-label', `Продолжить в ${targetName}`);
    $('handoff').title =
      `Продолжить в ${targetName}: в этом каталоге откроется новая сессия ` +
      'и получит выжимку из переписки этой сессии';
  }
  $('handoff').disabled = handoffId !== null;
  // Панель под стрелкой на телефоне: модель и та же кнопка смены агента.
  if (menuKey !== session.key) setSessionMenu(false);
  menuKey = session.key;
  $('more').hidden = false;
  $('sm-model').textContent = session.model || 'неизвестна';
  renderLimits(agent, session);
  $('sm-id-text').textContent = sessionId(session.key);
  $('sm-handoff').hidden = $('handoff').hidden;
  $('sm-handoff').textContent = target ? `Продолжить в ${targetName}` : '';
  $('sm-handoff').title = $('handoff').title;
  $('sm-handoff').disabled = $('handoff').disabled;
  const canSend = agent.online && session.delivery !== 'none';
  $('composer').hidden = false;
  $('text').disabled = !canSend;
  $('composer').querySelector('button').disabled = !canSend;
  if (!$('hint').dataset.sticky) {
    $('hint').className = 'hint';
    $('hint').textContent = !agent.online
      ? ''
      : session.status === 'stalled'
        ? STALLED_HINT
        : DELIVERY[session.delivery] || '';
  }
}

// Время сброса лимита: сегодня — только часы, иначе ещё и дата.
function resetText(ms) {
  if (!ms) return '';
  const d = new Date(ms);
  const now = new Date();
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  if (d.toDateString() === now.toDateString()) return hm;
  return `${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')} ${hm}`;
}

// Остаток лимитов подписки на машине агента. Показываем только тот инструмент,
// в котором идёт эта сессия: лимит второго в ленте не нужен.
// Проценты — сколько ещё доступно, а не сколько израсходовано.
function renderLimits(agent, session) {
  const box = $('sm-limits');
  box.replaceChildren();
  // Лимит OpenCode Go относится только к моделям этой подписки: сессия opencode
  // на zai, openai и т.п. его не тратит, поэтому строку скрываем целиком.
  const relevant = session.tool !== 'opencode' || (session.model || '').startsWith('opencode-go/');
  $('sm-limits-row').hidden = !relevant;
  if (!relevant) return;
  const all = agent && agent.limits ? agent.limits : [];
  const limits = all.filter((l) => l.tool === session.tool);
  if (!limits.length) {
    box.append(el('span', 'sm-limits-empty', agent && agent.online ? 'нет данных' : 'агент не в сети'));
    return;
  }
  for (const limit of limits) {
    const row = el('div', 'sm-limit');
    const meters = limit.meters || [];
    if (meters.some((m) => m.left === 0)) row.classList.add('out');
    const parts = [];
    const details = [];
    for (const m of meters) {
      parts.push(`${m.label} ${m.left}%`);
      const reset = resetText(m.resets_at);
      details.push(`${m.label}: осталось ${m.left}%${reset ? `, сброс ${reset}` : ''}`);
    }
    row.append(el('span', 'sm-limit-tool', limit.title || limit.tool));
    row.append(el('span', 'sm-limit-meters', parts.join(' · ')));
    // На телефоне подсказка по наведению недоступна, поэтому ближайший сброс
    // к тому же показан текстом.
    const nearest = meters.filter((m) => m.resets_at).sort((a, b) => a.resets_at - b.resets_at)[0];
    if (nearest) row.append(el('span', 'sm-limit-reset', `сброс ${resetText(nearest.resets_at)}`));
    row.title = `${limit.title || limit.tool}: ${details.join(' · ')}`;
    box.append(row);
  }
}

function setHint(text, isError) {
  const hint = $('hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
  // На телефоне лента (а с ней и #hint) скрыта, пока не выбрана сессия, поэтому
  // сообщение о запуске новой сессии дублируем в список — иначе на экране списка
  // не видно ни «Запускаем…», ни ошибки.
  const listHint = $('hint-list');
  listHint.textContent = text;
  listHint.className = isError ? 'hint hint-list error' : 'hint hint-list';
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

// Кнопка в шапке (компьютер) и строка в панели под стрелкой (телефон) копируют одно и то же.
async function copySessionId(btn) {
  const cur = current();
  if (!cur) return;
  const ok = await copyToClipboard(sessionId(cur.session.key));
  btn.classList.toggle('copied', ok);
  clearTimeout(copyIdTimer);
  if (ok) copyIdTimer = setTimeout(() => btn.classList.remove('copied'), 1500);
  setHint(ok ? 'Идентификатор скопирован.' : 'Не удалось скопировать идентификатор.', !ok);
}
$('copy-id').addEventListener('click', () => copySessionId($('copy-id')));
$('sm-id').addEventListener('click', () => copySessionId($('sm-id')));

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

function cancelQueued(item) {
  const cur = current();
  if (!cur || !item.id.startsWith('queue:')) return;
  const id = String(++cmdSeq);
  if (!send({ t: 'cancel_queued', id, agent: cur.agent.name, session: cur.session.key, queue: item.id.slice(6) })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  cancelQueuedId = id;
  setHint('Убираем сообщение из очереди…');
}

function onCancelQueuedResult(msg) {
  cancelQueuedId = null;
  setHint(msg.ok ? 'Сообщение убрано из очереди.' : `Ошибка: ${msg.error || 'неизвестная'}`, !msg.ok);
}

function firstLine(text) {
  const line = text.split('\n', 1)[0];
  return line.length > 140 ? `${line.slice(0, 140)}…` : line;
}

// Строки диффа: заголовки файлов и хунков — отдельными классами, изменения —
// зелёным и красным. Классы задаются по первому символу строки.
function diffLineClass(line) {
  if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('Index:') || line.startsWith('===')) {
    return 'meta';
  }
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return '';
}

function diffInfo(text) {
  let file = '';
  let add = 0;
  let del = 0;
  for (const line of text.split('\n')) {
    if (!file && line.startsWith('+++ ')) file = line.slice(4).replace(/^b\//, '').trim();
    else if (line.startsWith('+') && !line.startsWith('+++')) add++;
    else if (line.startsWith('-') && !line.startsWith('---')) del++;
  }
  return { file: file || 'файл', add, del };
}

// Правка файла: раскрыта сразу, чтобы изменения были видны без клика.
function diffDetails(text) {
  const details = el('details', 'diff');
  details.open = true;
  const info = diffInfo(text);
  const summary = el('summary');
  summary.append(
    el('span', 'diff-name', `Изменения: ${info.file}`),
    el('span', 'diff-stat', `+${info.add} −${info.del}`),
  );
  if (info.file !== 'файл') summary.title = info.file;
  const body = el('div', 'diff-body');
  const lines = text.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  // Содержимое строк подсвечивается по языку файла; знак +/−/пробел — как был.
  const code = Highlight.diffLines(lines, info.file);
  lines.forEach((line, k) => {
    const cls = diffLineClass(line);
    const row = el('div', cls ? `diff-line ${cls}` : 'diff-line');
    if (code[k]) row.append(line.slice(0, 1), code[k]);
    else row.textContent = line;
    body.append(row);
  });
  details.append(summary, body);
  return details;
}

// Пути к картинкам в ленте раскрываются через сервер: файл лежит на машине агента.
function mdOptions() {
  const cur = selected;
  if (!cur) return {};
  return {
    localImage: (path) =>
      `/api/image?${new URLSearchParams({ agent: cur.agent, session: cur.session, path })}`,
  };
}

// Подсветка строится при первом раскрытии: результаты свёрнуты, и в длинной
// ленте большинство из них так и не откроют. До этого в узле — тот же текст,
// так что поиск по странице его находит.
function highlightOnOpen(details, body, build) {
  let done = false;
  const run = () => {
    if (done || !details.open) return;
    done = true;
    let view = null;
    try {
      view = build();
    } catch (e) {
      console.error('подсветка:', e);
    }
    if (view) {
      body.replaceChildren(view);
      body.classList.add('hl');
    }
  };
  details.addEventListener('toggle', run);
  run();
}

// Вызов инструмента для результата. У opencode id результата — id вызова
// с `:r`. У Claude Code вызов и результат — разные строки транскрипта без
// общего id в ленте, но результаты идут в порядке вызовов.
function callFor(item, isNew) {
  if (resultCalls.has(item.id)) return resultCalls.get(item.id);
  let call = null;
  if (item.id.endsWith(':r') && toolCalls.has(item.id.slice(0, -2))) {
    call = toolCalls.get(item.id.slice(0, -2));
    pendingCalls = pendingCalls.filter((c) => c !== call);
  } else if (isNew) {
    call = pendingCalls.shift() || null;
  }
  resultCalls.set(item.id, call);
  return call;
}

function trackCall(item, isNew) {
  if (item.kind === 'tool_use') {
    toolCalls.set(item.id, item);
    if (isNew) pendingCalls.push(item);
  } else if (item.kind === 'text' && item.role === 'assistant' && isNew) {
    // Ответ модели текстом — результаты вызовов уже пришли: незакрытые
    // вызовы не должны забрать чужой результат. Своё сообщение из очереди
    // может прийти посреди работы инструмента, поэтому оно очередь не трогает.
    pendingCalls = [];
  }
}

function buildItem(item, call) {
  const node = el('div', `item ${item.role} ${item.kind}`);
  node.dataset.id = item.id;
  if (item.kind === 'text') {
    const body = el('div', 'body md');
    body.append(renderMarkdown(item.text, mdOptions()));
    node.append(body);
  } else if (item.kind === 'error') {
    node.append(el('div', 'body', item.text));
  } else if (item.kind === 'queued') {
    const label = el('div', 'queue-label');
    label.append(el('span', '', 'В очереди — ждёт завершения текущего ответа'));
    const cancel = el('button', 'queue-cancel', '×');
    cancel.type = 'button';
    cancel.title = 'Убрать из очереди';
    cancel.setAttribute('aria-label', 'Убрать сообщение из очереди');
    cancel.addEventListener('click', () => cancelQueued(item));
    label.append(cancel);
    const body = el('div', 'body md');
    body.append(renderMarkdown(item.text, mdOptions()));
    node.append(label, body);
  } else if (item.kind === 'diff') {
    node.append(diffDetails(item.text));
  } else {
    const details = el('details');
    const label =
      item.kind === 'thinking' ? 'Размышление'
      : item.kind === 'tool_use' ? `${item.tool || 'инструмент'}: ${firstLine(item.text)}`
      : `Результат: ${firstLine(item.text)}`;
    const body = el('div', 'body', item.text);
    details.append(el('summary', '', label), body);
    // Размышления раскрыты по умолчанию; вызовы инструментов и результаты свёрнуты.
    if (item.kind === 'thinking') details.open = true;
    if (item.kind === 'tool_use') {
      highlightOnOpen(details, body, () => Highlight.toolInput(item.text, item.tool));
    } else if (item.kind === 'tool_result') {
      highlightOnOpen(details, body, () => Highlight.toolResult(item.text, call && call.tool, call && call.text));
    }
    node.append(details);
    // Картинки из аргументов и вывода инструмента — под его строкой, их видно
    // и в свёрнутом виде.
    if (item.kind === 'tool_use' || item.kind === 'tool_result') {
      const images = renderMarkdown.imageLinks(item.text, mdOptions());
      if (images) node.append(images);
    }
  }
  if (item.ts && (item.kind === 'text' || item.kind === 'error')) {
    node.append(el('div', 'time', new Date(item.ts).toLocaleString()));
  }
  return node;
}

// Один элемент ленты. `target` — куда его поставить: сама лента для обычных
// обновлений или фрагмент перед ней, когда догружаются ранние сообщения.
function placeItem(item, target) {
  const old = nodes.get(item.id);
  trackCall(item, !old);
  const node = buildItem(item, item.kind === 'tool_result' ? callFor(item, !old) : null);
  if (item.id === flashId) node.classList.add('flash');
  if (old) {
    const oldDetails = old.querySelector('details');
    const newDetails = node.querySelector('details');
    if (oldDetails && newDetails) newDetails.open = oldDetails.open;
    renderMarkdown.keepOpenImages(old, node);
    old.replaceWith(node);
  } else {
    target.append(node);
  }
  nodes.set(item.id, node);
}

// Своё сообщение из ленты — оно же история для стрелок в поле ввода.
// `true`, если элемент — запрос из ленты.
function rememberRequest(item) {
  if (item.role !== 'user' || (item.kind !== 'text' && item.kind !== 'queued')) return false;
  feedRequests.set(item.id, item.text);
  return true;
}

function applyHistory(msg) {
  const feed = $('feed');
  const stick = Date.now() > scrollHoldUntil && feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
  if (msg.reset) {
    nodes.clear();
    feedRequests.clear();
    toolCalls.clear();
    resultCalls.clear();
    pendingCalls = [];
    // Агент сказал, есть ли перед этой лентой более ранние сообщения: если их
    // нет, кнопки догрузки не будет вовсе.
    olderMore = msg.more === true;
    feed.replaceChildren();
    if (!msg.items.length) feed.append(el('div', 'empty', 'Лента пуста.'));
  } else if (msg.items.length && !nodes.size) {
    feed.replaceChildren();
  }
  for (const id of msg.drop || []) {
    const node = nodes.get(id);
    if (node) {
      node.remove();
      nodes.delete(id);
    }
    feedRequests.delete(id);
    toolCalls.delete(id);
    resultCalls.delete(id);
  }
  for (const item of msg.items) {
    // Отправленное из панели уже в ленте: очередь больше не нужна.
    if (rememberRequest(item)) consumeSentRequest(item.text);
    placeItem(item, feed);
  }
  // Кнопка догрузки — всегда первая строка ленты; очистка выше её выбросила.
  mountOlder();
  updateOlder();
  // Закреп показываем до прокрутки в конец: полоса закрепа отнимает у ленты
  // высоту, и прокрутка, сделанная раньше, оставила бы низ последнего сообщения
  // скрытым — без единого следующего обновления, которое это исправило бы.
  syncPin();
  if (msg.reset || stick) feed.scrollTop = feed.scrollHeight;
  updateToEnd();
}

// ------------------------------------------------------------ ранние сообщения

// Кнопка догрузки живёт первой строкой ленты: очистка ленты выбрасывает её из
// DOM, поэтому возвращаем на место перед каждым новым элементом.
function mountOlder() {
  const feed = $('feed');
  if (feed.firstChild !== olderBtn) feed.prepend(olderBtn);
}

function updateOlder() {
  olderBtn.hidden = !olderMore || !nodes.size;
  olderBtn.disabled = olderPending;
  olderBtn.textContent = olderPending ? 'Загрузка…' : 'Загрузить ранние сообщения';
}

function resetOlder() {
  clearTimeout(olderTimer);
  olderPending = false;
  // Пока агент не сказал, что перед лентой что-то есть, кнопки не показываем.
  olderMore = false;
  mountOlder();
  updateOlder();
}

function askOlder() {
  if (!selected || olderPending || !olderMore || !nodes.size) return;
  if (!send({ t: 'older', agent: selected.agent, session: selected.session })) return;
  olderPending = true;
  updateOlder();
  // Старая версия агента команду не знает и ответа не пришлёт: не оставляем
  // кнопку навсегда в «Загрузке».
  clearTimeout(olderTimer);
  olderTimer = setTimeout(() => {
    olderPending = false;
    updateOlder();
    setHint('Ранние сообщения не загрузились: агент не ответил.', true);
  }, 10_000);
}

// Прокрутка до верхней кромки сама просит более ранние сообщения: доводить до
// них отдельным нажатием кнопки не нужно. Кнопка остаётся для тех случаев,
// когда догрузку не вышло запустить прокруткой.
function askOlderAtTop() {
  if (!olderMore || olderPending || !nodes.size) return;
  if ($('feed').scrollTop > 0) return;
  askOlder();
}

olderBtn.addEventListener('click', askOlder);

// Ранние сообщения пришли: ставим их в начало ленты, не сдвигая то место,
// которое человек читает.
function applyOlder(msg) {
  clearTimeout(olderTimer);
  olderPending = false;
  if (msg.error) {
    updateOlder();
    setHint(`Ранние сообщения не загрузились: ${msg.error}`, true);
    return;
  }
  olderMore = msg.more !== false;
  mountOlder();
  const feed = $('feed');
  const pinned = myMessages()[pinIdx] || null;
  // Держимся за первый элемент ленты: после вставки выше он же должен остаться
  // на том же месте экрана. Браузер сам подтягивает прокрутку (scroll anchoring),
  // но не везде и не всегда — остаток доводим вручную.
  const anchor = olderBtn.nextElementSibling;
  const anchorTop = anchor ? anchor.getBoundingClientRect().top : 0;
  const frag = document.createDocumentFragment();
  // Результаты вызовов сопоставляются между собой внутри старой части ленты:
  // очередь вызовов новой части им не достаётся.
  const savedCalls = pendingCalls;
  pendingCalls = [];
  for (const item of msg.items || []) {
    // Такое сообщение уже показано: страницы не пересекаются, но агент может
    // ответить повторно на второй запрос.
    if (nodes.has(item.id)) continue;
    rememberRequest(item);
    placeItem(item, frag);
  }
  pendingCalls = savedCalls;
  olderBtn.after(frag);
  if (anchor) {
    const shift = anchor.getBoundingClientRect().top - anchorTop;
    if (shift) feed.scrollTop += shift;
  }
  // Закреп остаётся на том же сообщении, но его номер среди своих сдвинулся.
  const list = myMessages();
  pinIdx = pinned ? list.indexOf(pinned) : list.length - 1;
  updateOlder();
  syncPin();
  updateToEnd();
}

// ------------------------------------------------------------ закреп и конец ленты

// Свои сообщения в ленте по порядку: отправленные и ещё стоящие в очереди.
// Порядок берём из DOM: ранние сообщения вставляются в начало ленты, а не в
// конец, и порядок вставки в `nodes` этому уже не соответствует.
function myMessages() {
  return [...$('feed').querySelectorAll('.item.user')];
}

function messageText(node) {
  const body = node.querySelector('.body');
  return ((body ? body.textContent : node.textContent) || '').replace(/\s+/g, ' ').trim();
}

function renderPin(list) {
  $('pin-preview').textContent = messageText(list[pinIdx]) || 'Сообщение';
  const count = $('pin-count');
  // Счётчик нужен, только когда закрепов несколько: по нему видно, где мы.
  count.hidden = list.length < 2;
  count.textContent = `${pinIdx + 1} / ${list.length}`;
}

// Лента изменилась: новое своё сообщение снова становится закрепом, а если
// закреплённое пропало (сообщение ушло из очереди), индекс подтягиваем к концу.
function syncPin() {
  const list = myMessages();
  if (!list.length) {
    resetPin();
    return;
  }
  const newest = list[list.length - 1].dataset.id || '';
  if (newest !== pinNewestId) {
    pinNewestId = newest;
    pinIdx = list.length - 1;
  }
  if (pinIdx < 0 || pinIdx >= list.length) pinIdx = list.length - 1;
  $('pin').hidden = false;
  renderPin(list);
}

function resetPin() {
  clearFlash();
  clearTimeout(pinJumpTimer);
  pinJumping = false;
  scrollHoldUntil = 0;
  $('pin').hidden = true;
  $('pin-preview').textContent = '';
  $('pin-count').hidden = true;
  $('to-end').hidden = true;
  pinIdx = -1;
  pinNewestId = '';
}

function clearFlash() {
  clearTimeout(flashTimer);
  flashId = '';
  for (const node of nodes.values()) node.classList.remove('flash');
}

$('pin').addEventListener('click', () => {
  const list = myMessages();
  if (!list.length) return;
  if (pinIdx < 0 || pinIdx >= list.length) pinIdx = list.length - 1;
  const node = list[pinIdx];
  const feed = $('feed');
  const box = feed.getBoundingClientRect();
  const item = node.getBoundingClientRect();
  // Сообщение ставим ближе к верху ленты: так видно и сам запрос, и ответ на него.
  const top = feed.scrollTop + item.top - box.top - Math.min(120, feed.clientHeight * 0.25);
  scrollHoldUntil = Date.now() + 600;
  feed.scrollTo({ top: Math.max(0, top), behavior: 'smooth' });
  clearFlash();
  flashId = node.dataset.id || '';
  node.classList.add('flash');
  flashTimer = setTimeout(clearFlash, 1600);
  // Следующее нажатие ведёт к более старому своему сообщению, а после самого
  // старого закреп возвращается к последнему.
  pinIdx = pinIdx === 0 ? list.length - 1 : pinIdx - 1;
  renderPin(list);
  // Пока лента едет к сообщению, закреп не пересчитываем по положению: он уже
  // показывает следующее. Следующая ручная прокрутка снова включит следование.
  pinJumping = true;
  clearTimeout(pinJumpTimer);
  pinJumpTimer = setTimeout(() => {
    pinJumping = false;
  }, 1500);
});

function nearEnd() {
  const feed = $('feed');
  return feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
}

// Закреп следует за прокруткой: показывает своё сообщение, ближайшее выше верхней
// кромки ленты, а у самого низа — последнее своё. Как в Telegram, где закреп
// меняется синхронно с движением ленты.
function followPin() {
  const list = myMessages();
  if (!list.length) return;
  let idx = list.length - 1;
  if (!nearEnd()) {
    const edge = $('feed').getBoundingClientRect().top + 1;
    idx = 0;
    for (let i = 0; i < list.length; i++) {
      if (list[i].getBoundingClientRect().top <= edge) idx = i;
      else break;
    }
  }
  if (idx !== pinIdx) {
    pinIdx = idx;
    renderPin(list);
  }
}

// Кнопка «в конец» — только когда лента прокручена вверх: внизу она не нужна.
function updateToEnd() {
  $('to-end').hidden = nodes.size === 0 || nearEnd();
}

$('to-end').addEventListener('click', () => {
  scrollHoldUntil = 0;
  const feed = $('feed');
  feed.scrollTo({ top: feed.scrollHeight, behavior: 'smooth' });
});

$('feed').addEventListener(
  'scroll',
  () => {
    updateToEnd();
    if (pinJumping) {
      // Ждём, пока плавная прокрутка к закрепу перестанет присылать события.
      clearTimeout(pinJumpTimer);
      pinJumpTimer = setTimeout(() => {
        pinJumping = false;
      }, 150);
      return;
    }
    followPin();
    askOlderAtTop();
  },
  { passive: true },
);

// ------------------------------------------------------------ запросы opencode

// Разрешения и формы opencode приходят отдельным сообщением и показываются над
// полем ввода. Это полный список: новый заменяет показанные карточки.

function clearPrompts() {
  promptSig = '';
  promptState.clear();
  promptControls.clear();
  promptCards.clear();
  $('prompts').replaceChildren();
}

function defaultsFor(prompt) {
  const out = {};
  for (const f of prompt.fields || []) {
    if (f.default !== undefined) out[f.key] = f.default;
  }
  return out;
}

function applyPrompts(list) {
  const sig = JSON.stringify(list);
  if (sig === promptSig) return;
  promptSig = sig;
  // Уже введённые значения переносим в новое дерево: пришёл ещё один запрос
  // или изменился список, но заполнять форму заново не нужно.
  const saved = new Map(promptState);
  for (const id of promptCards.keys()) saved.set(id, readControls(id));
  promptState.clear();
  promptControls.clear();
  promptCards.clear();
  const box = $('prompts');
  box.replaceChildren();
  const seen = new Set();
  for (const prompt of list) {
    if (!prompt || !prompt.id || seen.has(prompt.id)) continue;
    seen.add(prompt.id);
    promptState.set(prompt.id, saved.get(prompt.id) || defaultsFor(prompt));
    box.append(buildPrompt(prompt));
  }
}

function buildPrompt(prompt) {
  const card = el('div', `prompt ${prompt.kind}`);
  card.dataset.prompt = prompt.id;
  card.append(
    el('div', 'prompt-title', prompt.title || (prompt.kind === 'permission' ? 'opencode просит разрешение' : 'Вопрос opencode')),
  );
  if (prompt.text) card.append(el('div', 'prompt-text', prompt.text));
  if (prompt.kind === 'permission') buildPermission(card, prompt);
  else buildForm(card, prompt);
  card.append(el('div', 'prompt-error'));
  promptCards.set(prompt.id, card);
  setPromptBusy(prompt.id, promptSending.has(prompt.id));
  return card;
}

function buildPermission(card, prompt) {
  const actions = el('div', 'prompt-actions');
  const button = (label, cls, decision, title) => {
    const b = el('button', cls, label);
    b.type = 'button';
    if (title) b.title = title;
    b.addEventListener('click', () => sendPromptReply(prompt, { decision }));
    actions.append(b);
  };
  button('Разрешить', 'primary', 'once');
  if (prompt.save && prompt.save.length) {
    button('Разрешить всегда', '', 'always', `Сохранит правило: ${prompt.save.join(', ')}`);
  }
  button('Отклонить', 'danger', 'reject');
  card.append(actions);
  // Видно, что именно будет разрешено навсегда, — как в окне opencode.
  if (prompt.save && prompt.save.length) {
    card.append(el('div', 'prompt-save', `Всегда разрешит: ${prompt.save.join(', ')}`));
  }
}

function buildForm(card, prompt) {
  const values = promptState.get(prompt.id) || defaultsFor(prompt);
  const controls = new Map();
  promptControls.set(prompt.id, controls);
  const fields = el('div', 'prompt-fields');
  const changed = () => {
    promptState.set(prompt.id, readControls(prompt.id));
    updateConditions(prompt);
  };
  for (const f of prompt.fields || []) {
    const control = buildField(f, values[f.key], changed);
    controls.set(f.key, control);
    fields.append(control.node);
  }
  card.append(fields);
  // Enter в текстовом поле отправляет ответ, как в окне opencode.
  fields.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' || e.isComposing || e.target.tagName !== 'INPUT') return;
    if (e.target.type === 'radio' || e.target.type === 'checkbox') return;
    e.preventDefault();
    submitPrompt(prompt);
  });
  const actions = el('div', 'prompt-actions');
  const ok = el('button', 'primary', 'Отправить');
  ok.type = 'button';
  ok.addEventListener('click', () => submitPrompt(prompt));
  const cancel = el('button', 'link', 'Отменить');
  cancel.type = 'button';
  cancel.addEventListener('click', () => sendPromptReply(prompt, { cancel: true }));
  actions.append(ok, cancel);
  card.append(actions);
  updateConditions(prompt);
}

// Обёртка поля: заголовок, пояснение и сам элемент управления.
function fieldWrap(f, title) {
  const wrap = el('div', 'prompt-field');
  const label = el('div', 'prompt-label');
  label.append(document.createTextNode(title || f.key));
  if (f.required) label.append(el('span', 'req', ' *'));
  wrap.append(label);
  if (f.description) wrap.append(el('div', 'prompt-desc', f.description));
  return wrap;
}

function buildField(f, initial, changed) {
  const wrap = fieldWrap(f, f.title);
  let read = () => undefined;
  let set = () => {};

  if (f.kind === 'external') {
    // Ссылку из формы открываем, только если это обычный http(s)-адрес.
    let href = '';
    try {
      const url = new URL(f.url, location.origin);
      if (url.protocol === 'http:' || url.protocol === 'https:') href = url.href;
    } catch {}
    if (href) {
      const link = el('a', 'prompt-external', 'Открыть');
      link.href = href;
      link.target = '_blank';
      link.rel = 'noopener noreferrer';
      wrap.append(link);
    } else if (f.url) {
      wrap.append(el('div', 'prompt-desc', f.url));
    }
  } else if (f.kind === 'string' && f.options && f.options.length) {
    // Выбор одного варианта; при custom можно ввести своё значение.
    const box = el('div', 'choices');
    const name = `pf${++promptUid}`;
    const radios = [];
    let customRadio = null;
    let customInput = null;
    let customRow = null;
    const addChoice = (value, label, description) => {
      const line = el('label', 'choice');
      const input = document.createElement('input');
      input.type = 'radio';
      input.name = name;
      input.value = value;
      const text = el('span', 'choice-text');
      text.append(el('span', 'choice-label', label || value));
      if (description) text.append(el('span', 'choice-desc', description));
      line.append(input, text);
      input.addEventListener('change', () => { syncCustom(); changed(); });
      box.append(line);
      const entry = { input, value };
      radios.push(entry);
      return entry;
    };
    for (const o of f.options) addChoice(o.value, o.label, o.description);
    if (f.custom) {
      customRadio = addChoice('__custom__', 'Другой вариант', '');
      const row = el('div', 'prompt-custom');
      customInput = document.createElement('input');
      customInput.type = 'text';
      customInput.placeholder = 'Своё значение';
      customInput.addEventListener('input', changed);
      row.append(customInput);
      box.append(row);
      customRow = row;
    }
    const syncCustom = () => {
      if (customRow) customRow.hidden = !(customRadio && customRadio.input.checked);
    };
    read = () => {
      const sel = radios.find((r) => r.input.checked);
      if (!sel) return undefined;
      if (sel.value === '__custom__') return customInput.value.trim();
      return sel.value;
    };
    set = (v) => {
      if (v === undefined || v === null) return;
      const text = String(v);
      const found = radios.find((r) => r.value === text);
      if (found) {
        found.input.checked = true;
      } else if (customRadio) {
        customRadio.input.checked = true;
        customInput.value = text;
      }
      syncCustom();
    };
    wrap.append(box);
    set(initial);
    syncCustom();
  } else if (f.kind === 'multiselect') {
    // Несколько вариантов; при custom можно добавить своё значение.
    const box = el('div', 'choices');
    const boxes = [];
    let customBox = null;
    let customInput = null;
    let customRow = null;
    for (const o of f.options || []) {
      const line = el('label', 'choice');
      const input = document.createElement('input');
      input.type = 'checkbox';
      input.value = o.value;
      const text = el('span', 'choice-text');
      text.append(el('span', 'choice-label', o.label || o.value));
      if (o.description) text.append(el('span', 'choice-desc', o.description));
      line.append(input, text);
      input.addEventListener('change', changed);
      box.append(line);
      boxes.push(input);
    }
    const syncCustom = () => {
      if (customRow) customRow.hidden = !(customBox && customBox.checked);
    };
    if (f.custom) {
      const line = el('label', 'choice');
      customBox = document.createElement('input');
      customBox.type = 'checkbox';
      const text = el('span', 'choice-text');
      text.append(el('span', 'choice-label', 'Другой вариант'));
      line.append(customBox, text);
      customBox.addEventListener('change', () => { syncCustom(); changed(); });
      box.append(line);
      const row = el('div', 'prompt-custom');
      customInput = document.createElement('input');
      customInput.type = 'text';
      customInput.placeholder = 'Своё значение';
      customInput.addEventListener('input', changed);
      row.append(customInput);
      box.append(row);
      customRow = row;
    }
    read = () => {
      const out = boxes.filter((b) => b.checked).map((b) => b.value);
      if (customBox && customBox.checked) {
        const text = customInput.value.trim();
        if (text) out.push(text);
      }
      return out;
    };
    set = (v) => {
      const items = Array.isArray(v) ? v : [];
      const known = new Set((f.options || []).map((o) => o.value));
      for (const b of boxes) b.checked = items.includes(b.value);
      const custom = items.filter((x) => !known.has(x));
      if (customBox && custom.length) {
        customBox.checked = true;
        customInput.value = custom.join(', ');
      }
      syncCustom();
    };
    wrap.append(box);
    set(initial);
    syncCustom();
  } else if (f.kind === 'boolean') {
    const select = document.createElement('select');
    for (const [value, label] of [['', 'Не выбрано'], ['true', 'Да'], ['false', 'Нет']]) {
      const opt = el('option', '', label);
      opt.value = value;
      select.append(opt);
    }
    if (initial !== undefined && initial !== null) select.value = String(initial);
    select.addEventListener('change', changed);
    read = () => (select.value === '' ? undefined : select.value === 'true');
    set = (v) => { select.value = v === undefined || v === null ? '' : String(v); };
    wrap.append(select);
  } else if (f.kind === 'number' || f.kind === 'integer') {
    const input = document.createElement('input');
    input.type = 'number';
    input.step = f.kind === 'integer' ? '1' : 'any';
    if (f.min !== undefined) input.min = String(f.min);
    if (f.max !== undefined) input.max = String(f.max);
    if (initial !== undefined && initial !== null) input.value = String(initial);
    input.addEventListener('input', changed);
    read = () => (input.value === '' ? undefined : Number(input.value));
    set = (v) => { input.value = v === undefined || v === null ? '' : String(v); };
    wrap.append(input);
  } else {
    // Строка без готовых вариантов: обычное поле ввода.
    const input = document.createElement('input');
    input.type =
      f.format === 'email' ? 'email'
      : f.format === 'uri' ? 'url'
      : f.format === 'date' ? 'date'
      : f.format === 'date-time' ? 'datetime-local'
      : 'text';
    if (f.placeholder) input.placeholder = f.placeholder;
    if (f.min_length !== undefined) input.minLength = f.min_length;
    if (f.max_length !== undefined) input.maxLength = f.max_length;
    if (f.pattern) input.pattern = f.pattern;
    if (initial !== undefined && initial !== null) input.value = String(initial);
    input.addEventListener('input', changed);
    read = () => input.value;
    set = (v) => { input.value = v === undefined || v === null ? '' : String(v); };
    wrap.append(input);
  }

  return { node: wrap, read, set };
}

function readControls(id) {
  const controls = promptControls.get(id);
  const out = {};
  if (!controls) return out;
  for (const [key, control] of controls) {
    const value = control.read();
    if (value !== undefined) out[key] = value;
  }
  return out;
}

// Условия показа поля, как в окне opencode: поле с невыполненным условием
// не показывается и в ответ не попадает (даже его значение по умолчанию).
function conditionsMet(f, values) {
  if (f.kind === 'external' || !f.when || !f.when.length) return true;
  return f.when.every((when) => {
    const value = values[when.key];
    if (value === undefined) return false;
    const equal = Array.isArray(value) ? value.includes(String(when.value)) : value === when.value;
    return when.op === 'neq' ? !equal : equal;
  });
}

function fieldVisible(f, values) {
  if (f.kind === 'external') return true;
  return !f.hidden && conditionsMet(f, values);
}

function updateConditions(prompt) {
  const values = readControls(prompt.id);
  promptState.set(prompt.id, values);
  const controls = promptControls.get(prompt.id);
  if (!controls) return;
  for (const f of prompt.fields || []) {
    const control = controls.get(f.key);
    if (control) control.node.hidden = !fieldVisible(f, values);
  }
}

function collectAnswer(prompt) {
  const values = readControls(prompt.id);
  const answer = {};
  for (const f of prompt.fields || []) {
    if (f.kind === 'external') continue;
    if (!conditionsMet(f, values)) continue;
    // Скрытое поле передаёт значение по умолчанию, как в окне opencode.
    if (f.hidden) {
      if (f.default !== undefined) answer[f.key] = f.default;
      continue;
    }
    const value = values[f.key];
    if (f.kind === 'multiselect') {
      if (Array.isArray(value) && value.length) answer[f.key] = value;
    } else if (f.kind === 'boolean') {
      if (typeof value === 'boolean') answer[f.key] = value;
    } else if (f.kind === 'number' || f.kind === 'integer') {
      if (typeof value === 'number' && Number.isFinite(value)) answer[f.key] = value;
    } else if (typeof value === 'string' && value !== '') {
      answer[f.key] = value;
    }
  }
  return answer;
}

function validateAnswer(prompt) {
  const values = readControls(prompt.id);
  for (const f of prompt.fields || []) {
    if (f.kind === 'external' || !fieldVisible(f, values)) continue;
    const value = values[f.key];
    const label = f.title || f.key;
    const empty = value === undefined || value === '' || (Array.isArray(value) && !value.length);
    if (f.required && empty) {
      return `${label}: ${f.kind === 'multiselect' ? 'выберите хотя бы один вариант' : 'укажите значение'}`;
    }
    if (empty) continue;
    if (f.kind === 'multiselect') {
      if (f.min_items !== undefined && value.length < f.min_items) return `${label}: выберите не меньше ${f.min_items}`;
      if (f.max_items !== undefined && value.length > f.max_items) return `${label}: выберите не больше ${f.max_items}`;
    } else if (f.kind === 'number' || f.kind === 'integer') {
      if (!Number.isFinite(value)) return `${label}: нужно число`;
      if (f.kind === 'integer' && !Number.isInteger(value)) return `${label}: нужно целое число`;
      if (f.min !== undefined && value < f.min) return `${label}: не меньше ${f.min}`;
      if (f.max !== undefined && value > f.max) return `${label}: не больше ${f.max}`;
    } else if (typeof value === 'string') {
      if (f.min_length !== undefined && value.length < f.min_length) return `${label}: не короче ${f.min_length} символов`;
      if (f.max_length !== undefined && value.length > f.max_length) return `${label}: не длиннее ${f.max_length} символов`;
      if (f.pattern) {
        try {
          if (!new RegExp(f.pattern).test(value)) return `${label}: не соответствует формату`;
        } catch {
          // Некорректное выражение проверит сам opencode.
        }
      }
      if (f.format === 'email' && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value)) return `${label}: нужен адрес почты`;
      if (f.format === 'uri') {
        try {
          new URL(value);
        } catch {
          return `${label}: нужна ссылка`;
        }
      }
      if (f.format === 'date' && !/^\d{4}-\d{2}-\d{2}$/.test(value)) return `${label}: нужна дата в виде ГГГГ-ММ-ДД`;
      if (f.format === 'date-time' && Number.isNaN(new Date(value).getTime())) {
        return `${label}: нужны дата и время`;
      }
    }
  }
  return '';
}

function submitPrompt(prompt) {
  const card = promptCards.get(prompt.id);
  const error = validateAnswer(prompt);
  if (error) {
    if (card) card.querySelector('.prompt-error').textContent = error;
    return;
  }
  if (card) card.querySelector('.prompt-error').textContent = '';
  sendPromptReply(prompt, { answer: collectAnswer(prompt) });
}

function sendPromptReply(prompt, payload) {
  const cur = current();
  if (!cur || promptSending.has(prompt.id)) return;
  const id = String(++cmdSeq);
  const msg = {
    t: 'prompt_reply',
    id,
    agent: cur.agent.name,
    session: cur.session.key,
    prompt: prompt.id,
    kind: prompt.kind,
    ...payload,
  };
  if (!send(msg)) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  promptSending.add(prompt.id);
  promptReplyIds.set(id, prompt.id);
  setPromptBusy(prompt.id, true);
  setHint(payload.cancel ? 'Отменяем запрос…' : 'Отправляем ответ…');
}

function setPromptBusy(id, busy) {
  const card = promptCards.get(id);
  if (!card) return;
  card.classList.toggle('sending', busy);
  for (const node of card.querySelectorAll('button, input, select, textarea')) node.disabled = busy;
}

function onPromptResult(msg) {
  const promptId = promptReplyIds.get(msg.id);
  promptReplyIds.delete(msg.id);
  if (msg.ok) {
    // Карточка исчезнет со следующим обновлением списка. До этого не даём
    // отправить ответ второй раз; таймер — на случай, если обновление не придёт.
    if (promptId) {
      setTimeout(() => {
        promptSending.delete(promptId);
        setPromptBusy(promptId, false);
      }, 8000);
    }
    setHint('Ответ отправлен в opencode.');
    return;
  }
  if (promptId) {
    promptSending.delete(promptId);
    setPromptBusy(promptId, false);
  }
  const error = `Ошибка: ${msg.error || 'неизвестная'}`;
  setHint(error, true);
  const card = promptId && promptCards.get(promptId);
  if (card) card.querySelector('.prompt-error').textContent = msg.error || 'Не удалось отправить ответ.';
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

// Модель для новой сессии: у opencode — его список (провайдер/модель), у
// Claude Code — алиасы, которые принимает `--model`. Значение можно выбрать
// только из списка: подсказки открываются по клику, ищут по подстроке, а
// недавние модели показываются первыми. Модель всегда явная: пустого пункта
// «по умолчанию» нет, без выбора запуск не уходит.
const launchModelSuggest = Suggest.attach($('launch-model'), {
  items: launchModelItems,
  // Клик по пустому полю тоже открывает весь список — иначе модель пришлось бы
  // набирать руками, пока не появится первое совпадение.
  minChars: 0,
  openAllWhenExact: true,
  // Моделей у opencode много: список прокручивается, а не обрезается.
  limit: 0,
  // Недавние — сверху, остальные — в порядке списка агента.
  rank: (item) => (item.recent ? 0 : 1),
  emptyText: (q) =>
    q
      ? 'Ничего не найдено.'
      : launchModelsRequest
        ? 'Загружаем модели…'
        : 'Список моделей не пришёл: без него запуск не уходит.',
  emptyOnQueryless: true,
});

// Недавние модели: свежие сверху, отдельно для каждого инструмента. Хранятся в
// браузере, потому что список opencode зависит от машины и провайдеров, а у
// Claude Code это алиасы CLI — оба набора нужны при следующем запуске.
const MODEL_RECENTS = 5;
const modelRecentsKey = (tool) => `aipanel.model-recents:${tool}`;

function modelRecents(tool) {
  try {
    const list = JSON.parse(localStorage.getItem(modelRecentsKey(tool)) || '[]');
    return Array.isArray(list) ? list.filter((v) => typeof v === 'string' && v) : [];
  } catch {
    return [];
  }
}

function rememberModel(tool, value) {
  value = String(value || '').trim();
  if (!value) return;
  const list = [value, ...modelRecents(tool).filter((v) => v !== value)].slice(0, MODEL_RECENTS);
  try {
    localStorage.setItem(modelRecentsKey(tool), JSON.stringify(list));
  } catch {}
}

function launchModelKey() {
  return `${$('launch-agent').value}\n${$('launch-tool').value}`;
}

// Значение поля модели: у Claude Code — алиас CLI, у opencode — «провайдер/модель».
function launchModelValue(tool, m) {
  return tool === 'claude' ? m.id : `${m.provider}/${m.id}`;
}

function launchModelItems() {
  const tool = $('launch-tool').value;
  const list = launchModelCache.get(launchModelKey()) || [];
  const recents = modelRecents(tool);
  return list.map((m) => {
    const value = launchModelValue(tool, m);
    return {
      value,
      label: m.name || m.id,
      hint: tool === 'claude' ? `claude --model ${m.id}` : value,
      recent: recents.includes(value),
    };
  });
}

// Есть ли значение в списке агента: null — ни в списке, ни в прошлом выборе.
function launchModelKnown(value) {
  const tool = $('launch-tool').value;
  const v = value.trim().toLowerCase();
  if (!v) return false;
  const list = launchModelCache.get(launchModelKey());
  if (!list) {
    // Список ещё в пути: прошлый выбор пришёл из списка, а свежий набор — нет.
    return v === (launchModelChoice[tool] || '').toLowerCase() ? true : null;
  }
  return list.some((m) => launchModelValue(tool, m).toLowerCase() === v);
}

// Готовая модель в поле: клик или Tab выделяет значение целиком, чтобы набор
// сразу заменял его, а не дописывался в конец — иначе фильтр не нашёл бы ничего.
function selectLaunchModel() {
  const value = $('launch-model').value.trim();
  if (value && launchModelKnown(value) === true) $('launch-model').select();
}
$('launch-model').addEventListener('focus', selectLaunchModel);
$('launch-model').addEventListener('click', selectLaunchModel);

function requestLaunchModels() {
  const agent = $('launch-agent').value;
  const tool = $('launch-tool').value;
  const key = launchModelKey();
  if (!agent || launchModelCache.has(key)) return;
  if (launchModelsRequest && launchModelsRequest.key === key) return;
  const id = String(++cmdSeq);
  if (!send({ t: 'tool_models', id, agent, tool, cwd: $('launch-cwd').value.trim() })) return;
  launchModelsRequest = { id, key };
}

// Поле модели при открытии окна и смене инструмента: своё значение у каждого
// инструмента, список подсказок — для выбранного компьютера и инструмента.
function syncLaunchModel() {
  const tool = $('launch-tool').value;
  $('launch-model').value = launchModelChoice[tool] || '';
  $('launch-model').placeholder =
    tool === 'claude'
      ? 'Выберите модель из списка (например, sonnet)'
      : 'Выберите модель из списка (провайдер/модель)';
  launchModelSuggest.close();
  requestLaunchModels();
}

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
  syncLaunchModel();
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
  if (e.target !== e.currentTarget || launchId !== null) return;
  // Клик мог прийти общему предку, хотя палец был внутри окна: так бывает, когда
  // раскладка сдвинулась между касанием и кликом (на телефоне закрылась клавиатура).
  // Такой клик за затемнение не считаем, иначе окно пропадает без запуска.
  const box = e.currentTarget.getBoundingClientRect();
  const inside = e.clientX >= box.left && e.clientX <= box.right && e.clientY >= box.top && e.clientY <= box.bottom;
  if (inside) return;
  e.currentTarget.close();
});

$('launch-agent').addEventListener('change', () => {
  // Если в поле был путь из подсказок прошлого компьютера, заменяем его на путь
  // нового: чужие абсолютные пути на другой машине обычно не существуют.
  const prev = launchDirCache;
  const cwd = $('launch-cwd').value.trim();
  const wasSuggested = !cwd || prev.dirs.some((d) => normCwd(d.cwd) === normCwd(cwd));
  const dirs = fillLaunchDirs();
  if (wasSuggested) $('launch-cwd').value = dirs[0]?.cwd ?? '';
  // У другого компьютера свой набор моделей.
  launchModelSuggest.close();
  requestLaunchModels();
});

$('launch-tool').addEventListener('change', syncLaunchModel);

$('launch-form').addEventListener('submit', (e) => {
  e.preventDefault();
  const agent = $('launch-agent').value;
  const tool = $('launch-tool').value;
  const cwd = $('launch-cwd').value.trim();
  const model = $('launch-model').value.trim();
  if (!agent) {
    launchHint('Нет подключённых агентов.', true);
    return;
  }
  if (!cwd) {
    launchHint('Укажите каталог.', true);
    $('launch-cwd').focus();
    return;
  }
  // Модель всегда явная и только из списка агента: без выбора запуск не уходит.
  if (!model) {
    launchHint('Выберите модель из списка.', true);
    launchModelSuggest.open();
    $('launch-model').focus();
    return;
  }
  const known = launchModelKnown(model);
  if (known !== true) {
    launchHint(
      known === null
        ? 'Список моделей ещё не пришёл. Подождите немного.'
        : 'Такой модели нет в списке. Выберите пункт из подсказок.',
      true,
    );
    if (known === null) requestLaunchModels();
    launchModelSuggest.open();
    $('launch-model').focus();
    $('launch-model').select();
    return;
  }
  const a = agents.find((x) => x.name === agent);
  const keys = new Set(a ? a.sessions.map((s) => s.key) : []);
  startLaunch({ agent, tool, cwd, model, keys, create: false });
});

// Отправляет запуск агенту. `create` разрешает создать каталог, которого нет:
// существование каталога проверяет машина агента, поэтому разрешение приходит
// только после её ответа «каталог не найден» и вопроса человеку.
function startLaunch(req) {
  const id = String(++cmdSeq);
  const msg = { t: 'start', id, agent: req.agent, tool: req.tool, cwd: req.cwd, model: req.model, create: req.create };
  if (!send(msg)) {
    launchHint('Нет соединения с сервером.', true);
    return false;
  }
  launchModelChoice[req.tool] = req.model;
  launchId = id;
  launchRequest = req;
  $('launch-ok').disabled = true;
  launchHint(req.create ? 'Создаём каталог и запускаем…' : 'Запускаем…');
  return true;
}

function onLaunchResult(msg) {
  const req = launchRequest;
  launchId = null;
  launchRequest = null;
  $('launch-ok').disabled = false;
  if (!msg.ok) {
    // Каталога на машине агента нет: спрашиваем, создавать ли его, и повторяем
    // запуск уже с разрешением. Остальные ошибки показываем как обычно.
    if (req && !req.create && String(msg.error || '').startsWith(DIR_MISSING)) {
      askMkdir(req);
      return;
    }
    launchHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  rememberModel(req.tool, req.model);
  $('launch').close();
  pendingLaunch = {
    agent: req.agent,
    tool: req.tool,
    cwd: req.cwd,
    key: msg.session || null,
    keys: req.keys,
    // У opencode идентификатор известен сразу, у Claude Code — только после того,
    // как тот запишет реестр (обычно 2-3 секунды). Долгое ожидание в неизвестном
    // каталоге бесполезно: окно могло остаться на вопросе про доверие.
    until: Date.now() + (msg.session ? 90000 : 20000),
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
    setHint(
      'Сессия не появилась в списке. Проверьте окно на компьютере: возможно, оно ждёт ответа на первый вопрос.',
      true,
    );
  }
}

// ------------------------------------------------------------ каталог новой сессии

// Существует ли каталог, знает только машина агента: панель узнаёт об этом из
// ответа на запуск и спрашивает человека, создавать ли каталог. Разрешение уходит
// тем же запуском с `create`, поэтому каталог появится на той же машине.
function askMkdir(req) {
  if (mkdirRequest) return;
  mkdirRequest = req;
  $('mkdir-text').textContent = `Каталога «${req.cwd}» на компьютере «${req.agent}» нет. Создать его?`;
  $('mkdir-hint').textContent = '';
  $('mkdir-hint').className = 'hint';
  $('mkdir-ok').disabled = false;
  $('mkdir-cancel').disabled = false;
  launchHint(`Каталога «${req.cwd}» нет.`);
  if (!$('mkdir').open) $('mkdir').showModal();
}

// Отказ от создания: окно запуска остаётся открытым — путь можно поправить.
function closeMkdir() {
  if (!mkdirRequest) return;
  mkdirRequest = null;
  $('mkdir').close();
  launchHint('Каталог не создан.');
  $('launch-cwd').focus();
}

$('mkdir-cancel').addEventListener('click', closeMkdir);

// Esc закрывает окно сам, поэтому здесь только сбрасываем ожидающий запуск.
$('mkdir').addEventListener('cancel', () => {
  mkdirRequest = null;
  launchHint('Каталог не создан.');
  $('launch-cwd').focus();
});

$('mkdir').addEventListener('click', (e) => {
  if (e.target === e.currentTarget) closeMkdir();
});

$('mkdir-form').addEventListener('submit', (e) => {
  e.preventDefault();
  const req = mkdirRequest;
  if (!req) return;
  mkdirRequest = null;
  $('mkdir').close();
  startLaunch({ ...req, create: true });
});

// ------------------------------------------------------------ панель сессии (телефон)

function setSessionMenu(open) {
  $('session-menu').hidden = !open;
  $('more').setAttribute('aria-expanded', String(open));
}

$('more').addEventListener('click', () => setSessionMenu($('session-menu').hidden));
$('sm-handoff').addEventListener('click', () => {
  setSessionMenu(false);
  $('handoff').click();
});
document.addEventListener('click', (e) => {
  if ($('session-menu').hidden) return;
  if (!e.target.closest('#session-menu, #more')) setSessionMenu(false);
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !$('session-menu').hidden) {
    setSessionMenu(false);
    $('more').focus();
  }
});

// ------------------------------------------------------------ продолжение сессии

$('handoff').addEventListener('click', () => {
  const cur = current();
  if (!cur || handoffId !== null) return;
  const tool = cur.session.tool === 'claude' ? 'opencode' : 'claude';
  const name = tool === 'claude' ? 'Claude Code' : 'opencode';
  const id = String(++cmdSeq);
  // Снимок ключей: новая сессия ищется как появившаяся в этом каталоге.
  const keys = new Set(cur.agent.sessions.map((s) => s.key));
  if (!send({ t: 'handoff', id, agent: cur.agent.name, session: cur.session.key, tool })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  handoffId = id;
  handoffRequest = { agent: cur.agent.name, tool, cwd: cur.session.cwd, keys };
  $('handoff').disabled = true;
  $('sm-handoff').disabled = true;
  setHint(`Собираем выжимку и открываем сессию ${name} в этом каталоге…`);
});

function onHandoffResult(msg) {
  const req = handoffRequest;
  handoffId = null;
  handoffRequest = null;
  $('handoff').disabled = false;
  $('sm-handoff').disabled = false;
  if (!req) return;
  if (!msg.ok) {
    setHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  pendingLaunch = {
    agent: req.agent,
    tool: req.tool,
    cwd: req.cwd,
    key: msg.session || null,
    keys: req.keys,
    until: Date.now() + (msg.session ? 90000 : 20000),
  };
  setHint(req.tool === 'claude' ? 'Открываем окно Claude Code…' : 'Открываем окно opencode…');
  checkPendingLaunch();
}

// ------------------------------------------------------------ команды

// Слэш-команды сессии (`/init`, `/clear`) — как в терминале Claude Code и
// opencode: набираешь `/` в начале сообщения, открывается список с описаниями.
// Список присылает агент; он же выполняет команду, когда сообщение отправлено.
// Модуль подсказок работает в режиме слова: выбор заменяет только `/имя`, а не
// весь текст поля, и не трогает Enter, если ничего не выбрано стрелками.
const slash = Suggest.attach($('text'), {
  minChars: 0,
  limit: 0,
  openOnFocus: false,
  emptyOnQueryless: true,
  emptyText: slashEmptyText,
  token: slashToken,
  items: slashItems,
  text: (item) => `/${item.value} `,
  // Совпадение с начала имени — выше совпадения в середине.
  rank: (item, q) => (q && item.value.toLowerCase().startsWith(q) ? 0 : 1),
  // `models` — не текстовая команда, а действие панели: окно выбора модели.
  onSelect: (item) => {
    if (item.action !== 'models') return;
    $('text').value = '';
    resetHistoryNav();
    autoGrow();
    openModels();
    return false;
  },
});

function commandKey(agent, session) {
  return `${agent}\n${session}`;
}

// Команды из панели выполняются только там, где агент может их передать:
// у Claude Code — набором в консоль открытой сессии (закрытая получила бы текст
// первым сообщением), у opencode — запросом к его API.
function slashAllowed(cur) {
  if (!cur || !cur.agent.online) return false;
  if (cur.session.tool === 'claude') return cur.session.delivery === 'console';
  return cur.session.delivery !== 'none';
}

function slashItems() {
  const cur = current();
  if (!cur || !slashAllowed(cur)) return [];
  const key = commandKey(cur.agent.name, cur.session.key);
  const list = sessionCommands.get(key);
  if (!list) {
    askCommands(cur.agent.name, cur.session.key);
    return [];
  }
  const items = list.map((c) => ({ value: c.name, label: c.name, hint: c.description || '' }));
  // Команды `/models` в API opencode нет — она живёт в его TUI. Зато модели
  // можно менять из панели, поэтому пункт добавляем сами.
  if (cur.session.tool === 'opencode' && !items.some((i) => i.value === 'models')) {
    items.push({ value: 'models', label: 'models', hint: 'Сменить модель сессии', action: 'models' });
    items.sort((a, b) => a.value.localeCompare(b.value));
  }
  return items;
}

// Слово после `/` в начале сообщения: только там список и открывается —
// «/» посреди текста это просто текст. Возвращает запрос без слэша и границы
// слова, чтобы выбор заменил именно его.
function slashToken(input) {
  const value = input.value;
  const at = input.selectionStart == null ? value.length : input.selectionStart;
  const m = /^\/\S*/.exec(value);
  if (!m || at < 1 || at > m[0].length) return null;
  return { query: value.slice(1, at), start: 0, end: m[0].length };
}

function askCommands(agent, session) {
  const key = commandKey(agent, session);
  const asked = commandsAsked.get(key);
  // Если ответ не пришёл (агент перезапустился), через время спросим снова.
  if (asked && Date.now() - asked < 10000) return;
  if (!send({ t: 'commands', agent, session })) return;
  commandsAsked.set(key, Date.now());
}

function slashEmptyText() {
  const cur = current();
  if (!cur) return '';
  const key = commandKey(cur.agent.name, cur.session.key);
  if (!slashAllowed(cur)) {
    if (!cur.agent.online) return 'Агент не в сети.';
    return cur.session.tool === 'claude'
      ? 'Команды Claude Code есть только у открытой в терминале сессии.'
      : 'В эту сессию команды отправить нельзя.';
  }
  const list = sessionCommands.get(key);
  if (list) return list.length ? 'Ничего не найдено' : 'Команд нет';
  if (!ws || ws.readyState !== WebSocket.OPEN) return 'Нет соединения с сервером.';
  const asked = commandsAsked.get(key);
  // Старый агент не знает запроса команд и просто молчит.
  if (asked && Date.now() - asked > 6000) return 'Список не пришёл: возможно, агент старой версии.';
  return 'Загружаем команды…';
}

// ------------------------------------------------------------ модель сессии

// `/models` — не команда opencode, а действие панели: список моделей приходит
// от агента, выбор уходит обратно и агент переключает сессию.
function askModels(agent, session) {
  const key = commandKey(agent, session);
  const asked = modelsAsked.get(key);
  if (asked && Date.now() - asked < 10000) return;
  if (!send({ t: 'models', agent, session })) return;
  modelsAsked.set(key, Date.now());
}

function openModels() {
  const cur = current();
  if (!cur || cur.session.tool !== 'opencode') return;
  modelsSession = { agent: cur.agent.name, session: cur.session.key, current: cur.session.model || '' };
  $('models-filter').value = '';
  setModelsHint('');
  renderModels(modelsSession);
  if (!$('models').open) $('models').showModal();
  $('models-filter').focus();
  askModels(modelsSession.agent, modelsSession.session);
}

function setModelsHint(text, isError) {
  const hint = $('models-hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
}

function renderModels(req) {
  const list = $('models-list');
  list.replaceChildren();
  const all = sessionModels.get(commandKey(req.agent, req.session));
  if (!all) {
    list.append(el('div', 'models-empty', 'Загружаем модели…'));
    return;
  }
  const q = $('models-filter').value.trim().toLowerCase();
  const rows = all.filter((m) => !q || `${m.name} ${m.provider} ${m.id}`.toLowerCase().includes(q));
  if (!rows.length) {
    list.append(el('div', 'models-empty', all.length ? 'Ничего не найдено.' : 'Список моделей пуст.'));
    return;
  }
  for (const m of rows) {
    const full = `${m.provider}/${m.id}`;
    const btn = el('button', 'model');
    btn.type = 'button';
    if (full === req.current) btn.classList.add('current');
    btn.title = full;
    btn.append(el('span', 'model-name', m.name || m.id), el('span', 'model-meta', full));
    btn.addEventListener('click', () => setSessionModel(m));
    list.append(btn);
  }
}

function setSessionModel(m) {
  if (!modelsSession || modelSet) return;
  const id = String(++cmdSeq);
  const label = m.name || m.id;
  const ok = send({
    t: 'set_model',
    id,
    agent: modelsSession.agent,
    session: modelsSession.session,
    provider: m.provider,
    model: m.id,
  });
  if (!ok) {
    setModelsHint('Нет соединения с сервером.', true);
    return;
  }
  modelSet = { id, label, value: `${m.provider}/${m.id}` };
  setModelsHint(`Переключаем на ${label}…`);
  for (const b of $('models-list').querySelectorAll('.model')) b.disabled = true;
}

function onModelResult(msg) {
  const done = modelSet;
  modelSet = null;
  for (const b of $('models-list').querySelectorAll('.model')) b.disabled = false;
  if (!msg.ok) {
    setModelsHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  // Модель только что использовали — она должна быть вверху списка в окне запуска.
  if (done) rememberModel('opencode', done.value);
  $('models').close();
  setHint(done && done.label ? `Модель: ${done.label}` : 'Модель переключена.');
}

$('models-filter').addEventListener('input', () => {
  if (modelsSession) renderModels(modelsSession);
});
// Enter в фильтре выбирает первую подходящую модель — как в списке команд.
$('models-form').addEventListener('submit', (e) => {
  e.preventDefault();
  const first = $('models-list').querySelector('.model');
  if (first) first.click();
});
$('models-cancel').addEventListener('click', () => $('models').close());
$('models').addEventListener('close', () => {
  modelsSession = null;
  setModelsHint('');
});

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
if (!touchInput) $('text').placeholder += '. Enter — отправить, Shift+Enter — новая строка, / — команды';

$('text').addEventListener('keydown', (e) => {
  // Список команд обрабатывает стрелки, Enter и Escape раньше нас: если он уже
  // занялся клавишей, полю остаётся только всё остальное.
  if (e.defaultPrevented) return;
  // isComposing: Enter при выборе слова в IME не должен ни отправлять сообщение,
  // ни листать историю.
  if (e.isComposing) return;
  if (e.key === 'Enter') {
    if (e.shiftKey || touchInput) return;
    // Enter без выбранного стрелками пункта остаётся отправкой: список просто
    // закрывается, а набранный текст уходит как есть.
    if (slash.isOpen()) slash.close();
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
  interruptId = String(++cmdSeq);
  send({ t: 'interrupt', id: interruptId, agent: selected.agent, session: selected.session });
  setHint('Прерываю…');
});

function onInterruptResult(msg) {
  interruptId = null;
  if (!msg.ok) {
    setHint(`Не удалось прервать: ${msg.error || 'неизвестная ошибка'}`, true);
    return;
  }
  setHint('Ход остановлен. Если запрос был отложен, opencode продолжит его выполнять.');
}

boot();
