'use strict';

// Подагенты открытой сессии. Их вызовы в ленте — карточки (`kind: "subagent"`),
// а лента выбранного подагента открывается панелью поверх ленты сессии: сессия
// остаётся на месте, и к ней не нужно возвращаться вкладкой. Статус, время и
// токены карточка берёт из списка, который агент присылает целиком.

let subCalls = new Map(); // id вызова -> элемент ленты подагента
let subPendingCalls = []; // вызовы, результата которых ещё не было
let subOpenedFrom = null; // карточка, из которой открыли панель

// Полный список подагентов от агента: он заменяет показанный.
function applySubagents(list) {
  subagents = Array.isArray(list) ? list : [];
  refreshSubViews();
}

function subagentById(id) {
  return subagents.find((s) => s.id === id) || null;
}

function subagentTitle(sub) {
  const title = (sub && sub.title) || (sub && sub.id) || '';
  const line = title.split('\n', 1)[0];
  return line.length > 120 ? `${line.slice(0, 120)}…` : line;
}

// Короткая подпись из вызова: описание задачи одной строкой. Подагент и его
// карточка приходят из разных мест, поэтому подпись сравнивается по началу.
function shortTitle(text) {
  return String(text || '')
    .split('\n', 1)[0]
    .trim()
    .replace(/…$/, '')
    .slice(0, 200);
}

// Подагент карточки: точная ссылка из агента, а если её нет (синхронный вызов
// opencode) — ближайший по времени подагент с той же подписью.
function subagentForItem(item) {
  if (item.sub) return item.sub;
  const want = shortTitle(item.text);
  if (!want) return '';
  let best = null;
  for (const sub of subagents) {
    if (!sub.id || shortTitle(sub.title) !== want) continue;
    // Время запуска и отметка вызова относятся к одному моменту; далёкие
    // совпадения — одноимённые подагенты из разных мест сессии.
    if (!item.ts || !sub.started_at) continue;
    const d = Math.abs(sub.started_at - item.ts);
    if (d > 10 * 60 * 1000) continue;
    if (!best || d < best.d) best = { id: sub.id, d };
  }
  return best ? best.id : '';
}

function cardSubId(item) {
  return subagentForItem(item);
}

// Карточка вызова подагента в ленте. Она стоит там, где вызов был на самом
// деле, поэтому несколько подагентов подряд видно рядом, а старые не теряются.
function subagentCard(item) {
  cardItems.set(item.id, item);
  const card = el('button', 'sub-card');
  card.type = 'button';
  card.dataset.card = item.id;
  const kind = el('span', 'sub-card-kind', item.tool || '');
  if (!item.tool) kind.hidden = true;
  card.append(el('span', 'sub-card-dot'), kind);
  card.append(el('span', 'sub-card-title', shortTitle(item.text) || 'Подагент'), el('span', 'sub-card-meta'));
  const open = el('span', 'sub-card-open');
  open.append(icon(['M6 3.5 10.5 8 6 12.5']));
  card.append(open);
  refreshSubCard(card, item);
  card.addEventListener('click', () => openCardSub(item.id));
  return card;
}

// Итог подагента под карточкой: ответ, который получил основной агент. Раньше
// он не был виден вовсе — только в ленте самого подагента.
function subagentResult(item) {
  const details = el('details', 'sub-result');
  const body = el('div', 'body md');
  body.append(renderMarkdown(item.text, mdOptions()));
  details.append(el('summary', '', 'Итог подагента'), body);
  return details;
}

