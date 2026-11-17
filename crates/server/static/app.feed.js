'use strict';

// Лента открытой сессии: шапка, строка хода, лимиты и один элемент ленты —
// сообщение, вызов инструмента, дифф, уведомление.

// ------------------------------------------------------------ шапка и лента

// Строка хода: сколько идёт запрос и сколько токенов записал транскрипт.
// Только записанное агентом: до первого ответа токенов нет, и строка их не показывает.
function fmtTokens(n) {
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(n < 10000 ? 2 : 1).replace(/\.?0+$/, '')}k`;
}

function fmtElapsed(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}с`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}м ${String(s % 60).padStart(2, '0')}с`;
  return `${Math.floor(m / 60)}ч ${String(m % 60).padStart(2, '0')}м`;
}

function renderTurn() {
  const box = $('turn');
  const cur = current();
  const turn = cur && cur.agent.online && BUSY_STATUSES.has(cur.session.status) ? cur.session.turn : null;
  if (!turn) {
    box.hidden = true;
    return;
  }
  const parts = [];
  const mark = el('span', 'turn-mark', '✻');
  parts.push(el('span', '', fmtElapsed(Date.now() - turn.started_at)));
  if (turn.input) parts.push(el('span', '', `↑ ${fmtTokens(turn.input)}`));
  if (turn.output) parts.push(el('span', '', `↓ ${fmtTokens(turn.output)}`));
  box.replaceChildren(mark, ...parts);
  box.title = 'Время с запроса и токены, записанные в транскрипт: ↑ контекст последнего ответа, ↓ выход за ход';
  box.hidden = false;
}
setInterval(renderTurn, 1000);

function renderHeader() {
  renderTurn();
  // Сессию могли убрать из списка, пока правили её имя: поле больше не к чему.
  if (renaming && renaming.place === 'header' && !current()) dropRename();
  const cur = current();
  const editing = renaming && renaming.place === 'header';
  const titleEl = $('title');
  const badge = $('status');
  if (!cur) {
    titleEl.textContent = selected ? selected.session : 'Выберите сессию';
    titleEl.classList.remove('editable');
    titleEl.removeAttribute('title');
    titleEl.tabIndex = -1;
    $('rename').hidden = true;
    $('subtitle').textContent = '';
    $('session-id').hidden = true;
    $('copy-id').hidden = true;
    badge.hidden = true;
    $('handoff').hidden = true;
    $('interrupt').hidden = true;
    $('composer').hidden = true;
    $('more').hidden = true;
    $('favorites').hidden = true;
    setFavoritesMenu(false);
    setSessionMenu(false);
    return;
  }
  const { agent, session } = cur;
  // Пока правится имя, заголовок занят полем: текст вернётся после сохранения.
  if (!editing) {
    titleEl.textContent = session.title || session.key;
    titleEl.classList.add('editable');
    titleEl.title = 'Переименовать сессию';
    titleEl.tabIndex = 0;
  }
  // Карандаш ведёт к тому же полю, что и нажатие на заголовок. У агента не в
  // сети правка не отправится, поэтому кнопка неактивна.
  $('rename').hidden = Boolean(editing);
  $('rename').disabled = !agent.online;
  $('subtitle').textContent = `${agent.name} · ${session.tool} · ${session.cwd}`;
  $('session-id').hidden = false;
  $('session-id').textContent = sessionId(session.key);
  $('copy-id').hidden = false;
  $('favorites').hidden = false;
  badge.hidden = false;
  badge.className = `badge ${agent.online ? session.status : ''}`;
  badge.textContent = agent.online ? STATUS[session.status] || session.status : 'агент не в сети';
  const busy = agent.online && ['busy', 'shell', 'stalled'].includes(session.status);
  $('interrupt').hidden = !busy;
  const target = session.tool === 'claude' ? 'opencode' : session.tool === 'opencode' ? 'claude' : null;
  const targetName = target === 'claude' ? 'Claude Code' : 'opencode';
  $('handoff').hidden = !agent.online || !target;
  if (target) {
    $('handoff').querySelector('.label').textContent = `В ${targetName}`;
    $('handoff').setAttribute('aria-label', `Продолжить в ${targetName}`);
    $('handoff').title =
      `Продолжить в ${targetName}: в этом каталоге откроется новая сессия ` +
      'и получит выжимку из переписки этой сессии';
  }
  $('handoff').disabled = handoffId !== null;
  // Панель под стрелкой на телефоне: модель и та же кнопка смены агента.
  if (menuKey !== session.key) setSessionMenu(false);
  menuKey = session.key;
  $('more').hidden = false;
  $('sm-model').textContent = session.model || 'неизвестна';
  renderLimits(agent, session);
  $('sm-id-text').textContent = sessionId(session.key);
  $('sm-handoff').hidden = $('handoff').hidden;
  $('sm-handoff').textContent = target ? `Продолжить в ${targetName}` : '';
  $('sm-handoff').title = $('handoff').title;
  $('sm-handoff').disabled = $('handoff').disabled;
  const canSend = agent.online && session.delivery !== 'none';
  $('composer').hidden = false;
  $('text').disabled = !canSend;
  $('composer').querySelector('button').disabled = !canSend;
  if (!$('hint').dataset.sticky) {
    $('hint').className = 'hint';
    $('hint').textContent = !agent.online
      ? ''
      : session.status === 'stalled'
        ? STALLED_HINT
        : DELIVERY[session.delivery] || '';
  }
}

// Время сброса лимита: сегодня — только часы, иначе ещё и дата.
function resetText(ms) {
  if (!ms) return '';
  const d = new Date(ms);
  const now = new Date();
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  if (d.toDateString() === now.toDateString()) return hm;
  return `${String(d.getDate()).padStart(2, '0')}.${String(d.getMonth() + 1).padStart(2, '0')} ${hm}`;
}

// Остаток лимитов подписки на машине агента. Показываем только тот инструмент,
// в котором идёт эта сессия: лимит второго в ленте не нужен.
// Проценты — сколько ещё доступно, а не сколько израсходовано.
// Лимиты старше этого считаются не свежими: агент опрашивает раз в 3 минуты.
const LIMITS_STALE_MS = 10 * 60 * 1000;

function renderLimits(agent, session) {
  const box = $('sm-limits');
  box.replaceChildren();
  // Лимит OpenCode Go относится только к моделям этой подписки: сессия opencode
  // на zai, openai и т.п. его не тратит, поэтому строку скрываем целиком.
  const relevant = session.tool !== 'opencode' || (session.model || '').startsWith('opencode-go/');
  $('sm-limits-row').hidden = !relevant;
  if (!relevant) return;
  const all = agent && agent.limits ? agent.limits : [];
  const limits = all.filter((l) => l.tool === session.tool);
  if (!limits.length) {
    box.append(el('span', 'sm-limits-empty', agent && agent.online ? 'нет данных' : 'агент не в сети'));
    return;
  }
  for (const limit of limits) {
    const row = el('div', 'sm-limit');
    const meters = limit.meters || [];
    if (meters.some((m) => m.left === 0)) row.classList.add('out');
    const parts = [];
    const details = [];
    for (const m of meters) {
      parts.push(`${m.label} ${m.left}%`);
      const reset = resetText(m.resets_at);
      details.push(`${m.label}: осталось ${m.left}%${reset ? `, сброс ${reset}` : ''}`);
    }
    row.append(el('span', 'sm-limit-tool', limit.title || limit.tool));
    row.append(el('span', 'sm-limit-meters', parts.join(' · ')));
    // На телефоне подсказка по наведению недоступна, поэтому ближайший сброс
    // к тому же показан текстом.
    const nearest = meters.filter((m) => m.resets_at).sort((a, b) => a.resets_at - b.resets_at)[0];
    const notes = [];
    if (nearest) notes.push(`сброс ${resetText(nearest.resets_at)}`);
    // Агент держит последний удачный ответ, если источник перестал отвечать
    // (Anthropic отвечает 429 на частые запросы): тогда пишем, когда лимиты
    // проверялись, чтобы старые проценты не выглядели свежими.
    const stale = limit.updated_at && Date.now() - limit.updated_at > LIMITS_STALE_MS;
    if (stale) notes.push(`данные на ${resetText(limit.updated_at)}`);
    if (notes.length) row.append(el('span', 'sm-limit-reset', notes.join(' · ')));
    row.title = `${limit.title || limit.tool}: ${details.join(' · ')}${stale ? ` (проверено ${resetText(limit.updated_at)})` : ''}`;
    box.append(row);
  }
}

function setHint(text, isError) {
  const hint = $('hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
  // На телефоне лента (а с ней и #hint) скрыта, пока не выбрана сессия, поэтому
  // сообщение о запуске новой сессии дублируем в список — иначе на экране списка
  // не видно ни «Запускаем…», ни ошибки.
  const listHint = $('hint-list');
  listHint.textContent = text;
  listHint.className = isError ? 'hint hint-list error' : 'hint hint-list';
  if (text) {
    hint.dataset.sticky = '1';
    clearTimeout(setHint.timer);
    setHint.timer = setTimeout(() => {
      delete hint.dataset.sticky;
      renderHeader();
    }, isError ? 15000 : 4000);
  } else {
    delete hint.dataset.sticky;
  }
}

// Кнопка в шапке (компьютер) и строка в панели под стрелкой (телефон) копируют одно и то же.
async function copySessionId(btn) {
  const cur = current();
  if (!cur) return;
  const ok = await copyToClipboard(sessionId(cur.session.key));
  btn.classList.toggle('copied', ok);
  clearTimeout(copyIdTimer);
  if (ok) copyIdTimer = setTimeout(() => btn.classList.remove('copied'), 1500);
  setHint(ok ? 'Идентификатор скопирован.' : 'Не удалось скопировать идентификатор.', !ok);
}
$('copy-id').addEventListener('click', () => copySessionId($('copy-id')));
$('sm-id').addEventListener('click', () => copySessionId($('sm-id')));

async function copyToClipboard(text) {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {}
  // Запасной путь: Clipboard API нет на странице без HTTPS (панель в локальной сети).
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.opacity = '0';
  document.body.append(area);
  area.select();
  let ok = false;
  try {
    ok = document.execCommand('copy');
  } catch {}
  area.remove();
  return ok;
}

// Кнопка «скопировать текст» в строке под сообщением (рядом со временем).
// Иконка та же, что у идентификатора сессии; копируется исходный текст сообщения.
// После удачного копирования значок на пару секунд сменяется галочкой — как в
// Claude Code; никакого текста внизу панели не появляется.
const copyTextTimers = new WeakMap();
function copyTextButton(text) {
  const btn = el('button', 'copy-text');
  btn.type = 'button';
  btn.title = 'Скопировать текст сообщения';
  btn.setAttribute('aria-label', 'Скопировать текст сообщения');
  const copyIcon = icon([
    'M7 5.5h5A1.5 1.5 0 0 1 13.5 7v5a1.5 1.5 0 0 1-1.5 1.5H7A1.5 1.5 0 0 1 5.5 12V7A1.5 1.5 0 0 1 7 5.5Z',
    'M10.5 3.5V3A1.5 1.5 0 0 0 9 1.5H4A1.5 1.5 0 0 0 2.5 3v5A1.5 1.5 0 0 0 4 9.5h.5',
  ]);
  const checkIcon = icon(['M3.5 8.5 6.5 11.5 12.5 5']);
  btn.append(copyIcon);
  btn.addEventListener('click', async () => {
    const ok = await copyToClipboard(text);
    if (!ok) return;
    btn.replaceChildren(checkIcon);
    btn.classList.add('copied');
    clearTimeout(copyTextTimers.get(btn));
    copyTextTimers.set(btn, setTimeout(() => {
      btn.replaceChildren(copyIcon);
      btn.classList.remove('copied');
    }, 2000));
  });
  return btn;
}
function firstLine(text) {
  const line = text.split('\n', 1)[0];
  return line.length > 140 ? `${line.slice(0, 140)}…` : line;
}

// Строки диффа: заголовки файлов и хунков — отдельными классами, изменения —
// зелёным и красным. Классы задаются по первому символу строки.
function diffLineClass(line) {
  if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('Index:') || line.startsWith('===')) {
    return 'meta';
  }
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return '';
}

function diffInfo(text) {
  let file = '';
  let add = 0;
  let del = 0;
  for (const line of text.split('\n')) {
    if (!file && line.startsWith('+++ ')) file = line.slice(4).replace(/^b\//, '').trim();
    else if (line.startsWith('+') && !line.startsWith('+++')) add++;
    else if (line.startsWith('-') && !line.startsWith('---')) del++;
  }
  return { file: file || 'файл', add, del };
}

// Правка файла: раскрыта сразу, чтобы изменения были видны без клика.
function diffDetails(text) {
  const details = el('details', 'diff');
  details.open = true;
  const info = diffInfo(text);
  const summary = el('summary');
  summary.append(
    el('span', 'diff-name', `Изменения: ${info.file}`),
    el('span', 'diff-stat', `+${info.add} −${info.del}`),
  );
  if (info.file !== 'файл') summary.title = info.file;
  const body = el('div', 'diff-body');
  const lines = text.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  // Содержимое строк подсвечивается по языку файла; знак +/−/пробел — как был.
  const code = Highlight.diffLines(lines, info.file);
  lines.forEach((line, k) => {
    const cls = diffLineClass(line);
    const row = el('div', cls ? `diff-line ${cls}` : 'diff-line');
    if (code[k]) row.append(line.slice(0, 1), code[k]);
    else row.textContent = line;
    body.append(row);
  });
  details.append(summary, body);
  return details;
}

// Пути к картинкам в ленте раскрываются через сервер: файл лежит на машине агента.
function mdOptions() {
  const cur = selected;
  if (!cur) return {};
  return {
    localImage: (path) =>
      `/api/image?${new URLSearchParams({ agent: cur.agent, session: cur.session, path })}`,
  };
}

// Подсветка строится при первом раскрытии: результаты свёрнуты, и в длинной
// ленте большинство из них так и не откроют. До этого в узле — тот же текст,
// так что поиск по странице его находит.
function highlightOnOpen(details, body, build) {
  let done = false;
  const run = () => {
    if (done || !details.open) return;
    done = true;
    let view = null;
    try {
      view = build();
    } catch (e) {
      console.error('подсветка:', e);
    }
    if (view) {
      body.replaceChildren(view);
      body.classList.add('hl');
    }
  };
  details.addEventListener('toggle', run);
  run();
}

// Вызов инструмента для результата. У opencode id результата — id вызова
// с `:r`. У Claude Code вызов и результат — разные строки транскрипта без
// общего id в ленте, но результаты идут в порядке вызовов.
function callFor(item, isNew) {
  if (resultCalls.has(item.id)) return resultCalls.get(item.id);
  let call = null;
  if (item.id.endsWith(':r') && toolCalls.has(item.id.slice(0, -2))) {
    call = toolCalls.get(item.id.slice(0, -2));
    pendingCalls = pendingCalls.filter((c) => c !== call);
  } else if (isNew) {
    call = pendingCalls.shift() || null;
  }
  resultCalls.set(item.id, call);
  return call;
}

function trackCall(item, isNew) {
  if (item.kind === 'tool_use') {
    toolCalls.set(item.id, item);
    if (isNew) pendingCalls.push(item);
  } else if (item.kind === 'text' && item.role === 'assistant' && isNew) {
    // Ответ модели текстом — результаты вызовов уже пришли: незакрытые
    // вызовы не должны забрать чужой результат. Своё сообщение из очереди
    // может прийти посреди работы инструмента, поэтому оно очередь не трогает.
    pendingCalls = [];
  }
}

// Неуспешный статус уведомления (`failed`, `killed` и т.п.): строка плашки красная.
function noticeFailed(text) {
  const m = /<status>([\s\S]*?)<\/status>/.exec(text);
  return !!m && m[1].trim() !== '' && m[1].trim() !== 'completed';
}

// Строка уведомления Claude Code: итог из `<summary>`, иначе первая строка без тегов.
function noticeLabel(text) {
  const tag = (name) => {
    const m = new RegExp(`<${name}>([\\s\\S]*?)</${name}>`).exec(text);
    return m ? m[1].trim() : '';
  };
  // Итог Claude Code пишет сам («Background command … completed»), своё
  // название к нему не добавляем.
  const summary = tag('summary');
  const status = tag('status');
  const failed = status && status !== 'completed';
  const label = summary || `Уведомление: ${firstLine(text.replace(/<[^>]+>/g, ' ').trim())}`;
  return `${label}${failed ? ` (${status})` : ''}`;
}

// Аргументы `AskUserQuestion` (JSON) — строка ленты и текст вопросов с вариантами.
// Не разобрались (обрезанный JSON, другой формат) — null, строка покажется как обычный вызов.
function askText(text) {
  let questions;
  try {
    questions = JSON.parse(text).questions;
  } catch {
    return null;
  }
  if (!Array.isArray(questions) || !questions.length) return null;
  const first = questions[0] || {};
  const more = questions.length > 1 ? ` (и ещё ${questions.length - 1})` : '';
  const blocks = questions.map((q) => {
    const lines = [q.header ? `${q.header}: ${q.question || ''}` : String(q.question || '')];
    if (q.multiSelect) lines.push('(можно выбрать несколько)');
    for (const o of Array.isArray(q.options) ? q.options : []) {
      lines.push(`  • ${o.label || ''}${o.description ? ` — ${o.description}` : ''}`);
    }
    return lines.join('\n');
  });
  return { label: `Вопрос: ${firstLine(String(first.question || first.header || ''))}${more}`, text: blocks.join('\n\n') };
}

function buildItem(item, call) {
  const node = el('div', `item ${item.role} ${item.kind}`);
  node.dataset.id = item.id;
  // Время нужно фрагменту в избранном: выделенный кусок берёт его у сообщения.
  if (item.ts) node.dataset.ts = String(item.ts);
  if (item.kind === 'text') {
    const body = el('div', 'body md');
    body.append(renderMarkdown(item.text, mdOptions()));
    node.append(body);
  } else if (item.kind === 'error') {
    node.append(el('div', 'body', item.text));
  } else if (item.kind === 'queued') {
    const label = el('div', 'queue-label');
    // Очередь opencode (`queue:`) ждёт конца ответа и снимается из панели;
    // очередь Claude Code (`cq:`) модель прочтёт после текущего инструмента,
    // убрать её можно только в консоли.
    const own = item.id.startsWith('queue:');
    label.append(el('span', '', own ? 'В очереди — ждёт завершения текущего ответа' : 'В очереди — модель прочтёт после текущего шага'));
    if (own) {
      const cancel = el('button', 'queue-cancel', '×');
      cancel.type = 'button';
      cancel.title = 'Убрать из очереди';
      cancel.setAttribute('aria-label', 'Убрать сообщение из очереди');
      cancel.addEventListener('click', () => cancelQueued(item));
      label.append(cancel);
    }
    const body = el('div', 'body md');
    body.append(renderMarkdown(item.text, mdOptions()));
    node.append(label, body);
  } else if (item.kind === 'notice' && item.tool === 'compact') {
    // Сводка, с которой Claude Code продолжил сессию после сжатия контекста:
    // её прислал не человек, поэтому плашка, а внутри — markdown.
    const details = el('details');
    const body = el('div', 'body md');
    body.append(renderMarkdown(item.text, mdOptions()));
    details.append(el('summary', '', 'Контекст сжат: сводка прежней части сессии'), body);
    node.append(details);
  } else if (item.kind === 'notice') {
    // Служебный запрос Claude Code (уведомление о фоновой задаче): не своё
    // сообщение, а плашка, как у вызова инструмента; текст — XML с подсветкой.
    const details = el('details');
    const body = el('div', 'body', item.text);
    details.append(el('summary', noticeFailed(item.text) ? 'failed' : '', noticeLabel(item.text)), body);
    highlightOnOpen(details, body, () => Highlight.fragment(item.text, 'xml'));
    node.append(details);
  } else if (item.kind === 'diff') {
    node.append(diffDetails(item.text));
  } else {
    const details = el('details');
    // Вопрос Claude с вариантами: в ленте — текстом, а не JSON аргументов.
    const ask = item.kind === 'tool_use' && item.tool === 'AskUserQuestion' ? askText(item.text) : null;
    const label =
      item.kind === 'thinking' ? 'Размышление'
      : ask ? ask.label
      : item.kind === 'tool_use' ? `${item.tool || 'инструмент'}: ${firstLine(item.text)}`
      : `Результат: ${firstLine(item.text)}`;
    // Размышление — текст модели в Markdown: блоки кода в нём подсвечиваются
    // так же, как в ответе.
    let body;
    if (item.kind === 'thinking') {
      body = el('div', 'body md');
      body.append(renderMarkdown(item.text, mdOptions()));
    } else {
      body = el('div', 'body', ask ? ask.text : item.text);
    }
    details.append(el('summary', '', label), body);
    // Размышления раскрыты по умолчанию; вызовы инструментов и результаты свёрнуты.
    if (item.kind === 'thinking') details.open = true;
    if (item.kind === 'tool_use' && !ask) {
      highlightOnOpen(details, body, () => Highlight.toolInput(item.text, item.tool));
    } else if (item.kind === 'tool_result') {
      highlightOnOpen(details, body, () => Highlight.toolResult(item.text, call && call.tool, call && call.text));
    }
    node.append(details);
    // Картинки из аргументов и вывода инструмента — под его строкой, их видно
    // и в свёрнутом виде.
    if (item.kind === 'tool_use' || item.kind === 'tool_result') {
      const images = renderMarkdown.imageLinks(item.text, mdOptions());
      if (images) node.append(images);
    }
  }
  if (item.ts && (item.kind === 'text' || item.kind === 'error')) {
    // Строка под сообщением: время слева, избранное и копирование — справа.
    const foot = el('div', 'foot');
    foot.append(el('div', 'time', new Date(item.ts).toLocaleString()));
    const actions = el('div', 'foot-actions');
    // В избранное попадают свои сообщения и ответы агента: у ошибок и
    // служебных строк в списке был бы мусор.
    if (item.kind === 'text' && (item.role === 'user' || item.role === 'assistant')) {
      actions.append(favoriteButton(item));
    }
    if (item.text) actions.append(copyTextButton(item.text));
    if (actions.childElementCount) foot.append(actions);
    node.append(foot);
  }
  return node;
}

// Один элемент ленты. `target` — куда его поставить: сама лента для обычных
// обновлений или фрагмент перед ней, когда догружаются ранние сообщения.
function placeItem(item, target) {
  const old = nodes.get(item.id);
  trackCall(item, !old);
  const node = buildItem(item, item.kind === 'tool_result' ? callFor(item, !old) : null);
  if (item.id === flashId) node.classList.add('flash');
  if (old) {
    const oldDetails = old.querySelector('details');
    const newDetails = node.querySelector('details');
    if (oldDetails && newDetails) newDetails.open = oldDetails.open;
    renderMarkdown.keepOpenImages(old, node);
    old.replaceWith(node);
  } else {
    target.append(node);
  }
  nodes.set(item.id, node);
}

// Своё сообщение из ленты — оно же история для стрелок в поле ввода.
// `true`, если элемент — запрос из ленты.
function rememberRequest(item) {
  if (item.role !== 'user' || (item.kind !== 'text' && item.kind !== 'queued')) return false;
  feedRequests.set(item.id, item.text);
  return true;
}

function applyHistory(msg) {
  const feed = $('feed');
  const stick = Date.now() > scrollHoldUntil && feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
  if (msg.reset) {
    nodes.clear();
    favButtons.clear();
    feedRequests.clear();
    toolCalls.clear();
    resultCalls.clear();
    pendingCalls = [];
    // Агент сказал, есть ли перед этой лентой более ранние сообщения: если их
    // нет, кнопки догрузки не будет вовсе.
    olderMore = msg.more === true;
    feed.replaceChildren();
    if (!msg.items.length) feed.append(el('div', 'empty', 'Лента пуста.'));
  } else if (msg.items.length && !nodes.size) {
    feed.replaceChildren();
  }
  for (const id of msg.drop || []) {
    const node = nodes.get(id);
    if (node) {
      node.remove();
      nodes.delete(id);
    }
    feedRequests.delete(id);
    toolCalls.delete(id);
    resultCalls.delete(id);
    favButtons.delete(id);
  }
  for (const item of msg.items) {
    // Отправленное из панели уже в ленте: очередь больше не нужна.
    if (rememberRequest(item)) consumeSentRequest(item.text);
    placeItem(item, feed);
  }
  // Кнопка догрузки — всегда первая строка ленты; очистка выше её выбросила.
  mountOlder();
  updateOlder();
  // Закреп показываем до прокрутки в конец: полоса закрепа отнимает у ленты
  // высоту, и прокрутка, сделанная раньше, оставила бы низ последнего сообщения
  // скрытым — без единого следующего обновления, которое это исправило бы.
  syncPin();
  if (msg.reset || stick) feed.scrollTop = feed.scrollHeight;
  updateToEnd();
}
