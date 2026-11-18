'use strict';

// Список сессий и действия над сессией: закрыть консоль, переименовать,
// удалить, открыть ленту. Здесь же панель под стрелкой и продолжение
// сессии в другом инструменте.

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
  // Пока в строке правится название, список не пересобираем: иначе поле
  // потеряет фокус и недопечатанное имя. Новый список придёт после сохранения.
  if (renaming && renaming.place === 'list') return;
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
    const list = a.sessions.filter((s) => {
      if (!q) return true;
      const r = s.repo || {};
      return `${s.title} ${s.cwd} ${r.name || ''} ${r.url || ''} ${r.branch || ''}`.toLowerCase().includes(q);
    });
    if (!list.length) box.append(el('div', 'empty', 'Сессий нет.'));
    for (const s of list) {
      const row = el('div', 'session-row');
      const btn = el('button', 'session');
      btn.type = 'button';
      // Подсвечивается открытая сессия, а после выхода из неё — та, где только что были.
      const mark = selected || lastViewed;
      if (mark && mark.agent === a.name && mark.session === s.key) btn.classList.add('active');
      // Название открытой сессии правится нажатием на него; остальная строка
      // по-прежнему открывает сессию. Правится именно открытая (selected), а не
      // просто подсвеченная после выхода: на телефоне нажатие на имя последней
      // сессии должно снова её открывать, а не включать поле.
      const name = el('span', 'name', s.title || s.key);
      if (selected && selected.agent === a.name && selected.session === s.key) {
        name.classList.add('rename-target');
        name.title = 'Переименовать сессию';
        name.addEventListener('click', (e) => {
          e.preventDefault();
          e.stopPropagation();
          startRename('list', a, s, row);
        });
      }
      const meta = el('span', 'meta', `${s.tool} · ${placeLabel(s)}`);
      meta.title = placeHint(s);
      btn.append(
        el('span', dotClass(s, a.online)),
        name,
        el('span', 'when', ago(s.updated_at)),
        meta,
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
      const del = el('button', 'session-delete');
      del.append(icon(['M5 5l6 6', 'M11 5l-6 6']));
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

// ------------------------------------------------------------ переименование

// Название сессии меняется у самого инструмента: Claude Code получает в
// транскрипт строку custom-title (так же, как от своего /rename), opencode —
// новый заголовок через свой API. Панель правит имя на месте: нажатие на
// название открытой сессии в списке или на заголовок в шапке открывает поле,
// Enter и уход из поля сохраняют, Esc отменяет, пустое имя — тоже отмена.
const RENAME_MAX = 200; // столько же, сколько у сервера и агента
let renaming = null; // { agent, session, place, old, input } — открытое поле

// Убирает поле, ничего не отправляя. Нужно, когда строку или шапку перерисовывает
// другое действие: клик по другой сессии, возврат к списку.
function dropRename() {
  if (!renaming) return;
  const input = renaming.input;
  renaming = null;
  input.remove();
}

function startRename(place, agent, session, row) {
  if (!agent.online) {
    setHint('Агент не в сети: переименовать нельзя.', true);
    return;
  }
  commitRename();
  if (place === 'list' && !row) return;
  const old = session.title || session.key;
  const input = el('input', `rename rename-${place}`);
  input.type = 'text';
  input.value = old;
  input.maxLength = RENAME_MAX;
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.setAttribute('aria-label', 'Название сессии');
  input.setAttribute('enterkeyhint', 'done');
  // Свайп строки не должен ловить движение пальца по полю.
  for (const type of ['pointerdown', 'pointermove', 'pointerup', 'click']) {
    input.addEventListener(type, (e) => e.stopPropagation());
  }
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeRename();
    }
  });
  input.addEventListener('blur', () => {
    if (renaming && renaming.input === input) commitRename();
  });
  renaming = { agent: agent.name, session: session.key, place, old, input };
  // В шапке поле встаёт на место заголовка, в списке — поверх всей строки: на
  // телефоне так удобнее печатать, и не приходится угадывать место под палец.
  if (place === 'header') {
    $('title').replaceChildren(input);
    $('rename').hidden = true;
  } else if (row.isConnected) row.append(input);
  else {
    renaming = null;
    return;
  }
  input.focus({ preventScroll: true });
  input.setSelectionRange(0, input.value.length);
}

