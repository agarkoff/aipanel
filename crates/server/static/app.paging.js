'use strict';

// Прокрутка ленты: догрузка ранних сообщений, закреп за своим сообщением
// и кнопка «в конец».

// ------------------------------------------------------------ ранние сообщения

// Сколько страниц подряд нажатие на закреп просит, пока не дойдёт до
// предыдущего своего сообщения. Обычно хватает одной; предел — страховка от
// бесконечной догрузки, если своих сообщений в ранних нет вовсе.
const PIN_OLDER_MAX = 20;

// Кнопка догрузки живёт первой строкой ленты: очистка ленты выбрасывает её из
// DOM, поэтому возвращаем на место перед каждым новым элементом.
function mountOlder() {
  const feed = $('feed');
  if (feed.firstChild !== olderBtn) feed.prepend(olderBtn);
}

function updateOlder() {
  olderBtn.hidden = !olderMore || !nodes.size;
  olderBtn.disabled = olderPending;
  olderBtn.textContent = olderPending ? 'Загрузка…' : 'Загрузить ранние сообщения';
}

function resetOlder() {
  clearTimeout(olderTimer);
  olderPending = false;
  pinOlderFrom = null;
  pinOlderPages = 0;
  // Пока агент не сказал, что перед лентой что-то есть, кнопки не показываем.
  olderMore = false;
  mountOlder();
  updateOlder();
}

// `true` — запрос ушёл (или ответ на такой же уже ждём); `false` — грузить
// нечего или соединения нет. По этому ответу нажатие закрепа понимает,
// дождётся ли оно догрузки, или идти обычным путём.
function askOlder() {
  if (subTab || !selected || !olderMore || !nodes.size) return false;
  if (olderPending) return true;
  if (!send({ t: 'older', agent: selected.agent, session: selected.session })) return false;
  olderPending = true;
  updateOlder();
  // Старая версия агента команду не знает и ответа не пришлёт: не оставляем
  // кнопку навсегда в «Загрузке».
  clearTimeout(olderTimer);
  olderTimer = setTimeout(() => {
    olderPending = false;
    // Ждать догрузку больше некому: нажатие на закреп идёт обычным путём.
    pinOlderFrom = null;
    updateOlder();
    setHint('Ранние сообщения не загрузились: агент не ответил.', true);
  }, 10_000);
  return true;
}

// Прокрутка до верхней кромки сама просит более ранние сообщения: доводить до
// них отдельным нажатием кнопки не нужно. Кнопка остаётся для тех случаев,
// когда догрузку не вышло запустить прокруткой.
function askOlderAtTop() {
  if (subTab || !olderMore || olderPending || !nodes.size) return;
  if ($('feed').scrollTop > 0) return;
  askOlder();
}

olderBtn.addEventListener('click', askOlder);

// Ранние сообщения пришли: ставим их в начало ленты, не сдвигая то место,
// которое человек читает.
function applyOlder(msg) {
  clearTimeout(olderTimer);
  olderPending = false;
  if (msg.error) {
    pinOlderFrom = null;
    updateOlder();
    setHint(`Ранние сообщения не загрузились: ${msg.error}`, true);
    return;
  }
  olderMore = msg.more !== false;
  msg.items = (msg.items || []).map(plainItem);
  cachePrepend('', msg.items || []);
  // Пока догрузка шла, могли открыть вкладку подагента: тогда пополняем только
  // кэш — соберётся при возврате в основную ленту.
  if (subTab) return;
  mountOlder();
  const feed = $('feed');
  const pinned = myMessages()[pinIdx] || null;
  // Держимся за первый элемент ленты: после вставки выше он же должен остаться
  // на том же месте экрана. Браузер сам подтягивает прокрутку (scroll anchoring),
  // но не везде и не всегда — остаток доводим вручную.
  const anchor = olderBtn.nextElementSibling;
  const anchorTop = anchor ? anchor.getBoundingClientRect().top : 0;
  const frag = document.createDocumentFragment();
  // Результаты вызовов сопоставляются между собой внутри старой части ленты:
  // очередь вызовов новой части им не достаётся.
  const savedCalls = pendingCalls;
  pendingCalls = [];
  for (const item of msg.items || []) {
    // Такое сообщение уже показано: страницы не пересекаются, но агент может
    // ответить повторно на второй запрос.
    if (nodes.has(item.id)) continue;
    rememberRequest(item);
    placeItem(item, frag);
  }
  pendingCalls = savedCalls;
  olderBtn.after(frag);
  if (anchor) {
    const shift = anchor.getBoundingClientRect().top - anchorTop;
    if (shift) feed.scrollTop += shift;
  }
  // Закреп остаётся на том же сообщении, но его номер среди своих сдвинулся.
  const list = myMessages();
  pinIdx = pinned ? list.indexOf(pinned) : list.length - 1;
  updateOlder();
  syncPin();
  updateToEnd();
  // Продолжение нажатия на самый старый закреп: догруженное могло принести
  // предыдущее своё сообщение.
  if (pinOlderFrom) continuePinOlder(list);
  // Прокрутка открытой сессии ждёт более ранние сообщения: без этого шага она
  // осталась бы наверху, не дойдя до сохранённого места.
  if (feedRestore) applyFeedRestore();
}

