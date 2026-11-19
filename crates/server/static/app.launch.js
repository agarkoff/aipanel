'use strict';

// Окно «Новая сессия», часть вторая: выбор модели, запуск и создание
// каталога, которого на машине агента ещё нет.

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
  // Выбранная человеком модель — уже не подставленная: из-за списка её не
  // очищаем, даже если она в него не попала (тогда запуск скажет об этом сам).
  onSelect: () => {
    launchModelPrefill = '';
  },
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

// Недавние модели: свежие сверху, отдельно для каждого инструмента и машины.
// Их помнит агент (`launch.json`) и присылает вместе со списком моделей, поэтому
// на телефоне подсказка та же, что на компьютере. В браузере не храним: см.
// AGENTS.md, «Состояние интерфейса: память агента, а не браузера».
function modelRecents() {
  return launchModelRecents.get(launchModelKey()) || [];
}

function launchModelKey() {
  return `${$('launch-agent').value}\n${$('launch-tool').value}`;
}

// Состояние полей окна: что подставлено, а что человек выбрал сам. Пока поле не
// трогали, ответ агента с сохранёнными параметрами может его поправить.
let launchToolPinned = false; // инструмент взят из открытой сессии, его не меняем
let launchToolEdited = false; // инструмент выбрал человек
// Значение поля модели, подставленное из сохранённых параметров: если его нет в
// списке этого компьютера, поле очищается, а не остаётся нерабочим текстом.
let launchModelPrefill = '';

// Параметры запуска выбранного компьютера; пустые строки — запусков не было.
function launchPrefsOf(agent) {
  return launchPrefs.get(agent) || { tool: '', claude: '', opencode: '' };
}

// Инструмент, которым запускали новую сессию на этом компьютере в прошлый раз.
function launchToolChoice(agent) {
  return launchPrefsOf(agent).tool;
}

// Модель, которой запускали сессию этого инструмента на этом компьютере.
function launchModelChoice(tool) {
  return launchPrefsOf($('launch-agent').value)[tool] || '';
}

// Значение поля модели: у Claude Code — алиас CLI, у opencode — «провайдер/модель».
function launchModelValue(tool, m) {
  return tool === 'claude' ? m.id : `${m.provider}/${m.id}`;
}

function launchModelItems() {
  const tool = $('launch-tool').value;
  const list = launchModelCache.get(launchModelKey()) || [];
  const recents = modelRecents();
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
    return v === launchModelChoice(tool).toLowerCase() ? true : null;
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

// Параметры окна запуска хранит агент: на телефоне открытой сессии нет, и
// инструмент с моделью неоткуда было бы взять. Спрашиваем при каждом открытии
// окна: за это время новую сессию могли запустить с другого устройства.
function requestLaunchPrefs() {
  const agent = $('launch-agent').value;
  if (!agent) return;
  // Такой же запрос уже в пути: второй ответ ничего не добавит.
  for (const pending of launchPrefsAsks.values()) if (pending === agent) return;
  const id = String(++cmdSeq);
  if (!send({ t: 'launch_state', id, agent })) return;
  launchPrefsAsks.set(id, agent);
}

// Ответ агента пришёл: подставляем то, чем запускали на этом компьютере в
// прошлый раз, если человек ещё ничего не выбрал в этом окне.
function onLaunchPrefs(agent) {
  if (!$('launch').open || $('launch-agent').value !== agent) return;
  const tool = launchToolChoice(agent);
  const switched = tool && tool !== $('launch-tool').value && !launchToolPinned && !launchToolEdited;
  if (switched) $('launch-tool').value = tool;
  // Поле модели, которое человек не трогал, берём из свежих параметров: у
  // другого инструмента своя модель, а запуск с другого устройства мог сменить
  // и модель этого. Набранное вручную не переписываем.
  const value = $('launch-model').value.trim();
  if (switched || !value || value === launchModelPrefill) syncLaunchModel();
  else requestLaunchModels();
}

// Значение, подставленное из сохранённых параметров, могло исчезнуть из списка
// (модель убрали с компьютера): пустое поле честнее нерабочего текста.
function dropStaleLaunchModel() {
  const value = $('launch-model').value.trim();
  if (!value || value !== launchModelPrefill || launchModelKnown(value) === true) return;
  $('launch-model').value = '';
  launchModelPrefill = '';
  launchModelSuggest.close();
}

// Поле модели при открытии окна и смене инструмента: своё значение у каждого
// инструмента, список подсказок — для выбранного компьютера и инструмента.
function syncLaunchModel() {
  const tool = $('launch-tool').value;
  launchModelPrefill = launchModelChoice(tool);
  $('launch-model').value = launchModelPrefill;
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
  // Дерево выбора папки помнит состояние; у другого компьютера пути чужие.
  syncLaunchTreeAgent();
  const dirs = fillLaunchDirs();
  const agent = $('launch-agent').value;
  const cur = current();
  const sameAgent = cur && cur.agent.name === agent;
  launchToolEdited = false;
  // Инструмент открытой сессии — самый вероятный выбор. Если сессии нет (на
  // телефоне её обычно и нет), берём тот, которым запускали здесь в прошлый
  // раз; пока агент не ответил, остаётся Claude Code — первый в списке.
  launchToolPinned = Boolean(sameAgent && (cur.session.tool === 'claude' || cur.session.tool === 'opencode'));
  $('launch-tool').value = launchToolPinned ? cur.session.tool : launchToolChoice(agent) || 'claude';
  // Каталог открытой сессии — самый вероятный выбор, иначе самый свежий из списка.
  $('launch-cwd').value = sameAgent && cur.session.cwd ? cur.session.cwd : (dirs[0]?.cwd ?? '');
  if (launchTree.open) {
    renderLaunchTree();
    loadLaunchDir('');
    revealLaunchPath($('launch-cwd').value);
  }
  syncLaunchModel();
  requestLaunchPrefs();
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

// Закрытое окно не должно дорисовывать дерево: раскрытие пути отменяется.
$('launch').addEventListener('close', () => {
  launchTreeReveal += 1;
});

$('launch-agent').addEventListener('change', () => {
  // Если в поле был путь из подсказок прошлого компьютера, заменяем его на путь
  // нового: чужие абсолютные пути на другой машине обычно не существуют.
  const prev = launchDirCache;
  const cwd = $('launch-cwd').value.trim();
  const wasSuggested = !cwd || prev.dirs.some((d) => normCwd(d.cwd) === normCwd(cwd));
  const dirs = fillLaunchDirs();
  if (wasSuggested) $('launch-cwd').value = dirs[0]?.cwd ?? '';
  // У другого компьютера свои диски: дерево выбора папки начинается заново.
  syncLaunchTreeAgent();
  if (launchTree.open) {
    renderLaunchTree();
    loadLaunchDir('');
    revealLaunchPath($('launch-cwd').value);
  }
  // У другого компьютера свои параметры запуска и свой набор моделей: инструмент
  // берём из них, а не оставляем от прошлой машины.
  launchToolEdited = false;
  launchToolPinned = false;
  $('launch-tool').value = launchToolChoice($('launch-agent').value) || 'claude';
  syncLaunchModel();
  requestLaunchPrefs();
});

$('launch-tool').addEventListener('change', () => {
  launchToolEdited = true;
  syncLaunchModel();
});

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
  // Агент запомнил запуск у себя (`launch.json`) — инструмент, модель и место
  // модели в недавних. Спрашиваем свежие параметры, а не догадываемся о них:
  // ответ подставит их при следующем открытии окна.
  requestLaunchPrefs();
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
