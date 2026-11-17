// Push-уведомления: колокольчик в шапке списка подписывает это устройство,
// service worker (sw.js) показывает уведомления, которые шлёт сервер.
'use strict';

// Колокольчик в шапке списка подписывает это устройство на push-уведомления:
// их шлёт сервер, поэтому они приходят и при закрытой панели. Разрешение
// браузера спрашивается только по нажатию на колокольчик.
let pushSub = null; // подписка этого устройства (PushSubscription) или null
let pushBusy = false;
const pushReplies = new Map(); // id команды -> обработчик ответа сервера

function pushCapable() {
  return 'serviceWorker' in navigator && 'PushManager' in window && notifySupported();
}

// iPhone и iPad (новые iPad представляются Mac с сенсорным экраном).
function isIOS() {
  return /iPhone|iPad|iPod/.test(navigator.userAgent)
    || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
}

// Панель открыта как установленное приложение (с экрана «Домой»).
function isStandalone() {
  return navigator.standalone === true || matchMedia('(display-mode: standalone)').matches;
}

function b64urlBytes(text) {
  const b64 = text.replace(/-/g, '+').replace(/_/g, '/');
  const raw = atob(b64 + '='.repeat((4 - (b64.length % 4)) % 4));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

function sameKey(buffer, key) {
  if (!buffer) return false;
  const a = new Uint8Array(buffer);
  const b = b64urlBytes(key);
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

// Подписка в виде, который ждёт сервер.
function pushSubJson(sub) {
  const json = sub.toJSON();
  return { endpoint: json.endpoint, p256dh: json.keys.p256dh, auth: json.keys.auth };
}

function sendPush(msg, done) {
  const id = String(++cmdSeq);
  if (!send({ ...msg, id })) {
    if (done) done({ ok: false, error: 'нет соединения с сервером' });
    return;
  }
  if (done) pushReplies.set(id, done);
}

async function initPush() {
  const btn = $('push');
  const key = who && who.push_key;
  // iPhone в обычном Safari: подписаться нельзя, но колокольчик подскажет, как.
  const iosTab = isIOS() && !isStandalone();
  btn.hidden = !key || (!pushCapable() && !iosTab);
  if (btn.hidden || !pushCapable()) {
    renderPush();
    return;
  }
  try {
    const reg = await navigator.serviceWorker.register('/sw.js');
    let sub = await reg.pushManager.getSubscription();
    // Ключ сервера сменился: старая подписка больше не работает.
    if (sub && !sameKey(sub.options && sub.options.applicationServerKey, key)) {
      await sub.unsubscribe().catch(() => {});
      sub = null;
    }
    pushSub = sub;
  } catch (e) {
    console.error('push:', e);
  }
  // Сервер мог потерять подписку (у агента удалили файл): напоминаем ему.
  if (pushSub) sendPush({ t: 'push_subscribe', sub: pushSubJson(pushSub) });
  renderPush();
}

function renderPush() {
  const btn = $('push');
  const on = !!pushSub && notifySupported() && Notification.permission === 'granted';
  btn.classList.toggle('on', on);
  btn.setAttribute('aria-pressed', String(on));
  btn.disabled = pushBusy;
  btn.title = on
    ? 'Уведомления включены на этом устройстве. Нажмите, чтобы выключить'
    : 'Включить уведомления на этом устройстве: сессия ждёт ответа или закончила ход';
}

$('push').addEventListener('click', async () => {
  if (pushBusy) return;
  if (isIOS() && !isStandalone()) {
    setHint('На iPhone уведомления работают в установленной панели: «Поделиться» → «На экран Домой», затем откройте её оттуда и нажмите колокольчик.');
    return;
  }
  if (!pushCapable()) return;
  if (Notification.permission === 'denied') {
    setHint('Уведомления запрещены в настройках браузера для этого сайта. Разрешите их там и нажмите колокольчик снова.', true);
    return;
  }
  pushBusy = true;
  renderPush();
  try {
    if (pushSub) await pushOff();
    else await pushOn();
  } finally {
    pushBusy = false;
    renderPush();
  }
});

async function pushOn() {
  // Разрешение — первым делом: Safari даёт спросить только сразу по нажатию.
  const permission = await Notification.requestPermission();
  if (permission !== 'granted') {
    setHint('Уведомления не разрешены.', true);
    return;
  }
  let sub;
  try {
    const reg = await navigator.serviceWorker.register('/sw.js');
    await navigator.serviceWorker.ready;
    sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64urlBytes(who.push_key) });
  } catch (e) {
    setHint(`Не удалось подписаться на уведомления: ${e.message || e}`, true);
    return;
  }
  const reply = await new Promise((done) => sendPush({ t: 'push_subscribe', sub: pushSubJson(sub) }, done));
  if (!reply.ok) {
    await sub.unsubscribe().catch(() => {});
    setHint(`Уведомления не включены: ${reply.error || 'ошибка сервера'}`, true);
    return;
  }
  pushSub = sub;
  setHint('Уведомления включены на этом устройстве.');
}

async function pushOff() {
  const endpoint = pushSub.endpoint;
  await pushSub.unsubscribe().catch(() => {});
  pushSub = null;
  sendPush({ t: 'push_unsubscribe', endpoint });
  setHint('Уведомления на этом устройстве выключены.');
}

// Ответ сервера на подписку; true — ответ был для неё.
function onPushResult(msg) {
  const done = pushReplies.get(msg.id);
  if (!done) return false;
  pushReplies.delete(msg.id);
  done(msg);
  return true;
}

// Сессия, которую нужно открыть по нажатию на уведомление: из адреса
// (`#agent=…&session=…`, панель была закрыта) или от service worker.
let pendingOpen = null;

function takeOpenFromHash() {
  const params = new URLSearchParams(location.hash.slice(1));
  const agent = params.get('agent');
  const session = params.get('session');
  if (!agent || !session) return;
  pendingOpen = { agent, session };
  history.replaceState(history.state, '', location.pathname + location.search);
}

function tryPendingOpen() {
  if (!pendingOpen) return;
  const a = agents.find((x) => x.name === pendingOpen.agent);
  if (!a || !a.sessions.some((s) => s.key === pendingOpen.session)) return;
  const { agent, session } = pendingOpen;
  pendingOpen = null;
  select(agent, session);
}

if ('serviceWorker' in navigator) {
  navigator.serviceWorker.addEventListener('message', (e) => {
    const data = e.data || {};
    if (data.t !== 'open' || !data.agent || !data.session) return;
    pendingOpen = { agent: data.agent, session: data.session };
    tryPendingOpen();
  });
}
