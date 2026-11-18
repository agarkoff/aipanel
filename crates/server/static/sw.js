// Service worker панели: показывает push-уведомления, которые шлёт сервер,
// когда сессия ждёт ответа или закончила ход. Работает и при закрытой панели
// и заблокированном телефоне. Нажатие открывает сессию.
'use strict';

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) =>
  e.waitUntil(
    (async () => {
      await self.clients.claim();
      // После обновления панели на устройстве остаются плашки прежних версий:
      // сессии давно прочитаны, а убирать их вручную никто не станет. Новая
      // версия начинается с чистого списка — дальше плашки снимает открытие
      // сессии (см. `dismiss` ниже).
      const shown = await self.registration.getNotifications();
      shown.filter((n) => (n.tag || '').startsWith('aipanel')).forEach((n) => n.close());
    })(),
  ),
);

self.addEventListener('push', (e) => {
  let data = {};
  try {
    data = e.data ? e.data.json() : {};
  } catch {
    data = { title: 'AiPanel', body: e.data ? e.data.text() : '' };
  }
  // Сессию открыли — например, на компьютере: плашку пора убрать, а не показывать.
  if (data.close) {
    e.waitUntil(dismiss(data.tag || 'aipanel'));
    return;
  }
  // Показывать нужно всегда: Safari отзывает подписку, если push пришёл,
  // а уведомления не было.
  e.waitUntil(
    self.registration.showNotification(data.title || 'AiPanel', {
      body: data.body || '',
      tag: data.tag || 'aipanel',
      // Повторное событие той же сессии снова звенит, а не тихо заменяет прежнее.
      renotify: true,
      icon: '/icon-192.png',
      // Строка состояния Android рисует этот значок силуэтом по альфе: из
      // цветной плитки с белым рисунком получается сплошное пятно, поэтому
      // для неё отдельная картинка — белый контур на прозрачном фоне.
      badge: '/badge.png',
      data: { agent: data.agent || '', session: data.session || '' },
    }),
  );
});

// Убирает плашку с тегом. Push обязан показать уведомление (Chrome иначе
// пишет «сайт обновлён в фоне», Safari отзывает подписку), поэтому сначала
// ставим на её место молчаливую копию, а потом закрываем все с этим тегом.
async function dismiss(tag) {
  const reg = self.registration;
  await reg.showNotification('AiPanel', {
    body: '',
    tag,
    silent: true,
    icon: '/icon-192.png',
    badge: '/badge.png',
    data: { close: true },
  });
  const shown = await reg.getNotifications();
  shown.filter((n) => n.tag === tag).forEach((n) => n.close());
}

self.addEventListener('notificationclick', (e) => {
  e.notification.close();
  const { agent, session } = e.notification.data || {};
  e.waitUntil(
    (async () => {
      const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
      const open = windows.find((w) => new URL(w.url).origin === self.location.origin);
      if (open) {
        // Панель уже открыта: показываем её и просим открыть сессию.
        await open.focus();
        if (agent && session) open.postMessage({ t: 'open', agent, session });
        return;
      }
      const hash = agent && session ? `#${new URLSearchParams({ agent, session })}` : '';
      await self.clients.openWindow(`/${hash}`);
    })(),
  );
});
