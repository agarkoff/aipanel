'use strict';

// Окно «Новая сессия», часть первая: каталоги на машине агента — подсказки
// в поле пути и дерево выбора папки.

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

// Дерево выбора папки — как в проводнике: диски, раскрытие узлов, выбор папки.
// Список читает агент на своей машине, поэтому на Windows показываются диски, а
// на Linux и macOS — файловая система. Состояние живёт, пока открыто окно:
// закрыли «Новую сессию» — дерево помнит, где остановились.
const launchTree = {
  agent: '',
  open: false,
  expanded: new Set(),
  selected: '',
  focus: '',
  // path -> { entries, error, loading, promise, resolve, timer }; '' — корни.
  nodes: new Map(),
};
// id команды -> { agent, path } — чтобы ответ агента попал в свой узел.
const launchTreeRequests = new Map();
// Номер раскрытия пути: ответы прошлого раскрытия не должны двигать дерево.
let launchTreeReveal = 0;
const TREE_TIMEOUT = 15000;

function launchTreeAgent() {
  const agent = agents.find((a) => a.name === $('launch-agent').value);
  return agent ? agent.os : '';
}

// Пути сравниваем без хвостового разделителя, а на Windows — ещё и без учёта
// регистра: агент возвращает «D:\Projects», человек мог набрать «d:/projects».
function sameTreePath(a, b) {
  const norm = (p) => p.replace(/\\/g, '/').replace(/\/+$/, '');
  const x = norm(a);
  const y = norm(b);
  return x === y || (launchTreeAgent() === 'windows' && x.toLowerCase() === y.toLowerCase());
}

function treeNodeState(path) {
  let node = launchTree.nodes.get(path);
  if (!node) {
    node = { entries: null, error: '', loading: false, promise: null, resolve: null, timer: null };
    launchTree.nodes.set(path, node);
  }
  return node;
}

function resetLaunchTree() {
  launchTreeReveal += 1;
  for (const req of launchTreeRequests.values()) {
    const node = launchTree.nodes.get(req.path);
    if (node) {
      clearTimeout(node.timer);
      node.timer = null;
      node.loading = false;
      node.promise = null;
      // Будим ожидающее раскрытие: оно проверит номер и выйдет.
      node.resolve?.(node);
      node.resolve = null;
    }
  }
  launchTreeRequests.clear();
  launchTree.agent = $('launch-agent').value;
  launchTree.expanded.clear();
  launchTree.selected = '';
  launchTree.focus = '';
  launchTree.nodes.clear();
  renderLaunchTree();
}

// Дерево относится к выбранному компьютеру: у другого агента свои диски и пути.
function syncLaunchTreeAgent() {
  if (launchTree.agent === $('launch-agent').value) return false;
  resetLaunchTree();
  return true;
}

// Список папок узла. Повторный вызов ждёт уже отправленный запрос, а не шлёт
// новый: раскрытие одного узла не должно порождать пачку одинаковых команд.
function loadLaunchDir(path) {
  const node = treeNodeState(path);
  if (node.entries || node.error) return Promise.resolve(node);
  if (node.promise) return node.promise;
  const agent = launchTree.agent;
  const id = String(++cmdSeq);
  if (!send({ t: 'dirs', id, agent, path })) {
    node.error = 'Нет соединения с панелью.';
    renderLaunchTree();
    return Promise.resolve(node);
  }
  node.loading = true;
  node.promise = new Promise((resolve) => {
    node.resolve = resolve;
  });
  node.timer = setTimeout(() => {
    if (!launchTreeRequests.has(id)) return;
    launchTreeRequests.delete(id);
    node.loading = false;
    node.promise = null;
    node.error = 'Агент не ответил.';
    node.resolve?.(node);
    node.resolve = null;
    renderLaunchTree();
  }, TREE_TIMEOUT);
  launchTreeRequests.set(id, { agent, path });
  renderLaunchTree();
  return node.promise;
}

function finishTreeRequest(node) {
  if (!node) return;
  clearTimeout(node.timer);
  node.timer = null;
  node.loading = false;
  node.promise = null;
  const resolve = node.resolve;
  node.resolve = null;
  resolve?.(node);
}

