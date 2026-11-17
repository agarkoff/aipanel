'use strict';

// Вход в панель и соединение с сервером: адрес, переподключение и разбор
// сообщений агентов, которые дальше расходятся по своим участкам.

// ------------------------------------------------------------ вход

async function boot() {
  const res = await fetch('/api/me');
  if (!res.ok) {
    $('login').hidden = false;
    return;
  }
  const me = await res.json().catch(() => ({}));
  if (me.user) {
    who = me;
    renderWho();
    $('user').hidden = false;
  }
  start();
}

// Вошедший пользователь и версия панели из /api/me.
let who = null;

// `9.14.0` → `9.14`: в панели версия показывается как major.minor.
function shortVersion(v) {
  const [major, minor] = String(v || '').split('.');
  return minor === undefined ? String(v || '') : `${major}.${minor}`;
}

// «имя · ПАНЕЛЬ / АГЕНТ»: после версии панели через слеш — версия агентов в сети.
// Если версии у агентов разные, они перечисляются через запятую; какая у какого
// агента — в подсказке.
function renderWho() {
  if (!who) return;
  const online = agents.filter((a) => a.online && a.version);
  const versions = [...new Set(online.map((a) => shortVersion(a.version)))];
  let text = who.user;
  if (who.version) text += ` · ${who.version}`;
  if (versions.length) text += ` / ${versions.join(', ')}`;
  const tip = [];
  if (who.version) tip.push(`Версия панели ${who.version}`);
  for (const a of online) tip.push(`Агент ${a.name}: ${shortVersion(a.version)}`);
  $('user').textContent = text;
  $('user').title = tip.join('\n');
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
    if (selected) {
      send({ t: 'watch', agent: selected.agent, session: selected.session });
      // Избранное живёт у агента: после переподключения берём его заново.
      send({ t: 'favorites', agent: selected.agent, session: selected.session });
    }
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
    renderWho();
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
  } else if (msg.t === 'favorites') {
    if (!selected || msg.agent !== selected.agent || msg.session !== selected.session) return;
    favorites = Array.isArray(msg.items) ? msg.items : [];
    applyFavorites();
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
  } else if (msg.t === 'dirs') {
    onDirsResult(msg);
  } else if (msg.t === 'result') {
    if (launchId && msg.id === launchId) onLaunchResult(msg);
    else if (handoffId && msg.id === handoffId) onHandoffResult(msg);
    else if (cancelQueuedId && msg.id === cancelQueuedId) onCancelQueuedResult(msg);
    else if (deleteId && msg.id === deleteId) onDeleteResult(msg);
    else if (closeIds.has(msg.id)) onCloseResult(msg);
    else if (renameIds.has(msg.id)) onRenameResult(msg);
    else if (interruptId && msg.id === interruptId) onInterruptResult(msg);
    else if (modelSet && msg.id === modelSet.id) onModelResult(msg);
    else if (promptReplyIds.has(msg.id)) onPromptResult(msg);
    else if (shot && msg.id === shot.id) shotError(msg.error || 'агент не ответил');
    else if (!onDirsError(msg.id, msg.error))
      setHint(msg.ok ? 'Доставлено агенту.' : `Ошибка: ${msg.error || 'неизвестная'}`, !msg.ok);
  } else if (msg.t === 'screen') {
    onScreen(msg);
  } else if (msg.t === 'screens') {
    onScreens(msg);
  }
}
