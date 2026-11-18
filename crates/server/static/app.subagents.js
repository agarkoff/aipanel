'use strict';

// Подагенты открытой сессии: их вызовы в основную ленту не попадают (агент их
// прячет), поэтому список над строкой хода — единственный вход в ленту
// подагента. Открытый подагент живёт вкладкой в блоке ленты: вкладки
// переключаются, а прокрутка и раскрытые вызовы каждой запоминаются.

// Полный список подагентов от агента: он заменяет показанный.
function applySubagents(list) {
  subagents = Array.isArray(list) ? list : [];
  subagentsKnown = true;
  // Сессию могли перезапустить: вкладку, которой больше нет в списке, открыть
  // заново нечем.
  const known = new Set(subagents.map((s) => s.id));
  for (const id of subTabs.slice()) {
    if (!known.has(id)) closeSubTab(id);
  }
  renderSubagents();
  renderSubagentTabs();
  // Ссылка вела на подагента: теперь видно, есть ли он в этой сессии.
  tryPendingSub();
}

function subagentById(id) {
  return subagents.find((s) => s.id === id) || null;
}

function subagentTitle(sub) {
  const title = (sub && sub.title) || (sub && sub.id) || '';
  const line = title.split('\n', 1)[0];
  return line.length > 120 ? `${line.slice(0, 120)}…` : line;
}

// Строка над счётчиком хода: работающие и завершённые подагенты. Нажатие
// открывает его ленту вкладкой, повторное — возвращает к ней.
function renderSubagents() {
  const box = $('subagents');
  if (!selected || !subagents.length) {
    box.hidden = true;
    box.replaceChildren();
    return;
  }
  box.hidden = false;
  box.replaceChildren(
    ...subagents.map((sub) => {
      const chip = el('button', sub.running ? 'subagent running' : 'subagent');
      chip.type = 'button';
      chip.dataset.sub = sub.id;
      if (subTab === sub.id) chip.classList.add('active');
      const title = subagentTitle(sub) || 'Подагент';
      chip.append(el('span', 'subagent-dot'), el('span', 'subagent-title', title));
      if (sub.agent && sub.agent !== 'general-purpose') chip.append(el('span', 'subagent-kind', sub.agent));
      chip.title = [
        title,
        sub.agent ? `агент: ${sub.agent}` : '',
        sub.running ? 'работает' : 'завершён',
      ]
        .filter(Boolean)
        .join('\n');
      chip.setAttribute('aria-label', `Открыть ленту подагента: ${title}`);
      chip.addEventListener('click', () => openSubTab(sub.id));
      return chip;
    }),
  );
}

// Вкладки в блоке ленты: основная сессия и открытые подагенты. Появляются,
// только когда открыт хотя бы один подагент.
function renderSubagentTabs() {
  const bar = $('feed-tabs');
  if (!selected || !subTabs.length) {
    bar.hidden = true;
    bar.replaceChildren();
    return;
  }
  bar.hidden = false;
  const list = [{ id: '', title: 'Сессия', running: false }];
  for (const id of subTabs) {
    const sub = subagentById(id);
    list.push({ id, title: subagentTitle(sub) || 'Подагент', running: Boolean(sub && sub.running) });
  }
  bar.replaceChildren(
    ...list.map((tab) => {
      const box = el(
        'div',
        `feed-tab${subTab === tab.id ? ' active' : ''}${tab.running ? ' running' : ''}`,
      );
      const btn = el('button', 'feed-tab-btn', tab.title);
      btn.type = 'button';
      btn.title = tab.title;
      btn.setAttribute('role', 'tab');
      btn.setAttribute('aria-selected', subTab === tab.id ? 'true' : 'false');
      btn.addEventListener('click', () => switchFeed(tab.id));
      box.append(btn);
      if (tab.id) {
        const close = el('button', 'feed-tab-close');
        close.type = 'button';
        close.title = 'Закрыть вкладку';
        close.setAttribute('aria-label', `Закрыть вкладку «${tab.title}»`);
        close.append(icon(['M4 4l8 8', 'M12 4l-8 8']));
        close.addEventListener('click', (e) => {
          e.stopPropagation();
          closeSubTab(tab.id);
        });
        box.append(close);
      }
      return box;
    }),
  );
}

// Открывает ленту подагента из ссылки. Список подагентов приходит от агента
// позже самой сессии, поэтому здесь запоминается, что открыть, когда он придёт.
function openLinkedSub(id) {
  if (!id || !selected || subTab === id) return;
  pendingSub = id;
  tryPendingSub();
}

function tryPendingSub() {
  if (!pendingSub || !selected) return;
  const id = pendingSub;
  if (!subagentById(id)) {
    // Список уже пришёл, а подагента в нём нет: ссылка устарела. Остаёмся
    // в основной ленте и убираем `sub` из адреса.
    if (subagentsKnown) {
      pendingSub = '';
      updateSessionUrl();
    }
    return;
  }
  pendingSub = '';
  if (subTab === id) {
    renderSubagents();
    renderSubagentTabs();
    return;
  }
  switchFeed(id);
}

