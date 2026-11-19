'use strict';

// Release Notes: статический `/releases.md`, собранный из истории выпусков
// (`scripts/releases.mjs`) и закоммиченный вместе с ними. Открывается
// нажатием на имя «AiPanel» в шапке списка и занимает место ленты чата.
// У заметок свой адрес `#releases`: ссылкой можно поделиться, «Назад»
// возвращает к списку сессий.

const NOTES_HASH = '#releases';

// Текст файла запоминаем: пока панель открыта, по сети за ним не ходим.
let notesText = null;
let notesLoading = null;
// Разметка уже построена: при повторном открытии не пересобираем DOM.
let notesRendered = false;

const notesShown = () => $('app').classList.contains('notes');
const notesHash = () => location.hash === NOTES_HASH;

// Показывает или прячет заметки. Адрес и открытую сессию не трогает:
// этим занимаются openNotes/closeNotes и разбор адреса в app.sessions.js.
function showNotes(show) {
  $('app').classList.toggle('notes', show);
  $('notes').hidden = !show;
  document.title = show ? 'Release Notes — AiPanel' : 'AiPanel';
}

function hideNotes() {
  if (notesShown()) showNotes(false);
}

async function loadNotes() {
  const body = $('notes-body');
  if (notesText === null) {
    if (!notesLoading) {
      notesLoading = fetch('/releases.md').then((res) => {
        if (!res.ok) throw new Error(`/releases.md: ${res.status}`);
        return res.text();
      });
    }
    try {
      notesText = await notesLoading;
    } catch (e) {
      // Файл встроен в сборку сервера, поэтому ошибка — это скорее разрыв
      // связи, чем пропавший файл. Следующее открытие попробует снова.
      console.error('Release Notes:', e);
      notesLoading = null;
      body.replaceChildren(el('p', 'empty', 'Не удалось загрузить Release Notes. Попробуйте открыть заново.'));
      return;
    }
  }
  if (!notesRendered) {
    body.replaceChildren(renderMarkdown(notesText, {}));
    notesRendered = true;
  }
}

// Открывает заметки. `fromLink` — пришли по адресу `#releases`: тогда
// запись истории не добавляем, иначе «Назад» увёл бы со страницы.
function openNotes(fromLink) {
  // Заметки занимают место ленты, поэтому открытый чат закрываем. Адрес
  // чата ещё в адресной строке — его и заменит адрес заметок.
  const viewing = Boolean(selected) || Boolean(history.state && history.state.viewing);
  if (selected) closeView(true);
  if (fromLink) {
    history.replaceState(null, '', NOTES_HASH);
  } else if (viewing) {
    // Чат закрылся в этом же переходе: заменяем его запись, а не кладём
    // заметки поверх, — «Назад» должно вести к списку сессий.
    history.replaceState({ notes: true }, '', NOTES_HASH);
  } else if (!notesHash()) {
    history.pushState({ notes: true }, '', NOTES_HASH);
  }
  showNotes(true);
  $('notes-body').scrollTop = 0;
  loadNotes();
}

function closeNotes() {
  if (!notesShown()) return;
  showNotes(false);
  if (notesHash()) history.replaceState(null, '', location.pathname + location.search);
}

$('brand').addEventListener('click', () => {
  if (notesShown()) return;
  openNotes(false);
});

$('notes-back').addEventListener('click', () => {
  // Заметки открыли переходом — «Назад» вернёт к списку; пришли по прямой
  // ссылке — просто закрываем заметки.
  if (history.state && history.state.notes) history.back();
  else closeNotes();
});
