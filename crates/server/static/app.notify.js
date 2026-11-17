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

// Разрешение на уведомления спрашивает только колокольчик в шапке
// (app.push.js): Safari разрешает спросить лишь по явному действию.
function showNotice(agent, session, kind) {
  if (!notifySupported() || Notification.permission !== 'granted') return;
  // Это устройство подписано на push: то же уведомление пришлёт сервер.
  if (pushSub) return;
  // Пользователь смотрит именно эту сессию — уведомление ему не нужно.
  const open = selected && selected.agent === agent.name && selected.session === session.key;
  if (open && document.visibilityState === 'visible') return;
  let n;
  try {
    n = new Notification(session.title || session.key, {
      body: kind === 'waiting' ? `${agent.name} · сессия ждёт ответа` : `${agent.name} · ход завершён`,
      // Одно уведомление на сессию: новое заменяет прежнее, а не копится стопкой.
      tag: `aipanel:${agent.name}:${session.key}`,
      icon: '/icon-192.png',
    });
  } catch {
    // На телефоне уведомление строит только service worker: без него молчим.
    return;
  }
  n.onclick = () => {
    window.focus();
    select(agent.name, session.key);
    n.close();
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
