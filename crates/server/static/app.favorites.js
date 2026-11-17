'use strict';

// Избранное: звезда в строке сообщения и плашка со списком в шапке.
// Список хранит агент, поэтому звёзды одинаковы во всех браузерах.

// ------------------------------------------------------------ избранное

// Звезда в строке сообщения: залита жёлтым, если сообщение в избранном.
// Хранится список у агента, поэтому нажатие меняет только запрос к нему, а
// звёзды обновляются по присланному списку (сразу — для отзывчивости).
function isFavorite(id) {
  return favorites.some((f) => f.id === id);
}

function setFavoriteButton(btn, on) {
  btn.classList.toggle('on', on);
  btn.title = on ? 'Убрать из избранного' : 'Добавить в избранное';
  btn.setAttribute('aria-label', btn.title);
  btn.setAttribute('aria-pressed', String(on));
}

function favoriteButton(item) {
  const btn = el('button', 'fav');
  btn.type = 'button';
  btn.append(icon([STAR]));
  setFavoriteButton(btn, isFavorite(item.id));
  btn.addEventListener('click', () => toggleFavorite(item));
  favButtons.set(item.id, btn);
  return btn;
}

function toggleFavorite(item) {
  const fav = { id: item.id, ts: item.ts || 0, role: item.role, text: item.text || '' };
  sendFavorite(fav, !isFavorite(item.id));
}

// Запрос агенту: добавить в избранное (`on`) или убрать. `fav.of` есть у
// фрагмента — это сообщение, из которого выделен текст. `true`, если ушло.
function sendFavorite(fav, on) {
  const cur = current();
  if (!cur) return false;
  const msg = {
    t: 'favorite',
    agent: cur.agent.name,
    session: cur.session.key,
    item: fav.id,
    on,
    ts: fav.ts || 0,
    role: fav.role,
    text: fav.text || '',
  };
  if (fav.of) msg.of = fav.of;
  if (!send(msg)) {
    setHint('Нет соединения с сервером.', true);
    return false;
  }
  // Звезда загорается сразу: агент пришлёт полный список следом и подтвердит его.
  const rest = favorites.filter((f) => f.id !== fav.id);
  favorites = on ? [...rest, fav] : rest;
  applyFavorites();
  return true;
}

// Список изменился: подтягиваем звёзды у всех показанных сообщений и плашку.
function applyFavorites() {
  const ids = new Set(favorites.map((f) => f.id));
  for (const [id, btn] of favButtons) setFavoriteButton(btn, ids.has(id));
  const btn = $('favorites');
  btn.title = favorites.length ? `Избранное: ${favorites.length}` : 'Избранное';
  btn.setAttribute('aria-label', btn.title);
  // Счётчик у звезды: сколько сообщений этой сессии в избранном.
  const count = $('favorites-count');
  count.hidden = !favorites.length;
  count.textContent = favorites.length > 99 ? '99+' : String(favorites.length);
  if (!$('favorites-menu').hidden) renderFavoritesMenu();
}

function setFavoritesMenu(open) {
  $('favorites-menu').hidden = !open;
  $('favorites').setAttribute('aria-expanded', String(open));
  if (open) renderFavoritesMenu();
}