// Нажатие на самый старый закреп догружает ранние сообщения до предыдущего
// своего и везёт к нему ленту. Страницы без своих сообщений (одни ответы)
// пропускаем и просим следующую.
function continuePinOlder(list) {
  const at = list.indexOf(pinOlderFrom);
  if (at > 0) {
    pinOlderFrom = null;
    pinOlderPages = 0;
    // Дальше закреп, как и при обычном нажатии, показывает следующее своё —
    // но если раньше него в догруженном ничего нет, остаётся на том, к
    // которому едем: следующего ещё нет в ленте, и его текст показать неоткуда.
    pinIdx = Math.max(0, at - 2);
    renderPin(list);
    jumpToNode(list[at - 1]);
    holdPinJump();
    return;
  }
  if (at === 0 && pinOlderPages < PIN_OLDER_MAX && askOlder()) {
    pinOlderPages += 1;
    return;
  }
  // Раньше своих сообщений нет: нажатие идёт обычным путём — к самому старому
  // и по кругу к последнему.
  pinOlderFrom = null;
  jumpToPin();
}

// ------------------------------------------------------------ закреп и конец ленты

// Свои сообщения в ленте по порядку: отправленные и ещё стоящие в очереди.
// Порядок берём из DOM: ранние сообщения вставляются в начало ленты, а не в
// конец, и порядок вставки в `nodes` этому уже не соответствует.
function myMessages() {
  return [...$('feed').querySelectorAll('.item.user')];
}

function messageText(node) {
  const body = node.querySelector('.body');
  return ((body ? body.textContent : node.textContent) || '').replace(/\s+/g, ' ').trim();
}

function renderPin(list) {
  $('pin-preview').textContent = messageText(list[pinIdx]) || 'Сообщение';
  const count = $('pin-count');
  // Счётчик нужен, только когда закрепов несколько: по нему видно, где мы.
  count.hidden = list.length < 2;
  count.textContent = `${pinIdx + 1} / ${list.length}`;
}

// Лента изменилась: новое своё сообщение снова становится закрепом, а если
// закреплённое пропало (сообщение ушло из очереди), индекс подтягиваем к концу.
function syncPin() {
  const list = myMessages();
  if (!list.length) {
    resetPin();
    return;
  }
  const newest = list[list.length - 1].dataset.id || '';
  if (newest !== pinNewestId) {
    pinNewestId = newest;
    pinIdx = list.length - 1;
  }
  if (pinIdx < 0 || pinIdx >= list.length) pinIdx = list.length - 1;
  $('pin').hidden = false;
  renderPin(list);
}

function resetPin() {
  clearFlash();
  clearTimeout(pinJumpTimer);
  pinJumping = false;
  scrollHoldUntil = 0;
  $('pin').hidden = true;
  $('pin-preview').textContent = '';
  $('pin-count').hidden = true;
  $('to-end').hidden = true;
  pinIdx = -1;
  pinNewestId = '';
  pinOlderFrom = null;
  pinOlderPages = 0;
}

function clearFlash() {
  clearTimeout(flashTimer);
  flashId = '';
  for (const node of nodes.values()) node.classList.remove('flash');
}

// Прокрутка к сообщению с подсветкой. Нужна и закрепу, и переходу из избранного:
// сообщение ставим ближе к верху ленты, чтобы был виден и ответ на него.
function jumpToNode(node) {
  const feed = $('feed');
  const box = feed.getBoundingClientRect();
  const item = node.getBoundingClientRect();
  const top = feed.scrollTop + item.top - box.top - Math.min(120, feed.clientHeight * 0.25);
  // Авто-прилипание к концу на время перехода выключено: иначе первый же ответ
  // агента вернул бы ленту вниз, не дав дочитать найденное сообщение.
  scrollHoldUntil = Date.now() + 600;
  feed.scrollTo({ top: Math.max(0, top), behavior: 'smooth' });
  clearFlash();
  flashId = node.dataset.id || '';
  node.classList.add('flash');
  flashTimer = setTimeout(clearFlash, 1600);
}

