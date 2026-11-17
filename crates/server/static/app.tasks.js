'use strict';

// Фоновые задачи сессии: команды, агенты и мониторы, которые Claude Code
// запустил в фоне и которые ещё работают. Список целиком приходит от агента
// сообщением `tasks`; полоска стоит над полем ввода, в `index.html`.

// [{ id, kind, title, started_at, ends_at }] — как пришло от агента.
let bgTasks = [];

const TASK_KINDS = { bash: 'команда', agent: 'агент', monitor: 'монитор' };

function tasksWord(n) {
  const d = n % 10;
  const h = n % 100;
  if (d === 1 && h !== 11) return 'фоновая задача';
  if (d >= 2 && d <= 4 && (h < 12 || h > 14)) return 'фоновые задачи';
  return 'фоновых задач';
}

function renderTasks() {
  const box = $('tasks');
  if (!bgTasks.length) {
    box.hidden = true;
    box.open = false;
    $('tasks-list').replaceChildren();
    return;
  }
  box.hidden = false;
  $('tasks-title').textContent = `${bgTasks.length} ${tasksWord(bgTasks.length)}`;
  $('tasks-list').replaceChildren(
    ...bgTasks.map((t) => {
      const row = el('li', 'bg-task');
      row.dataset.task = t.id || '';
      row.append(
        el('span', 'bg-task-kind', TASK_KINDS[t.kind] || t.kind || 'задача'),
        el('span', 'bg-task-title', t.title || t.id || ''),
        el('span', 'bg-task-age', t.started_at ? ago(t.started_at) : ''),
      );
      if (t.title) row.title = t.title;
      return row;
    }),
  );
}

// Полный список из `tasks`: он заменяет показанный.
function applyTasks(list) {
  bgTasks = Array.isArray(list) ? list : [];
  renderTasks();
}

setInterval(() => { if (bgTasks.length) renderTasks(); }, 30000); // обновляет «N мин»

function clearTasks() {
  bgTasks = [];
  renderTasks();
}
