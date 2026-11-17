'use strict';

// Поле ввода: отправка, рост по тексту, история стрелками и остановка хода.

// ------------------------------------------------------------ история запросов

function historyKey() {
  return selected ? `${selected.agent}\n${selected.session}` : '';
}

function historyEntries() {
  // Сначала запросы из ленты (в хронологическом порядке), затем отправленные
  // из этого браузера, которые агент ещё не успел показать в ленте.
  return [...feedRequests.values(), ...(sentRequests.get(historyKey()) || [])];
}

// Возврат к обычному вводу: текущее содержимое поля становится черновиком.
function resetHistoryNav() {
  histPos = 0;
  histDraft = $('text').value;
}

// Отправленный запрос — в историю, пока он не пришёл в ленту. Имя не должно
// совпадать с `rememberRequest` ленты: второе объявление функции затирает первое.
function rememberSent(text) {
  const key = historyKey();
  if (!key) return;
  let list = sentRequests.get(key);
  if (!list) sentRequests.set(key, (list = []));
  list.push(text);
}

// Запрос дошёл до ленты: убираем его временную копию, иначе в истории был бы дубль.
function consumeSentRequest(text) {
  const key = historyKey();
  const list = sentRequests.get(key);
  if (!list) return;
  const i = list.indexOf(text);
  if (i < 0) return;
  list.splice(i, 1);
  if (!list.length) sentRequests.delete(key);
}

// step = 1 — к более старому запросу, -1 — к более новому.
// false означает, что событие не наше: стрелка должна двигать каретку как обычно.
function navigateHistory(step) {
  const list = historyEntries();
  const area = $('text');
  if (step > 0) {
    if (!list.length || histPos >= list.length) return false;
    // Просмотр начинаем только с начала поля, иначе Up листает строки текста.
    // В однострочном черновике двигать каретку вверх некуда, поэтому там Up
    // всегда вызывает историю — как в оболочке.
    // Дальше Up продолжает листать, даже если каретка в конце: текст не менялся.
    if (histPos === 0) {
      const atStart = area.selectionStart === 0 && area.selectionEnd === 0;
      if (!atStart && area.value.includes('\n')) return false;
      histDraft = area.value;
    }
    histPos += 1;
    area.value = list[list.length - histPos];
  } else {
    if (histPos === 0) return false;
    histPos -= 1;
    area.value = histPos === 0 ? histDraft : list[list.length - histPos];
  }
  // Каретка в конец, чтобы сразу править запрос.
  area.setSelectionRange(area.value.length, area.value.length);
  autoGrow();
  return true;
}
$('composer').addEventListener('submit', (e) => {
  e.preventDefault();
  const text = $('text').value;
  if (!selected || !text.trim()) return;
  const ok = send({ t: 'send', id: String(++cmdSeq), agent: selected.agent, session: selected.session, text });
  if (ok) {
    rememberSent(text);
    $('text').value = '';
    resetHistoryNav();
    autoGrow();
    setHint('Отправляется…');
  } else {
    setHint('Нет соединения с сервером.', true);
  }
});

// Поле ввода растёт вместе с текстом, до 40% высоты экрана.
function autoGrow() {
  const text = $('text');
  text.style.height = 'auto';
  text.style.height = `${Math.min(text.scrollHeight + 2, window.innerHeight * 0.4)}px`;
}
$('text').addEventListener('input', () => {
  // Правка текста завершает просмотр истории: восстановленный запрос становится
  // черновиком, и следующая стрелка вверх начинает с последнего запроса.
  resetHistoryNav();
  autoGrow();
});
// На компьютере Enter отправляет, Shift+Enter переносит строку (как в Claude Code и opencode).
// На телефоне Enter переносит строку, а отправляет кнопка рядом с полем.
const touchInput = matchMedia('(pointer: coarse)').matches;
if (!touchInput) $('text').placeholder += '. Enter — отправить, Shift+Enter — новая строка, / — команды';

$('text').addEventListener('keydown', (e) => {
  // Список команд обрабатывает стрелки, Enter и Escape раньше нас: если он уже
  // занялся клавишей, полю остаётся только всё остальное.
  if (e.defaultPrevented) return;
  // isComposing: Enter при выборе слова в IME не должен ни отправлять сообщение,
  // ни листать историю.
  if (e.isComposing) return;
  if (e.key === 'Enter') {
    if (e.shiftKey || touchInput) return;
    // Enter без выбранного стрелками пункта остаётся отправкой: список просто
    // закрывается, а набранный текст уходит как есть.
    if (slash.isOpen()) slash.close();
    e.preventDefault();
    $('composer').requestSubmit();
    return;
  }
  // Вверх/вниз — предыдущие запросы. Shift+стрелки оставляем выделению текста,
  // Ctrl/Alt/Meta — системным и браузерным сочетаниям.
  if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
  if (e.shiftKey || e.altKey || e.ctrlKey || e.metaKey) return;
  if (navigateHistory(e.key === 'ArrowUp' ? 1 : -1)) e.preventDefault();
});

$('interrupt').addEventListener('click', () => {
  if (!selected) return;
  interruptId = String(++cmdSeq);
  send({ t: 'interrupt', id: interruptId, agent: selected.agent, session: selected.session });
  setHint('Прерываю…');
});

function onInterruptResult(msg) {
  interruptId = null;
  if (!msg.ok) {
    setHint(`Не удалось прервать: ${msg.error || 'неизвестная ошибка'}`, true);
    return;
  }
  setHint('Ход остановлен. Если запрос был отложен, opencode продолжит его выполнять.');
}