// Плашка со списком: начало каждого сообщения, нажатие ведёт к нему в ленте.
function renderFavoritesMenu() {
  const box = $('favorites-list');
  if (!favorites.length) {
    box.replaceChildren(el('p', 'fav-empty', 'Пока пусто. Звезда у сообщения добавляет его сюда, а выделенный кусок текста — звезда на плашке под выделением.'));
    return;
  }
  const rows = favorites.map((f) => {
    const wrap = el('div', f.of ? 'fav-item part' : 'fav-item');
    const row = el('button', 'fav-row');
    row.type = 'button';
    const head = el('span', 'fav-row-head');
    head.append(el('span', 'fav-who', f.role === 'user' ? 'Вы' : 'Агент'));
    if (f.of) head.append(el('span', 'fav-part', 'фрагмент'));
    if (f.ts) head.append(el('span', 'fav-when', new Date(f.ts).toLocaleString()));
    row.append(head, el('span', 'fav-preview', firstLine(f.text || '') || 'Сообщение'));
    row.title = f.text || 'Сообщение';
    row.addEventListener('click', () => {
      setFavoritesMenu(false);
      if (f.of) gotoFragment(f);
      else gotoMessage(f.id);
    });
    // Убрать из списка можно прямо здесь: у фрагмента нет своей звезды в ленте.
    const remove = el('button', 'fav-remove');
    remove.type = 'button';
    remove.title = 'Убрать из избранного';
    remove.setAttribute('aria-label', remove.title);
    remove.append(icon(['M4.5 4.5l7 7', 'M11.5 4.5l-7 7']));
    remove.addEventListener('click', (e) => {
      // Список перерисуется, и кнопка выпадет из документа: без этого общий
      // обработчик решил бы, что нажали мимо плашки, и закрыл её.
      e.stopPropagation();
      sendFavorite(f, false);
    });
    wrap.append(row, remove);
    return wrap;
  });
  box.replaceChildren(...rows);
}

// Переход к сообщению по id: прокрутка и подсветка, как у закрепа. Если строки
// в ленте нет (сообщение старше загруженного), так и говорим.
function gotoMessage(id) {
  const node = nodes.get(id);
  if (!node) {
    setHint('Это сообщение не загружено в ленту: прокрутите её вверх — ранние сообщения подгрузятся.', true);
    return false;
  }
  jumpToNode(node);
  return true;
}

// Переход к фрагменту: к его сообщению, а сам кусок текста подсвечивается.
function gotoFragment(f) {
  if (!gotoMessage(f.of)) return;
  const range = findText(nodes.get(f.of), f.text || '');
  if (!range) return;
  markFragment(range);
  // Длинное сообщение: кусок может быть далеко от его начала — едем к нему.
  const feed = $('feed');
  const box = feed.getBoundingClientRect();
  const rect = range.getBoundingClientRect();
  const node = nodes.get(f.of).getBoundingClientRect();
  if (rect.top - node.top > feed.clientHeight * 0.5) {
    const top = feed.scrollTop + rect.top - box.top - Math.min(120, feed.clientHeight * 0.25);
    feed.scrollTo({ top: Math.max(0, top), behavior: 'smooth' });
  }
}

// Ищет текст в теле сообщения без учёта пробелов и переводов строк: выделение
// при копировании вставляет переводы строк между абзацами, которых нет в DOM.
// Возвращает Range или null. Текст фрагмента мог быть обрезан до «…».
function findText(node, text) {
  const body = node && node.querySelector('.body');
  const want = text.replace(/…$/, '').replace(/\s+/g, '');
  if (!body || !want) return null;
  const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
  const chars = [];
  const where = [];
  for (let t = walker.nextNode(); t; t = walker.nextNode()) {
    const s = t.nodeValue;
    for (let i = 0; i < s.length; i++) {
      if (/\s/.test(s[i])) continue;
      chars.push(s[i]);
      where.push([t, i]);
    }
  }
  const at = chars.join('').indexOf(want);
  if (at < 0) return null;
  const range = document.createRange();
  const [startNode, startOff] = where[at];
  const [endNode, endOff] = where[at + want.length - 1];
  range.setStart(startNode, startOff);
  range.setEnd(endNode, endOff + 1);
  return range;
}

// Подсветка куска текста без правки DOM (CSS Custom Highlight API). Где его
// нет, остаётся подсветка всего сообщения. Конструктор берётся у window: имя
// `Highlight` в скриптах панели занято подсветкой кода из hl.js.
let fragmentTimer = 0;
function markFragment(range) {
  const Mark = window.Highlight;
  if (!window.CSS || !CSS.highlights || typeof Mark !== 'function') return;
  CSS.highlights.set('fav-part', new Mark(range));
  clearTimeout(fragmentTimer);
  fragmentTimer = setTimeout(() => CSS.highlights.delete('fav-part'), 2600);
}

