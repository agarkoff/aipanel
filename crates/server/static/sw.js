// Service worker панели: показывает push-уведомления, которые шлёт сервер,
// когда сессия ждёт ответа или закончила ход. Работает и при закрытой панели
// и заблокированном телефоне. Нажатие открывает сессию.
'use strict';

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('push', (e) => {
  let data = {};
  try {
    data = e.data ? e.data.json() : {};
  } catch {
    data = { title: 'AiPanel', body: e.data ? e.data.text() : '' };
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
      badge: '/icon-192.png',
      data: { agent: data.agent || '', session: data.session || '' },
    }),
  );
});

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
