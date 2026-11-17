'use strict';

// Слэш-команды сессии (`/init`, `/clear`) и переключение модели через
// `/models`: список присылает агент, он же выполняет выбор.

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