// Возвращает список и шапку к обычному виду: их текст снова собирается из данных.
function closeRename() {
  if (!renaming) return;
  dropRename();
  renderSessions();
  renderHeader();
}

function commitRename() {
  const r = renaming;
  if (!r) return;
  const value = r.input.value.trim();
  dropRename();
  // Пустое имя и прежнее имя ничего не меняют: отменяем молча.
  if (value && value !== r.old) {
    const id = String(++cmdSeq);
    if (send({ t: 'rename', id, agent: r.agent, session: r.session, title: value })) {
      renameIds.add(id);
      setHint('Переименовываем…');
    } else {
      setHint('Нет соединения с сервером.', true);
    }
  }
  renderSessions();
  renderHeader();
}

function onRenameResult(msg) {
  renameIds.delete(msg.id);
  // Новое имя придёт следующим списком сессий от агента.
  setHint(msg.ok ? 'Сессия переименована.' : `Ошибка: ${msg.error || 'не удалось переименовать'}`, !msg.ok);
}

$('title').addEventListener('click', () => {
  if (renaming) return;
  const cur = current();
  if (cur) startRename('header', cur.agent, cur.session);
});
// Карандаш рядом с заголовком делает правку имени заметной — в том числе на
// телефоне, где наведение и подсказки недоступны.
$('rename').addEventListener('click', () => {
  const cur = current();
  if (cur) startRename('header', cur.agent, cur.session);
});
// Enter и пробел в шапке делают то же, что нажатие: правка имени с клавиатуры.
$('title').addEventListener('keydown', (e) => {
  if (e.target !== e.currentTarget) return;
  if (e.key === 'Enter' || e.key === ' ') {
    e.preventDefault();
    $('title').click();
  }
});

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
  // Удалённой сессии больше не к чему возвращать черновик.
  drafts.delete(`${req.agent}\n${req.session}`);
  draftSaved.delete(`${req.agent}\n${req.session}`);
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
  // Недописанное в прежней сессии сохраняем до смены selected — ключ берётся
  // по ней, иначе черновик ушёл бы не той сессии. Отправляем сразу: агенту
  // он нужен раньше, чем поле покажет черновик другой сессии.
  saveDraft();
  flushDraft();
  // Сессию открыли — её уведомление в строке состояния больше не нужно.
  closeNotice(agent, session);
  dropRename();
  selected = { agent, session };
  lastViewed = selected;
  nodes.clear();
  feedRequests.clear();
  favorites = [];
  favButtons.clear();
  setFavoritesMenu(false);
  applyFavorites();
  clearPrompts();
  clearTasks();
  // Вкладки и подагенты относятся к прежней сессии.
  resetSubagentView();
  // В поле — черновик открытой сессии, а не текст предыдущей.
  loadDraft();
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
  // Список избранного приходит от агента: у него он и хранится.
  send({ t: 'favorites', agent, session });
  renderSessions();
  renderHeader();
  // Поле уже видно: черновик сессии может быть многострочным, и высоту ему
  // нужно посчитать сейчас, а не при первом нажатии клавиши.
  autoGrow();
}

function closeView() {
  // Возврат к списку не должен терять недописанный текст: он останется
  // за сессией до следующего её открытия — и здесь, и на других устройствах.
  saveDraft();
  flushDraft();
  dropRename();
  $('app').classList.remove('viewing');
  selected = null;
  // Поля ввода в списке нет; пустое значение сбрасывает и просмотр истории.
  loadDraft();
  favorites = [];
  favButtons.clear();
  setFavoritesMenu(false);
  applyFavorites();
  clearPrompts();
  clearTasks();
  resetSubagentView();
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