// Ответ агента на запрос содержимого каталога.
function onDirsResult(msg) {
  const req = launchTreeRequests.get(msg.id);
  if (!req) return;
  launchTreeRequests.delete(msg.id);
  const node = launchTree.nodes.get(req.path);
  if (!node) return;
  node.entries = Array.isArray(msg.entries) ? msg.entries : [];
  node.error = msg.error || '';
  finishTreeRequest(node);
  if (launchTree.agent === req.agent) renderLaunchTree();
}

// Ошибка на уровне команды: сервер не нашёл агента или отклонил путь.
function onDirsError(id, error) {
  const req = launchTreeRequests.get(id);
  if (!req) return false;
  launchTreeRequests.delete(id);
  const node = launchTree.nodes.get(req.path);
  if (node) {
    node.entries = null;
    node.error = error || 'Не удалось прочитать каталог.';
    finishTreeRequest(node);
    renderLaunchTree();
  }
  return true;
}

// Видимые строки дерева — в порядке показа: по ним ходят стрелки.
function treeRows() {
  return [...$('launch-tree').querySelectorAll('.tree-row')];
}

function treeRow(path) {
  return treeRows().find((row) => row.dataset.path === path) || null;
}

function treeNote(text, depth, isError) {
  const note = el('div', isError ? 'tree-note error' : 'tree-note', text);
  note.style.paddingLeft = `${4 + depth * 16 + 22}px`;
  return note;
}

function treeGlyph(kind) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('width', '16');
  svg.setAttribute('height', '16');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('class', `tree-icon ${kind}`);
  const path = document.createElementNS(SVG_NS, 'path');
  if (kind === 'drive') {
    path.setAttribute('d', 'M2.5 4.5h11v7h-11zM5 9.2h3.2M12 9.2h.01');
    path.setAttribute('fill', 'none');
    path.setAttribute('stroke', 'currentColor');
    path.setAttribute('stroke-width', '1.2');
    path.setAttribute('stroke-linejoin', 'round');
    path.setAttribute('stroke-linecap', 'round');
  } else {
    // Жёлтая папка проводника: цвет не зависит от темы, как в Windows.
    path.setAttribute(
      'd',
      'M1.5 4.2a1.2 1.2 0 0 1 1.2-1.2h3l1.3 1.5h5.3a1.2 1.2 0 0 1 1.2 1.2v6.9a1.2 1.2 0 0 1-1.2 1.2H2.7a1.2 1.2 0 0 1-1.2-1.2z',
    );
    path.setAttribute('fill', '#e2b33c');
    path.setAttribute('stroke', '#c2952c');
    path.setAttribute('stroke-width', '.8');
    path.setAttribute('stroke-linejoin', 'round');
  }
  svg.append(path);
  return svg;
}

function renderTreeNode(entry, depth) {
  const open = launchTree.expanded.has(entry.path);
  const node = launchTree.nodes.get(entry.path);
  const nodeEl = el('div', open ? 'tree-node open' : 'tree-node');

  const row = el('div', 'tree-row');
  row.style.paddingLeft = `${4 + depth * 16}px`;
  row.dataset.path = entry.path;
  row.tabIndex = entry.path === (launchTree.focus || launchTree.selected) ? 0 : -1;
  row.setAttribute('role', 'treeitem');
  row.setAttribute('aria-level', String(depth + 1));
  row.setAttribute('aria-expanded', String(open));
  row.title = entry.path;
  if (entry.path === launchTree.selected) {
    row.classList.add('selected');
    row.setAttribute('aria-selected', 'true');
  }

  const twist = el('span', node && node.entries && !node.entries.length ? 'tree-twist empty' : 'tree-twist');
  twist.setAttribute('aria-hidden', 'true');
  const chevron = document.createElementNS(SVG_NS, 'svg');
  chevron.setAttribute('viewBox', '0 0 16 16');
  chevron.setAttribute('width', '12');
  chevron.setAttribute('height', '12');
  const chevronPath = document.createElementNS(SVG_NS, 'path');
  chevronPath.setAttribute('d', 'M6 4l4 4-4 4');
  chevronPath.setAttribute('fill', 'none');
  chevronPath.setAttribute('stroke', 'currentColor');
  chevronPath.setAttribute('stroke-width', '1.4');
  chevronPath.setAttribute('stroke-linecap', 'round');
  chevronPath.setAttribute('stroke-linejoin', 'round');
  chevron.append(chevronPath);
  twist.append(chevron);
  twist.addEventListener('click', (e) => {
    e.stopPropagation();
    toggleLaunchNode(entry.path);
  });

  row.append(twist, treeGlyph(depth === 0 ? 'drive' : 'folder'), el('span', 'tree-name', entry.name));
  row.addEventListener('click', () => selectLaunchDir(entry.path));
  row.addEventListener('dblclick', () => toggleLaunchNode(entry.path));
  row.addEventListener('keydown', (e) => onTreeKey(e, entry.path));
  nodeEl.append(row);

  if (open) {
    const children = el('div', 'tree-children');
    if (!node || (node.loading && !node.entries && !node.error)) {
      children.append(treeNote('Загрузка…', depth + 1));
    } else if (node.error) {
      children.append(treeNote(node.error, depth + 1, true));
    } else if (node.entries) {
      for (const child of node.entries) children.append(renderTreeNode(child, depth + 1));
    }
    nodeEl.append(children);
  }
  return nodeEl;
}