// Статус, время и токены карточки. Обновляется и при новом списке, и раз в
// секунду, пока подагент работает.
function refreshSubCard(card, item) {
  const id = cardSubId(item);
  const sub = id ? subagentById(id) : null;
  if (id) card.dataset.sub = id;
  else delete card.dataset.sub;
  card.classList.toggle('nolink', !id);
  card.classList.toggle('active', Boolean(id) && subDrawer === id);
  card.classList.toggle('running', Boolean(sub && sub.running));
  const meta = card.querySelector('.sub-card-meta');
  const parts = [];
  if (sub) {
    const end = sub.running ? Date.now() : sub.ended_at;
    if (sub.started_at && end > sub.started_at) parts.push(fmtElapsed(end - sub.started_at));
    const turn = sub.turn || {};
    if (turn.input) parts.push(`↑ ${fmtTokens(turn.input)}`);
    if (turn.output) parts.push(`↓ ${fmtTokens(turn.output)}`);
    meta.textContent = `${sub.running ? 'идёт' : 'готово'}${parts.length ? ` · ${parts.join(' · ')}` : ''}`;
  } else {
    meta.textContent = '';
  }
  const title = subagentTitle(sub) || shortTitle(item.text) || 'Подагент';
  const titleNode = card.querySelector('.sub-card-title');
  // В вызове подписи не было (старая версия инструмента) — берём её из списка.
  if (titleNode && !shortTitle(item.text)) titleNode.textContent = title;
  if (sub && sub.agent) {
    const kind = card.querySelector('.sub-card-kind');
    if (kind) {
      kind.textContent = sub.agent;
      kind.hidden = false;
    }
  }
  card.title = [title, sub && sub.agent ? `агент: ${sub.agent}` : '', sub ? (sub.running ? 'работает' : 'завершён') : '']
    .filter(Boolean)
    .join('\n');
  card.setAttribute('aria-label', id ? `Открыть ленту подагента: ${title}` : `Подагент: ${title}`);
}

// Обновление карточек и шапки открытой панели: карточки остаются в ленте, а
// статус приходит отдельным списком каждые пару секунд.
function refreshSubViews() {
  for (const card of document.querySelectorAll('.sub-card')) {
    const item = cardItems.get(card.dataset.card);
    if (item) refreshSubCard(card, item);
  }
  if (subDrawer) renderSubView();
}

// Шапка панели подагента: подпись, тип, работа, время и токены.
function renderSubView() {
  const box = $('sub-view');
  if (!selected || !subDrawer) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  const sub = subagentById(subDrawer);
  const item = sub ? null : findSubCardItem(subDrawer);
  const title = subagentTitle(sub) || shortTitle(item && item.text) || 'Подагент';
  $('sub-view-title').textContent = title;
  $('sub-view-title').title = title;
  const parts = [];
  if (sub) {
    if (sub.agent) parts.push(sub.agent);
    if (sub.running) {
      parts.push(
        sub.started_at && Date.now() > sub.started_at ? `работает ${fmtElapsed(Date.now() - sub.started_at)}` : 'работает',
      );
    } else {
      parts.push('завершён');
      if (sub.started_at && sub.ended_at > sub.started_at) parts.push(`всего ${fmtElapsed(sub.ended_at - sub.started_at)}`);
    }
    const turn = sub.turn || {};
    if (turn.input) parts.push(`↑ ${fmtTokens(turn.input)}`);
    if (turn.output) parts.push(`↓ ${fmtTokens(turn.output)}`);
  } else if (item && item.tool) {
    parts.push(item.tool);
  }
  $('sub-view-meta').textContent = parts.join(' · ');
}

// Открывает панель подагента (или переключает на другого). Подписку на ленту
// переключает здесь же: агент шлёт ленту только тому, кто её смотрит.
function openSubDrawer(id) {
  if (!selected || !id) return;
  if (subDrawer === id) {
    renderSubView();
    refreshSubViews();
    return;
  }
  if (subDrawer) saveSubView();
  const previous = subDrawer;
  subDrawer = id;
  if (previous) send({ t: 'unwatch_sub', agent: selected.agent, session: selected.session, sub: previous });
  send({ t: 'watch_sub', agent: selected.agent, session: selected.session, sub: id });
  updateSessionUrl();
  showSubView();
  refreshSubViews();
}

// Показывает панель из кэша (если он есть) и ставит ленту туда, где её читали.
function showSubView() {
  // На широком экране панель занимает колонку и сужает ленту сессии: место
  // чтения сохраняем, чтобы карточка не «уезжала» из-под глаз.
  const place = typeof feedPlace === 'function' ? feedPlace() : null;
  $('sub-view').hidden = false;
  setSubOpen(true);
  renderSubView();
  renderSubFeed(tabKey(subDrawer));
  // Поле ввода относится к сессии и на время панели скрыто.
  renderHeader();
  if (place) applyFeedPlace(place);
  const back = $('sub-view-back');
  if (back) back.focus({ preventScroll: true });
}