$('favorites').addEventListener('click', () => setFavoritesMenu($('favorites-menu').hidden));
document.addEventListener('click', (e) => {
  if ($('favorites-menu').hidden) return;
  if (!e.target.closest('#favorites-menu, #favorites')) setFavoritesMenu(false);
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !$('favorites-menu').hidden) {
    setFavoritesMenu(false);
    $('favorites').focus();
  }
});

// ------------------------------------------------------------ выделенный текст

// Плашка под выделением в ленте: скопировать кусок или отложить его в
// избранное. В избранное — только кусок одного сообщения (своего или ответа
// агента); копировать можно что угодно из ленты.
const selBar = el('div', 'sel-bar');
selBar.hidden = true;
selBar.setAttribute('role', 'toolbar');
selBar.setAttribute('aria-label', 'Выделенный текст');
const selCopy = el('button', 'sel-copy');
selCopy.type = 'button';
selCopy.title = 'Скопировать выделенное';
selCopy.setAttribute('aria-label', selCopy.title);
const selCopyIcon = icon([
  'M7 5.5h5A1.5 1.5 0 0 1 13.5 7v5a1.5 1.5 0 0 1-1.5 1.5H7A1.5 1.5 0 0 1 5.5 12V7A1.5 1.5 0 0 1 7 5.5Z',
  'M10.5 3.5V3A1.5 1.5 0 0 0 9 1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5',
]);
selCopy.append(selCopyIcon, el('span', '', 'Копировать'));
const selFav = el('button', 'sel-fav');
selFav.type = 'button';
selFav.append(icon([STAR]), el('span', '', 'В избранное'));
selBar.append(selCopy, selFav);
document.body.append(selBar);

let selState = null; // { text, msg: узел сообщения или null }
let selTimer = 0;
let selDoneTimer = 0;
let selPointer = false; // кнопка мыши зажата: выделение ещё тянут

// Фрагмент, уже лежащий в избранном: тот же текст из того же сообщения.
function savedFragment(msgId, text) {
  return favorites.find((f) => f.of === msgId && f.text === text);
}

// Выделение в ленте или null. Сообщение — если весь кусок внутри тела одного
// сообщения, которое можно положить в избранное.
function feedSelection() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount) return null;
  const range = sel.getRangeAt(0);
  const feed = $('feed');
  if (!feed.contains(range.commonAncestorContainer)) return null;
  const text = sel.toString().trim();
  if (!text) return null;
  const elementOf = (n) => (n.nodeType === Node.ELEMENT_NODE ? n : n.parentElement);
  const start = elementOf(range.startContainer);
  const end = elementOf(range.endContainer);
  const body = start && start.closest('.body');
  let msg = body && end && end.closest('.body') === body ? body.closest('.item') : null;
  if (msg && !msg.matches('.item.text.user, .item.text.assistant')) msg = null;
  return { text, range, msg };
}

function hideSelBar() {
  clearTimeout(selDoneTimer);
  selBar.hidden = true;
  selBar.classList.remove('done');
  selState = null;
}

function updateSelBar() {
  if (selPointer) return;
  const info = feedSelection();
  if (!info) {
    hideSelBar();
    return;
  }
  const same = selState && !selBar.hidden && selState.text === info.text && selState.msg === info.msg;
  selState = { text: info.text, msg: info.msg };
  if (!same) {
    clearTimeout(selDoneTimer);
    selBar.classList.remove('done');
    selCopy.replaceChildren(selCopyIcon, el('span', '', 'Копировать'));
  }
  selFav.hidden = !info.msg;
  if (info.msg) {
    const on = !!savedFragment(info.msg.dataset.id, info.text);
    selFav.classList.toggle('on', on);
    selFav.title = on ? 'Убрать фрагмент из избранного' : 'Добавить выделенное в избранное';
    selFav.setAttribute('aria-label', selFav.title);
    selFav.setAttribute('aria-pressed', String(on));
    selFav.lastChild.textContent = on ? 'В избранном' : 'В избранное';
  }
  selBar.hidden = false;
  placeSelBar(info.range);
}