function renderLaunchTree() {
  const box = $('launch-tree');
  box.hidden = !launchTree.open;
  if (!launchTree.open) return;
  const hadFocus = box.contains(document.activeElement);
  const focusPath = launchTree.focus || launchTree.selected;
  box.replaceChildren();

  const roots = launchTree.nodes.get('');
  if (!roots || (roots.loading && !roots.entries && !roots.error)) {
    box.append(treeNote('Загрузка…', 0));
  } else if (roots.error) {
    box.append(treeNote(roots.error, 0, true));
  } else if (!roots.entries || !roots.entries.length) {
    box.append(treeNote('Нет доступных дисков.', 0));
  } else {
    for (const entry of roots.entries) box.append(renderTreeNode(entry, 0));
  }

  // После перерисовки возвращаем фокус на ту же строку — иначе он уходил бы
  // в начало окна при каждом раскрытии и загрузке.
  if (!box.querySelector('.tree-row[tabindex="0"]')) {
    const first = box.querySelector('.tree-row');
    if (first) first.tabIndex = 0;
  }
  if (hadFocus) treeRow(focusPath)?.focus();
}

function selectLaunchDir(path) {
  launchTree.selected = path;
  launchTree.focus = path;
  $('launch-cwd').value = path;
  launchTreeReveal += 1;
  launchHint('');
  renderLaunchTree();
  const row = treeRow(path);
  row?.focus();
  row?.scrollIntoView({ block: 'nearest' });
}

// Раскрытие узла: список подгружается при первом раскрытии. Повторное
// раскрытие узла с ошибкой пробует прочитать каталог заново.
function toggleLaunchNode(path, force) {
  const open = force === undefined ? !launchTree.expanded.has(path) : force;
  const node = launchTree.nodes.get(path);
  if (open) {
    launchTree.expanded.add(path);
    if (node && (node.error || (node.entries && !node.entries.length))) {
      node.error = '';
      node.entries = null;
    }
    launchTree.focus = path;
    renderLaunchTree();
    loadLaunchDir(path);
  } else {
    launchTree.expanded.delete(path);
    launchTree.focus = path;
    renderLaunchTree();
  }
  treeRow(path)?.focus();
}

function moveTreeFocus(from, delta) {
  const rows = treeRows();
  const at = rows.findIndex((row) => row.dataset.path === from);
  const next = rows[Math.max(0, Math.min(rows.length - 1, at + delta))];
  if (next) selectLaunchDir(next.dataset.path);
}