// Панель открыта: на широком экране она становится колонкой рядом с лентой.
function setSubOpen(on) {
  const wrap = document.querySelector('.feed-wrap');
  if (wrap) wrap.classList.toggle('sub-open', on);
}

// Закрывает панель и возвращает фокус на карточку, из которой её открыли.
function closeSubDrawer() {
  if (!subDrawer) return;
  saveSubView();
  const place = typeof feedPlace === 'function' ? feedPlace() : null;
  const id = subDrawer;
  subDrawer = '';
  // Лента закрытой панели больше не нужна: при следующем открытии агент
  // пришлёт её заново, а место чтения останется в subScroll/subOpen.
  feedItems.delete(tabKey(id));
  feedOrder.delete(tabKey(id));
  subNodes.clear();
  subCalls.clear();
  subPendingCalls = [];
  $('sub-feed').replaceChildren();
  $('sub-view').hidden = true;
  setSubOpen(false);
  if (selected) send({ t: 'unwatch_sub', agent: selected.agent, session: selected.session, sub: id });
  updateSessionUrl();
  // Поле ввода снова относится к сессии.
  renderHeader();
  if (place) applyFeedPlace(place);
  refreshSubViews();
  if (subOpenedFrom && document.contains(subOpenedFrom)) subOpenedFrom.focus({ preventScroll: true });
  subOpenedFrom = null;
}

// Место чтения и раскрытые вызовы панели — на случай, если её откроют снова.
function saveSubView() {
  if (!subDrawer) return;
  const root = $('sub-feed');
  subScroll.set(subDrawer, root.scrollTop);
  subOpen.set(subDrawer, subOpenItems(root));
}

function subOpenItems(root) {
  const open = new Set();
  for (const node of root.querySelectorAll('.item')) {
    if (node.querySelector('details[open]')) open.add(node.dataset.id);
  }
  return open;
}

function subRestoreOpen(root, open) {
  if (!open || !open.size) return;
  for (const node of root.querySelectorAll('.item')) {
    if (!open.has(node.dataset.id)) continue;
    const details = node.querySelector('details');
    if (details) details.open = true;
  }
}

function subAtEnd(root) {
  return root.scrollHeight - root.scrollTop - root.clientHeight < 80;
}

// Собирает панель из кэша. Пустой кэш отличается от ещё не пришедшей ленты:
// первый появляется только после ответа агента.
function renderSubFeed(key) {
  const root = $('sub-feed');
  subNodes.clear();
  subCalls.clear();
  subPendingCalls = [];
  root.replaceChildren();
  const items = feedItems.get(key);
  if (!items) {
    root.append(el('div', 'empty', 'Загрузка ленты подагента…'));
    return;
  }
  if (!items.size) {
    root.append(el('div', 'empty', 'Лента подагента пуста.'));
    return;
  }
  for (const id of feedOrder.get(key) || []) {
    const item = items.get(id);
    if (item) placeSubItem(item, true);
  }
  subRestoreOpen(root, subOpen.get(subDrawer));
  const saved = subScroll.get(subDrawer);
  root.scrollTop = saved === undefined ? root.scrollHeight : Math.min(saved, root.scrollHeight);
}

// Один элемент ленты подагента. Подсказки и раскрытые вызовы те же, что в
// сессии, но избранного нет: оно относится к сессии, а не к подагенту.
function placeSubItem(item, isNew) {
  const old = subNodes.get(item.id);
  subTrackCall(item, isNew);
  let call = null;
  if (item.kind === 'tool_result') call = subCallFor(item, isNew);
  const node = buildItem(item, call, { sub: true });
  if (old) {
    const was = old.querySelector('details');
    const now = node.querySelector('details');
    if (was && now) now.open = was.open;
    old.replaceWith(node);
  } else {
    $('sub-feed').append(node);
  }
  subNodes.set(item.id, node);
}