// Плашка — под концом выделения, а если внизу ленты места нет — над его
// началом. На телефоне под выделением ручки и системное меню, поэтому отступ больше.
function placeSelBar(range) {
  const rects = [...range.getClientRects()].filter((r) => r.width || r.height);
  const whole = range.getBoundingClientRect();
  const first = rects[0] || whole;
  const last = rects[rects.length - 1] || whole;
  const feed = $('feed').getBoundingClientRect();
  const w = selBar.offsetWidth;
  const h = selBar.offsetHeight;
  const gap = matchMedia('(pointer: coarse)').matches ? 30 : 8;
  const floor = Math.min(window.innerHeight, feed.bottom) - 6;
  let top = last.bottom + gap;
  if (top + h > floor) {
    const above = first.top - gap - h;
    // Выделение длиннее экрана: тогда плашка просто прижимается к низу ленты.
    top = above >= feed.top + 6 ? above : floor - h;
  }
  top = Math.max(feed.top + 6, Math.min(top, floor - h));
  const left = Math.max(8, Math.min(last.right - w / 2, window.innerWidth - w - 8));
  selBar.style.top = `${Math.round(top)}px`;
  selBar.style.left = `${Math.round(left)}px`;
}

function scheduleSelBar() {
  clearTimeout(selTimer);
  selTimer = setTimeout(updateSelBar, 120);
}

document.addEventListener('selectionchange', scheduleSelBar);
$('feed').addEventListener('pointerdown', (e) => {
  if (e.pointerType === 'mouse' && e.button === 0) selPointer = true;
});
document.addEventListener('pointerup', () => {
  if (!selPointer) return;
  selPointer = false;
  scheduleSelBar();
});
$('feed').addEventListener('scroll', () => {
  if (selBar.hidden) return;
  const info = feedSelection();
  if (info) placeSelBar(info.range);
}, { passive: true });
window.addEventListener('resize', () => {
  if (!selBar.hidden) updateSelBar();
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !selBar.hidden) hideSelBar();
});
// Нажатие на плашку не должно снимать выделение (мышью — иначе пропадёт до click).
selBar.addEventListener('pointerdown', (e) => e.preventDefault());
selBar.addEventListener('mousedown', (e) => e.preventDefault());

selCopy.addEventListener('click', async () => {
  if (!selState) return;
  const text = selState.text;
  const ok = await copyToClipboard(text);
  if (!ok) {
    setHint('Не удалось скопировать текст.', true);
    return;
  }
  selCopy.replaceChildren(icon(['M3.5 8.5 6.5 11.5 12.5 5']), el('span', '', 'Скопировано'));
  selBar.classList.add('done');
  clearTimeout(selDoneTimer);
  selDoneTimer = setTimeout(hideSelBar, 1200);
});

selFav.addEventListener('click', () => {
  if (!selState || !selState.msg) return;
  const { text, msg } = selState;
  const msgId = msg.dataset.id;
  const saved = savedFragment(msgId, text);
  const role = msg.classList.contains('user') ? 'user' : 'assistant';
  const fav = saved || {
    id: `q:${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`,
    of: msgId,
    ts: Number(msg.dataset.ts) || 0,
    role,
    text,
  };
  if (!sendFavorite(fav, !saved)) return;
  // Отложили — выделение больше не нужно; плашка гаснет вместе с ним, а
  // подтверждение — счётчик у звезды в шапке, как и для целого сообщения.
  window.getSelection().removeAllRanges();
  hideSelBar();
});

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