function onTreeKey(e, path) {
  const rows = treeRows();
  if (e.key === 'ArrowDown') {
    e.preventDefault();
    moveTreeFocus(path, 1);
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    moveTreeFocus(path, -1);
  } else if (e.key === 'Home') {
    e.preventDefault();
    if (rows[0]) selectLaunchDir(rows[0].dataset.path);
  } else if (e.key === 'End') {
    e.preventDefault();
    if (rows.length) selectLaunchDir(rows[rows.length - 1].dataset.path);
  } else if (e.key === 'ArrowRight') {
    e.preventDefault();
    if (!launchTree.expanded.has(path)) toggleLaunchNode(path, true);
    else {
      const row = treeRow(path);
      const child = row?.parentElement?.querySelector(':scope > .tree-children > .tree-node > .tree-row');
      if (child) selectLaunchDir(child.dataset.path);
    }
  } else if (e.key === 'ArrowLeft') {
    e.preventDefault();
    if (launchTree.expanded.has(path)) {
      toggleLaunchNode(path, false);
    } else {
      const nodeEl = treeRow(path)?.parentElement;
      const parentRow = nodeEl?.parentElement?.parentElement?.querySelector(':scope > .tree-row');
      if (parentRow) selectLaunchDir(parentRow.dataset.path);
    }
  } else if (e.key === 'Enter') {
    e.preventDefault();
    toggleLaunchNode(path);
  }
}

// Разбивает путь на цепочку каталогов, чтобы раскрыть дерево до текущего:
// «D:\Projects\demo» -> «D:\», «D:\Projects», «D:\Projects\demo».
function treeChain(path) {
  path = path.trim();
  const win = /^([A-Za-z]:)[\\/]/.exec(path);
  if (win) {
    const chain = [`${win[1]}\\`];
    for (const part of path.slice(2).split(/[\\/]+/).filter(Boolean)) {
      chain.push(chain[chain.length - 1].replace(/\\$/, '') + '\\' + part);
    }
    return chain;
  }
  if (path.startsWith('/')) {
    const chain = ['/'];
    for (const part of path.split('/').filter(Boolean)) {
      chain.push(chain[chain.length - 1].replace(/\/$/, '') + '/' + part);
    }
    return chain;
  }
  return [];
}

// Раскрываем дерево до каталога в поле ввода и выделяем его — как проводник,
// который открывается на текущей папке. Если пути на дереве нет, ничего не
// трогаем: человек мог набрать новый путь, который ещё не создан.
async function revealLaunchPath(path) {
  const token = ++launchTreeReveal;
  const chain = treeChain(path);
  if (!chain.length) return;
  await loadLaunchDir('');
  if (token !== launchTreeReveal || !launchTree.open) return;
  const roots = launchTree.nodes.get('')?.entries || [];
  const root = roots.find((entry) => sameTreePath(entry.path, chain[0]));
  if (!root) return;

  launchTree.expanded.add(root.path);
  let current = root.path;
  for (let i = 1; i < chain.length; i++) {
    await loadLaunchDir(current);
    if (token !== launchTreeReveal || !launchTree.open) return;
    const entries = launchTree.nodes.get(current)?.entries || [];
    const child = entries.find((entry) => sameTreePath(entry.path, chain[i]));
    if (!child) {
      renderLaunchTree();
      return;
    }
    current = child.path;
    if (i < chain.length - 1) launchTree.expanded.add(current);
  }
  if (chain.length === 1) await loadLaunchDir(current);

  launchTree.selected = current;
  launchTree.focus = current;
  renderLaunchTree();
  treeRow(current)?.scrollIntoView({ block: 'nearest' });
}

// Кнопка «Обзор…» раскрывает и прячет дерево. При раскрытии грузим корни и
// показываем в дереве каталог, набранный в поле.
function toggleLaunchTree(force) {
  const open = force === undefined ? !launchTree.open : force;
  launchTree.open = open;
  $('launch-browse').setAttribute('aria-expanded', String(open));
  if (!open) {
    launchTreeReveal += 1;
    renderLaunchTree();
    return;
  }
  syncLaunchTreeAgent();
  renderLaunchTree();
  loadLaunchDir('').then(() => {
    if (!launchTree.open) return;
    const row = treeRow(launchTree.focus || launchTree.selected) || treeRows()[0];
    row?.focus();
  });
  revealLaunchPath($('launch-cwd').value);
}

$('launch-browse').addEventListener('click', () => toggleLaunchTree());

// Набранный вручную путь: по уходу из поля дерево переезжает на него.
$('launch-cwd').addEventListener('change', () => {
  if (launchTree.open) revealLaunchPath($('launch-cwd').value);
});