// Открывает ленту подагента вкладкой (или переключается на уже открытую).
function openSubTab(id) {
  if (!selected || !subagentById(id)) return;
  if (subTab === id) {
    renderSubagents();
    renderSubagentTabs();
    return;
  }
  switchFeed(id);
}

// Переключение вкладки: подписка агента на ленту и сборка DOM из кэша.
function switchFeed(id) {
  if (!selected || id === subTab) return;
  // Уходим с основной ленты — её место сохраняем сейчас, пока оно ещё в DOM.
  if (!subTab) flushFeedPlace();
  const feed = $('feed');
  tabScroll.set(subTab, feed.scrollTop);
  tabOpen.set(subTab, openItems());
  const msg = { agent: selected.agent, session: selected.session };
  if (id) {
    if (!subTabs.includes(id)) subTabs.push(id);
    send({ t: 'watch_sub', ...msg, sub: id });
  } else if (subTab) {
    send({ t: 'unwatch_sub', ...msg, sub: subTab });
  }
  showTab(id);
}

// Показывает вкладку `id` из кэша. Подписку на ленту переключает вызывающий.
function showTab(id) {
  subTab = id;
  // Адрес показывает открытую ленту: у подагента своя ссылка с `sub`.
  updateSessionUrl();
  const feed = $('feed');
  const key = tabKey(id);
  const cached = feedItems.get(key);
  renderFeed(key, Boolean(id) && !(cached && cached.size));
  restoreOpen(tabOpen.get(id));
  const saved = tabScroll.get(id);
  if (!id && feedRestore) {
    // Основную ленту ещё не поставили на сохранённое место (её собирали, пока
    // была открыта вкладка подагента): место важнее того, что запомнила вкладка.
    feedRestore.ready = true;
    applyFeedRestore();
  } else {
    feed.scrollTop = saved === undefined ? feed.scrollHeight : Math.min(saved, feed.scrollHeight);
    // Куда встала лента, то и решает, следует ли она теперь за концом: у
    // вкладки своё место чтения, и присвоение `scrollTop` события прокрутки
    // может не дать.
    followEnd = placeAtEnd();
  }
  // Закреп относится к основной ленте: у подагента его нет.
  if (!id) syncPin();
  renderSubagents();
  renderSubagentTabs();
  // В ленту подагента сообщение не отправить: поле ввода относится к сессии.
  renderHeader();
}

// Закрывает вкладку подагента. Активную заменяет соседняя (или основная лента).
function closeSubTab(id) {
  const at = subTabs.indexOf(id);
  if (at < 0) return;
  subTabs.splice(at, 1);
  cacheReset(tabKey(id));
  tabScroll.delete(id);
  tabOpen.delete(id);
  if (subTab !== id) {
    renderSubagentTabs();
    renderSubagents();
    return;
  }
  const next = subTabs[Math.min(at, subTabs.length - 1)] || '';
  if (selected) {
    const msg = { agent: selected.agent, session: selected.session };
    send({ t: 'unwatch_sub', ...msg, sub: id });
    if (next) send({ t: 'watch_sub', ...msg, sub: next });
  }
  // Лента закрытой вкладки больше не наша: уходим с неё, не отправляя отмену
  // повторно (она уже ушла выше).
  subTab = '';
  showTab(next);
}

// Лента подагента пришла от агента: элементы копятся в кэше, а в DOM попадают,
// пока открыта его вкладка.
function applySubHistory(msg) {
  const key = tabKey(msg.sub);
  if (msg.reset) cacheReset(key);
  for (const item of (msg.items || []).map(plainItem)) cachePut(key, item);
  if (msg.error && (msg.reset || (msg.items || []).length)) {
    setHint(`Лента подагента: ${msg.error}`, true);
  }
  if (subTab !== msg.sub) return;
  const feed = $('feed');
  // Как и в основной ленте: в конец лента уезжает, только пока человек у низа.
  // Пересборка (переподключение) не должна выдёргивать читающего в самый конец.
  const stick = followEnd;
  const place = msg.reset && !stick ? feedPlace() : null;
  const open = openItems();
  renderFeed(key);
  restoreOpen(open);
  if (stick) {
    feed.scrollTop = feed.scrollHeight;
    followEnd = true;
  } else if (place) {
    applyFeedPlace(place);
    followEnd = placeAtEnd();
  }
  updateToEnd();
}

// Сессию закрыли или открыли другую: подагенты и их вкладки к ней не относятся.
function resetSubagentView() {
  subagents = [];
  subagentsKnown = false;
  pendingSub = '';
  subTabs = [];
  subTab = '';
  cacheReset('');
  for (const key of [...feedItems.keys()]) {
    if (!key) continue;
    feedItems.delete(key);
    feedOrder.delete(key);
  }
  tabScroll.clear();
  tabOpen.clear();
  renderSubagents();
  renderSubagentTabs();
}
