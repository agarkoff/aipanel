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
  const cur = current();
  if (!cur) return;
  const on = !isFavorite(item.id);
  const fav = { id: item.id, ts: item.ts || 0, role: item.role, text: item.text || '' };
  if (!send({
    t: 'favorite',
    agent: cur.agent.name,
    session: cur.session.key,
    item: fav.id,
    on,
    ts: fav.ts,
    role: fav.role,
    text: fav.text,
  })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  // Звезда загорается сразу: агент пришлёт полный список следом и подтвердит его.
  favorites = on ? [...favorites, fav] : favorites.filter((f) => f.id !== fav.id);
  applyFavorites();
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
    box.replaceChildren(el('p', 'fav-empty', 'Пока пусто. Звезда у сообщения добавляет его сюда.'));
    return;
  }
  const rows = favorites.map((f) => {
    const row = el('button', 'fav-row');
    row.type = 'button';
    const head = el('span', 'fav-row-head');
    head.append(el('span', 'fav-who', f.role === 'user' ? 'Вы' : 'Агент'));
    if (f.ts) head.append(el('span', 'fav-when', new Date(f.ts).toLocaleString()));
    row.append(head, el('span', 'fav-preview', firstLine(f.text || '') || 'Сообщение'));
    row.title = f.text || 'Сообщение';
    row.addEventListener('click', () => {
      setFavoritesMenu(false);
      gotoMessage(f.id);
    });
    return row;
  });
  box.replaceChildren(...rows);
}

// Переход к сообщению по id: прокрутка и подсветка, как у закрепа. Если строки
// в ленте нет (сообщение старше загруженного), так и говорим.
function gotoMessage(id) {
  const node = nodes.get(id);
  if (!node) {
    setHint('Это сообщение не загружено в ленту: прокрутите её вверх — ранние сообщения подгрузятся.', true);
    return;
  }
  jumpToNode(node);
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