// Вызов инструмента для его результата: у opencode id результата — id вызова с
// `:r`, у Claude Code результаты идут в порядке вызовов.
function subCallFor(item, isNew) {
  if (item.id.endsWith(':r')) {
    const call = subCalls.get(item.id.slice(0, -2));
    if (call) {
      subPendingCalls = subPendingCalls.filter((c) => c !== call);
      return call;
    }
  }
  return isNew ? subPendingCalls.shift() || null : null;
}

function subTrackCall(item, isNew) {
  if (item.kind === 'tool_use') {
    subCalls.set(item.id, item);
    if (isNew) subPendingCalls.push(item);
  } else if (item.kind === 'text' && item.role === 'assistant' && isNew) {
    subPendingCalls = [];
  }
}

// Лента подагента пришла от агента: копим в кэше, а в панель ставим, пока она
// открыта. Пересборка идёт только по `reset`, остальное дописывается на место.
function applySubHistory(msg) {
  const key = tabKey(msg.sub);
  if (msg.reset) cacheReset(key);
  const items = (msg.items || []).map(plainItem);
  for (const item of items) cachePut(key, item);
  if (msg.error && (msg.reset || items.length)) {
    setHint(`Лента подагента: ${msg.error}`, true);
  }
  if (subDrawer !== msg.sub) return;
  const root = $('sub-feed');
  // Лента уезжает в конец, только если это первое открытие (места чтения ещё
  // нет) или человек уже у низа. Пересборка после переподключения не должна
  // выдёргивать его из середины транскрипта.
  const stick = !subScroll.has(msg.sub) && subAtEnd(root);
  if (msg.reset) {
    // Транскрипт прочитан заново: место чтения и раскрытое остаются прежними.
    // «Загрузка» вместо ленты — это первое открытие, сохранять нечего.
    if (root.querySelector('.item')) saveSubView();
    const open = subOpenItems(root);
    renderSubFeed(key);
    subRestoreOpen(root, open);
  } else {
    for (const item of items) placeSubItem(item, !subNodes.has(item.id));
  }
  if (stick) root.scrollTop = root.scrollHeight;
  renderSubView();
}

// Карточка нажата: открываем ленту её подагента. Ссылку берём из элемента
// ленты, а не из DOM: список подагентов мог прийти позже.
function openCardSub(itemId) {
  const item = findFeedItem(itemId);
  if (!item) return;
  const id = cardSubId(item);
  if (!id) {
    setHint('Лента подагента не найдена: агент не сообщил её идентификатор.', true);
    return;
  }
  subOpenedFrom = [...document.querySelectorAll('.sub-card')].find((c) => c.dataset.card === itemId) || null;
  openSubDrawer(id);
}

// Ссылка на ленту подагента из адреса: она открывается сразу, не дожидаясь
// списка — агенту хватает id.
function openLinkedSub(id) {
  if (!id || !selected) return;
  openSubDrawer(id);
}

function findFeedItem(id) {
  for (const items of feedItems.values()) {
    const item = items.get(id);
    if (item) return item;
  }
  return null;
}

function findSubCardItem(id) {
  for (const items of feedItems.values()) {
    for (const item of items.values()) {
      if (item.kind === 'subagent' && cardSubId(item) === id) return item;
    }
  }
  return null;
}

// Сессию закрыли или открыли другую: подагенты и их карточки к ней не относятся.
function resetSubagentView() {
  subagents = [];
  subDrawer = '';
  subOpenedFrom = null;
  cardItems.clear();
  subNodes.clear();
  subCalls.clear();
  subPendingCalls = [];
  subScroll.clear();
  subOpen.clear();
  for (const key of [...feedItems.keys()]) {
    if (!key) continue;
    feedItems.delete(key);
    feedOrder.delete(key);
  }
  $('sub-feed').replaceChildren();
  $('sub-view').hidden = true;
  setSubOpen(false);
  renderSubView();
}

$('sub-view-back').addEventListener('click', closeSubDrawer);

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape' || !subDrawer) return;
  // Диалог и меню закрываются сами: не закрываем за ними ещё и панель.
  if (document.querySelector('dialog[open]')) return;
  if (!$('favorites-menu').hidden || !$('session-menu').hidden) return;
  closeSubDrawer();
});
