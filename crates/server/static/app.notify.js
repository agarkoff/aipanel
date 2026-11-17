'use strict';

// Браузерные уведомления: конец хода и просьба выбрать вариант. Панель
// обычно открыта в фоне, поэтому это единственный способ позвать человека.

// ------------------------------------------------------------ уведомления

// Панель обычно открыта в фоне, а ход идёт минутами. Браузерное уведомление
// сообщает о конце хода и о сессии, которая ждёт выбора, чтобы ответ не
// прождали впустую. Нажатие на уведомление открывает эту сессию.
const BUSY_STATUSES = new Set(['busy', 'shell', 'stalled']);
// Тот же признак, что у зелёного кружка в списке: ход кончился, сессия свободна.
const DONE_STATUSES = new Set(['idle', 'ready']);
const sessionStatuses = new Map(); // "агент\nсессия" -> прошлый статус
let statusesSeeded = false; // первый список — точка отсчёта, по нему не уведомляем

function notifySupported() {
  return typeof Notification !== 'undefined';
}

// Уведомления страницы по тегу: созданные объекты перечислить нельзя, а
// закрыть уведомление открытой сессии нужно именно по ссылке.
const pageNotices = new Map(); // tag -> Notification

const noticeTag = (agent, session) => `aipanel:${agent}:${session}`;

// Убирает уведомления сессии из строки состояния: открывать её человек может
// и из списка, не нажимая на плашку. Push-уведомления живут у service worker
// (sw.js) и закрываются только через его регистрацию.
function closeNotice(agent, session) {
  const tag = noticeTag(agent, session);
  const own = pageNotices.get(tag);
  if (own) {
    own.close();
    pageNotices.delete(tag);
  }
  if (!('serviceWorker' in navigator)) return;
  navigator.serviceWorker.ready
    .then((reg) => reg.getNotifications())
    // Тег фильтруем сами: не все браузеры принимают его в параметрах.
    .then((list) => list.filter((n) => n.tag === tag).forEach((n) => n.close()))
    // Уведомлений нет или браузер не умеет их искать — гасить нечего.
    .catch(() => {});
}

// Разрешение на уведомления спрашивает только колокольчик в шапке
// (app.push.js): Safari разрешает спросить лишь по явному действию.
function showNotice(agent, session, kind) {
  if (!notifySupported() || Notification.permission !== 'granted') return;
  // Это устройство подписано на push: то же уведомление пришлёт сервер.
  if (pushSub) return;
  // Пользователь смотрит именно эту сессию — уведомление ему не нужно.
  const open = selected && selected.agent === agent.name && selected.session === session.key;
  if (open && document.visibilityState === 'visible') return;
  const tag = noticeTag(agent.name, session.key);
  let n;
  try {
    n = new Notification(session.title || session.key, {
      body: kind === 'waiting' ? `${agent.name} · сессия ждёт ответа` : `${agent.name} · ход завершён`,
      // Одно уведомление на сессию: новое заменяет прежнее, а не копится стопкой.
      tag,
      icon: '/icon-192.png',
    });
  } catch {
    // На телефоне уведомление строит только service worker: без него молчим.
    return;
  }
  pageNotices.set(tag, n);
  n.onclick = () => {
    window.focus();
    select(agent.name, session.key);
    n.close();
    pageNotices.delete(tag);
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