// Пока лента едет к сообщению, закреп не пересчитываем по положению: он уже
// показывает следующее. Следующая ручная прокрутка снова включит следование.
function holdPinJump() {
  pinJumping = true;
  clearTimeout(pinJumpTimer);
  pinJumpTimer = setTimeout(() => {
    pinJumping = false;
  }, 1500);
}

// Переход по закрепу: к сообщению, на котором он стоит, и закреп — на
// предыдущее своё (после самого старого — по кругу к последнему).
function jumpToPin() {
  const list = myMessages();
  if (!list.length) return;
  if (pinIdx < 0 || pinIdx >= list.length) pinIdx = list.length - 1;
  const node = list[pinIdx];
  jumpToNode(node);
  pinIdx = pinIdx === 0 ? list.length - 1 : pinIdx - 1;
  renderPin(list);
  holdPinJump();
}

$('pin').addEventListener('click', () => {
  // Прошлое нажатие ещё ждёт ранние сообщения: повторное ничего не добавляет,
  // ответ вот-вот придёт.
  if (pinOlderFrom) return;
  const list = myMessages();
  if (!list.length) return;
  if (pinIdx < 0 || pinIdx >= list.length) pinIdx = list.length - 1;
  // На самом старом закрепе за лентой может быть ещё что-то: нажатие сначала
  // догружает ранние сообщения до предыдущего своего и везёт к нему, а не
  // перескакивает к последнему. Переход продолжит applyOlder, когда страница
  // придёт.
  if (pinIdx === 0 && olderMore) {
    pinOlderFrom = list[0];
    pinOlderPages = 0;
    if (askOlder()) return;
    pinOlderFrom = null;
  }
  jumpToPin();
});

function nearEnd() {
  const feed = $('feed');
  return feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
}

// Закреп следует за прокруткой: показывает своё сообщение, ближайшее выше верхней
// кромки ленты, а у самого низа — последнее своё. Как в Telegram, где закреп
// меняется синхронно с движением ленты.
function followPin() {
  if (subTab) return;
  const list = myMessages();
  if (!list.length) return;
  let idx = list.length - 1;
  if (!nearEnd()) {
    const edge = $('feed').getBoundingClientRect().top + 1;
    idx = 0;
    for (let i = 0; i < list.length; i++) {
      if (list[i].getBoundingClientRect().top <= edge) idx = i;
      else break;
    }
  }
  if (idx !== pinIdx) {
    pinIdx = idx;
    renderPin(list);
  }
}

// Кнопка «в конец» — только когда лента прокручена вверх: внизу она не нужна.
function updateToEnd() {
  $('to-end').hidden = nodes.size === 0 || nearEnd();
}

$('to-end').addEventListener('click', () => {
  scrollHoldUntil = 0;
  const feed = $('feed');
  feed.scrollTo({ top: feed.scrollHeight, behavior: 'smooth' });
});

// ------------------------------------------------------------ место чтения

// Сколько пикселей от низа считается «в конце» при сохранении места: меньше
// двух строк запаса, чтобы дрожь пальца и округление прокрутки не превращали
// чтение середины в «я в конце».
const PLACE_END = 24;

function placeAtEnd() {
  const feed = $('feed');
  return feed.scrollHeight - feed.scrollTop - feed.clientHeight <= PLACE_END;
}

// Где остановилась прокрутка ленты: элемент у верхней кромки и его смещение.
// В конец лента возвращается не пикселями, а признаком `end`: новый ответ
// делает прежний низ уже не концом. Пиксели нужны запасным вариантом — если
// сохранённого сообщения в загруженной части ленты уже нет.
function feedPlace() {
  const feed = $('feed');
  if (!nodes.size) return null;
  if (placeAtEnd()) return { end: true, id: '', off: 0, top: 0 };
  const box = feed.getBoundingClientRect();
  for (const node of feed.querySelectorAll('.item')) {
    const b = node.getBoundingClientRect();
    if (b.bottom > box.top + 1) {
      return {
        id: node.dataset.id || '',
        off: +(b.top - box.top).toFixed(1),
        top: Math.round(feed.scrollTop),
        end: false,
      };
    }
  }
  return { id: '', off: 0, top: Math.round(feed.scrollTop), end: false };
}

// Ставит ленту на сохранённое место. Сообщение из сохранённого места может быть
// в ещё не догруженной части: тогда встаём на то же место по пикселям, а
// applyOlder попробует найти его после каждой страницы.
function applyFeedPlace(pos) {
  const feed = $('feed');
  if (!pos || pos.end) {
    feed.scrollTop = feed.scrollHeight;
    return;
  }
  const node = pos.id ? nodes.get(pos.id) : null;
  if (node) {
    const box = feed.getBoundingClientRect();
    const want = feed.scrollTop + node.getBoundingClientRect().top - box.top - (pos.off || 0);
    feed.scrollTop = Math.max(0, want);
    return;
  }
  if (pos.top > 0) {
    feed.scrollTop = Math.min(pos.top, feed.scrollHeight);
    return;
  }
  feed.scrollTop = feed.scrollHeight;
}

// Место пришло от агента: лента уже собрана — ставим сразу, ещё нет — применит
// applyHistory, когда лента придёт. Ответа нет вовсе (старый агент) — через
// полторы секунды открываем с конца, как раньше.
function onFeedPlace(msg) {
  const rest = feedRestore;
  if (!rest || rest.key !== `${msg.agent}\n${msg.session}`) return;
  clearTimeout(rest.timer);
  rest.timer = null;
  rest.pos = msg.pos || null;
  applyFeedRestore();
}

function applyFeedRestore() {
  const rest = feedRestore;
  if (!rest || !rest.ready || rest.pos === undefined) return;
  const pos = rest.pos;
  const feed = $('feed');
  // Сохранённое сообщение в загруженной части ленты отсутствует, а прокрутка
  // была выше неё: значит, место в ещё не догруженных ранних сообщениях.
  // Просим страницу за страницей, пока не дойдём или они не кончатся.
  const max = Math.max(0, feed.scrollHeight - feed.clientHeight);
  if (pos && !pos.end && !nodes.has(pos.id || '') && pos.top > max + 1 && olderMore && rest.tries < PIN_OLDER_MAX) {
    rest.tries += 1;
    if (askOlder()) return;
  }
  feedRestore = null;
  applyFeedPlace(pos);
  updateToEnd();
}

function askFeedPlace(agent, session) {
  const rest = feedRestore;
  if (!send({ t: 'scroll', id: String(++cmdSeq), agent, session })) {
    // Соединения нет: сохранённое место не узнать, лента откроется с конца.
    if (rest) rest.pos = null;
    return;
  }
  if (!rest) return;
  clearTimeout(rest.timer);
  rest.timer = setTimeout(() => {
    if (feedRestore !== rest || rest.pos !== undefined) return;
    // Старая версия агента команду не знает и ответа не пришлёт.
    rest.pos = null;
    applyFeedRestore();
  }, 1500);
}

// Прокрутку сохраняем после остановки, а не на каждое движение: агенту это
// запись файла, и делать её на каждый пиксель незачем.
function noteFeedPlace() {
  if (!selected || subTab) return;
  // Пока сохранённое место ещё не пришло, нынешняя (ещё не поставленная)
  // прокрутка не должна его затереть.
  if (feedRestore && feedRestore.pos === undefined) return;
  clearTimeout(feedSaveTimer);
  feedSaveTimer = setTimeout(sendFeedPlace, 700);
}

function flushFeedPlace() {
  clearTimeout(feedSaveTimer);
  feedSaveTimer = null;
  sendFeedPlace();
}

function sendFeedPlace() {
  if (!selected || subTab) return;
  // Сохранённое место ещё не пришло: то, что видно сейчас, — ещё не чтение.
  if (feedRestore && feedRestore.pos === undefined) return;
  const pos = feedPlace();
  if (!pos) return;
  const sig = `${pos.end ? 1 : 0}|${pos.id}|${Math.round(pos.off)}|${Math.round(pos.top)}`;
  if (sig === feedSentPlace) return;
  if (send({ t: 'scroll_set', agent: selected.agent, session: selected.session, pos })) feedSentPlace = sig;
}

$('feed').addEventListener(
  'scroll',
  () => {
    updateToEnd();
    if (pinJumping) {
      // Ждём, пока плавная прокрутка к закрепу перестанет присылать события.
      clearTimeout(pinJumpTimer);
      pinJumpTimer = setTimeout(() => {
        pinJumping = false;
      }, 150);
      return;
    }
    followPin();
    askOlderAtTop();
    noteFeedPlace();
  },
  { passive: true },
);

// Место чтения сохраняем и тогда, когда страницу уводят в фон: на телефоне
// вкладку могут закрыть, не дав сработать отложенной записи.
window.addEventListener('pagehide', flushFeedPlace);
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') flushFeedPlace();
});
