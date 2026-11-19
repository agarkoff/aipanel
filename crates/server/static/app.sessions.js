'use strict';

// Список сессий и действия над сессией: закрыть консоль, переименовать,
// удалить, открыть ленту. Здесь же панель под стрелкой и продолжение
// сессии в другом инструменте.

// ------------------------------------------------------------ список сессий

function ago(ts) {
  if (!ts) return '';
  const s = Math.max(0, (Date.now() - ts) / 1000);
  if (s < 60) return 'сейчас';
  if (s < 3600) return `${Math.floor(s / 60)} мин`;
  if (s < 86400) return `${Math.floor(s / 3600)} ч`;
  return `${Math.floor(s / 86400)} дн`;
}

function dotClass(s, online) {
  if (!online) return 'dot';
  if (s.status === 'busy' || s.status === 'shell' || s.status === 'stalled') return 'dot busy';
  if (s.status === 'waiting') return 'dot waiting';
  if (s.status === 'idle') return 'dot idle';
  return 'dot';
}

// ------------------------------------------------------------ проекты и закреп

// Агент присылает сессии в порядке последней активности, а у работающей сессии
// она меняется каждые две секунды. Такой порядок годится как исходный, но не как
// порядок строк: список перетасовывался бы под курсором. Поэтому строки собраны
// в проекты-каталоги, а место внутри проекта выдаётся один раз (см. rankOf).

// Каталог сессии — это проект. Путь сравниваем без хвостовых разделителей и без
// учёта регистра: `C:\Work\api` и `c:/work/api` — один и тот же проект.
function cwdKey(cwd) {
  return String(cwd || '').replace(/[\\/]+$/, '').toLowerCase().replace(/\//g, '\\');
}

// Проект — репозиторий, если каталог под git, иначе сам каталог. Сессии из
// worktree попадают в проект своего репозитория: ветки у них разные, а проект
// один, и ветку видно в строке.
function projectKeyOf(s) {
  const r = s.repo;
  return r ? `repo:${String(r.url || r.root || r.name || '').toLowerCase()}` : `dir:${cwdKey(s.cwd)}`;
}

function projectId(agentName, key) {
  return `project:${agentName}\n${key}`;
}

// Название проекта — конец пути. Если у агента два каталога заканчиваются
// одинаково, показываем больше сегментов, пока названия не разойдутся.
function projectLabels(projects) {
  const parts = new Map(projects.map((p) => [p.key, (p.cwd || '').split(/[\\/]+/).filter(Boolean)]));
  const tail = (key, n) => parts.get(key).slice(-n).join('\\');
  const labels = new Map();
  for (const p of projects) {
    const own = parts.get(p.key);
    if (!own.length) {
      labels.set(p.key, 'Без каталога');
      continue;
    }
    let take = 1;
    while (
      take < own.length &&
      projects.some((o) => {
        const other = parts.get(o.key);
        return o.key !== p.key && other.length && tail(o.key, take) === tail(p.key, take);
      })
    ) {
      take += 1;
    }
    labels.set(p.key, tail(p.key, take));
  }
  return labels;
}

// Статус задаёт порядок внутри проекта: сначала ждущие ответа, потом работающие.
const STATUS_GROUPS = ['wait', 'busy', 'idle'];

function statusGroup(s, online) {
  // Без связи состояние сессии устарело: держим её в общем списке, а не среди работающих.
  if (!online) return 'idle';
  if (s.status === 'waiting') return 'wait';
  if (s.status === 'busy' || s.status === 'shell' || s.status === 'stalled') return 'busy';
  return 'idle';
}

// Место внутри проекта выдаётся при первом появлении сессии и при переходе в
// другую группу статуса — тогда сессия встаёт сверху, потому что только что
// начала или закончила работу. Обновление «последней активности» место не меняет.
// Место проекта — то же самое: он не переезжает от чужой активности.
const listRanks = new Map(); // "агент\nсессия" -> { group, rank }
const projectRanks = new Map(); // id проекта -> место
let listRankSeq = 0;

function rankOf(agent, s, group) {
  const id = `${agent}\n${s.key}`;
  let meta = listRanks.get(id);
  if (!meta || meta.group !== group) {
    meta = { group, rank: ++listRankSeq };
    listRanks.set(id, meta);
  }
  return meta.rank;
}

function projectRankOf(id) {
  let rank = projectRanks.get(id);
  if (rank === undefined) {
    rank = ++listRankSeq;
    projectRanks.set(id, rank);
  }
  return rank;
}

// Точка проекта показывает самое важное, что внутри: ожидание, работу или покой.
function projectDot(items, online) {
  if (!online) return 'dot';
  if (items.some((i) => i.group === 'wait')) return 'dot waiting';
  if (items.some((i) => i.group === 'busy')) return 'dot busy';
  if (items.some((i) => i.s.status === 'idle')) return 'dot idle';
  return 'dot';
}

// Закреплённые сессии и свёрнутые проекты приходят от агента вместе со списком
// сессий (`pins` и `closed` в сообщении `agents`): хранятся они у него в
// `list.json`, поэтому одинаковы во всех браузерах и на всех устройствах.
function agentByName(name) {
  return agents.find((a) => a.name === name);
}

function agentPins(agent) {
  return agent && Array.isArray(agent.pins) ? agent.pins : [];
}

function agentClosed(agent) {
  return agent && Array.isArray(agent.closed) ? agent.closed : [];
}

// Ключ состояния: у агента свой список, поэтому имя агента в него не входит.
function sessionTag(agent, key) {
  return `${agent}\n${key}`;
}

function isPinned(agent, key) {
  return agentPins(agentByName(agent)).includes(key);
}

function togglePin(agentName, key) {
  const agent = agentByName(agentName);
  if (!agent) return;
  const on = !agentPins(agent).includes(key);
  // Список меняем сразу, не дожидаясь ответа агента: нажатие должно отзываться
  // мгновенно, а подтверждение придёт следующим списком.
  agent.pins = on ? [key, ...agentPins(agent).filter((k) => k !== key)] : agentPins(agent).filter((k) => k !== key);
  if (!send({ t: 'pin_session', agent: agentName, session: key, on })) {
    setHint('Нет соединения с сервером.', true);
  }
  renderSessions();
}

// Свёрнутые проекты лежат там же. Храним именно свёрнутые — новые проекты
// появляются раскрытыми.
function projectClosed(agentName, key) {
  return agentClosed(agentByName(agentName)).includes(key);
}

function toggleProject(agentName, key) {
  const agent = agentByName(agentName);
  if (!agent) return;
  const on = !agentClosed(agent).includes(key);
  if (on) projectShown.delete(projectId(agentName, key));
  agent.closed = on ? [...agentClosed(agent), key] : agentClosed(agent).filter((k) => k !== key);
  if (!send({ t: 'collapse_project', agent: agentName, project: key, on })) {
    setHint('Нет соединения с сервером.', true);
  }
  renderSessions();
}

function openProject(agentName, session) {
  const agent = agentByName(agentName);
  const key = projectKeyOf(session);
  if (!agent || !agentClosed(agent).includes(key)) return;
  agent.closed = agentClosed(agent).filter((k) => k !== key);
  send({ t: 'collapse_project', agent: agentName, project: key, on: false });
}

// Сессия только что начала ждать ответа — раскрываем её проект, чтобы вопрос не
// остался за свёрнутым заголовком. Свёрнутый вручную проект с уже известным
// вопросом обратно не раскрывается: решение человека сильнее.
//
// Что уже ждало ответа к моменту открытия панели — не повод раскрывать проекты:
// иначе после каждой перезагрузки (и на телефоне после каждого пробуждения)
// свёрнутые проекты раскрывались бы сами и стирали сохранённое состояние.
const lastWaiting = new Set();
let waitingKnown = false;

function revealWaiting() {
  const waiting = [];
  for (const a of agents) {
    for (const s of a.sessions) {
      if (s.status === 'waiting') waiting.push({ agent: a.name, session: s, tag: sessionTag(a.name, s.key) });
    }
  }
  if (waitingKnown) {
    for (const w of waiting) {
      if (!lastWaiting.has(w.tag)) openProject(w.agent, w.session);
    }
  }
  waitingKnown = true;
  lastWaiting.clear();
  for (const w of waiting) lastWaiting.add(w.tag);
}

// В раскрытом проекте сразу видно не больше пяти сессий: длинный каталог иначе
// выдавливает остальные проекты за край экрана. Сколько строк проекта уже
// догружено строкой «Ещё» — помним, пока открыта страница; свёрнутый проект
// при раскрытии снова показывает первые пять.
const PROJECT_ROWS = 5;
const projectShown = new Map(); // id проекта -> сколько строк догружено

// Раскладка одного агента: проекты-каталоги, внутри проекта — по статусу.
// Закреплённая сессия остаётся в своём проекте и лишь помечена кнопкой; наверх,
// в отдельный блок, она попадает только пока проект свёрнут — иначе одна и та же
// строка была бы в списке дважды.
function orderedList(agent, list) {
  const items = list.map((s) => ({ s, group: statusGroup(s, agent.online), pinned: isPinned(agent.name, s.key) }));

  // Места выдаём в обратном порядке активности: первый показ повторяет привычный
  // порядок (свежие сверху), а новая сессия получает место выше всех.
  for (const g of STATUS_GROUPS) {
    const group = items.filter((i) => i.group === g);
    for (let i = group.length - 1; i >= 0; i--) rankOf(agent.name, group[i].s, g);
  }

  const pinned = items.filter((i) => i.pinned);
  const pinOrder = agentPins(agent);
  pinned.sort((a, b) => pinOrder.indexOf(a.s.key) - pinOrder.indexOf(b.s.key));

  const byProject = new Map();
  for (const i of items) {
    const key = projectKeyOf(i.s);
    let p = byProject.get(key);
    if (!p) {
      const r = i.s.repo;
      p = {
        id: projectId(agent.name, key),
        key,
        // Подсказка: адрес репозитория, а для обычного каталога — полный путь.
        cwd: r ? r.url || r.root || r.name || '' : i.s.cwd || '',
        // Куда переехать, если строку бросить на этот проект: основная рабочая
        // копия репозитория (worktree — это тот же проект), иначе сам каталог.
        moveCwd: (r && r.root) || i.s.cwd || '',
        repo: !!r,
        name: r && r.name ? r.name : '',
        items: [],
      };
      byProject.set(key, p);
    }
    // Ключ проекта нужен строке: по нему видно, свёрнут ли её проект.
    i.projectKey = key;
    p.items.push(i);
  }

  const projects = [...byProject.values()];
  for (let i = projects.length - 1; i >= 0; i--) projectRankOf(projects[i].id);

  // Названия каталогов без репозитория: конец пути, при совпадении — длиннее.
  const labels = projectLabels(projects.filter((p) => !p.name));
  for (const p of projects) {
    p.items.sort(
      (a, b) =>
        STATUS_GROUPS.indexOf(a.group) - STATUS_GROUPS.indexOf(b.group) ||
        rankOf(agent.name, b.s, b.group) - rankOf(agent.name, a.s, a.group),
    );
    p.label = p.name || labels.get(p.key);
    p.waiting = p.items.some((i) => i.group === 'wait');
    p.dot = projectDot(p.items, agent.online);
  }

  // Проект с ожиданием поднимается наверх — это единственное движение в списке,
  // которое требует внимания. Остальные стоят по выданным местам.
  projects.sort(
    (a, b) =>
      Number(!a.cwd) - Number(!b.cwd) ||
      Number(b.waiting) - Number(a.waiting) ||
      projectRankOf(b.id) - projectRankOf(a.id),
  );

  return { pinned, projects };
}

// ------------------------------------------------------------ отрисовка списка

// Узлы списка переиспользуются между обновлениями: строки не создаются заново,
// поэтому список не мигает, не сбрасывает прокрутку и сохраняет фокус, а поле
// правки имени не теряется между сканами агента.
const listNodes = new Map(); // id узла -> запись с DOM и ссылками на поля

// Кнопки справа от текста: удаление у края, закрытие консоли ближе к тексту,
// закреп — левее всех, чтобы не сдвигать привычные места.
const PIN_ICON = ['M6.2 2.5h3.6l-.4 3.4 2.1 2.4H4.5l2.1-2.4z', 'M8 8.3v5.2'];

function agentEntry(a) {
  return {
    id: `agent:${a.name}`,
    init(entry) {
      entry.node = el('div', 'agent');
      entry.dot = el('span', 'dot');
      entry.name = el('span');
      entry.node.append(entry.dot, entry.name);
    },
    update(entry) {
      entry.dot.className = a.online ? 'dot idle' : 'dot';
      entry.name.textContent = a.online ? a.name : `${a.name} — не в сети`;
    },
  };
}

function emptyEntry(id, text) {
  return {
    id,
    init(entry) {
      entry.node = el('div', 'empty', text);
    },
    update() {},
  };
}

function groupEntry(agentName, key, title) {
  return {
    id: `group:${agentName}:${key}`,
    init(entry) {
      entry.node = el('div', 'list-group', title);
    },
    update(entry) {
      entry.node.textContent = title;
    },
  };
}

// Заголовок проекта: плюсик раскрывает и сворачивает каталог, счётчик говорит,
// сколько сессий внутри, точка — самое важное из их состояний.
function projectEntry(agent, project, expanded) {
  return {
    id: project.id,
    init(entry) {
      entry.agentName = agent.name;
      entry.projectKey = project.key;
      entry.node = el('button', 'project');
      entry.node.type = 'button';
      entry.toggle = el('span', 'project-toggle');
      entry.toggle.setAttribute('aria-hidden', 'true');
      entry.toggle.append(toggleIcon());
      entry.name = el('span', 'project-name');
      entry.count = el('span', 'project-count');
      entry.dot = el('span', 'dot');
      entry.node.append(entry.toggle, entry.name, entry.count, entry.dot);
      entry.node.addEventListener('click', () => toggleProject(entry.agentName, entry.projectKey));
    },
    update(entry) {
      entry.agentName = agent.name;
      entry.projectKey = project.key;
      // Куда переедет сессия, брошенная на этот проект: по этим полям цель
      // находит перетаскивание (см. dragTargetOf).
      entry.node.dataset.agent = agent.name;
      entry.node.dataset.moveCwd = project.moveCwd || '';
      entry.node.dataset.projectLabel = project.label || '';
      entry.node.setAttribute('aria-expanded', String(expanded));
      entry.name.textContent = project.label;
      // Подсказка объясняет, что за проект: репозиторий или каталог.
      entry.name.title = project.cwd
        ? `${project.repo ? 'Репозиторий' : 'Каталог'}: ${project.cwd}`
        : 'Каталог неизвестен';
      entry.count.textContent = String(project.items.length);
      entry.dot.className = project.dot;
      entry.node.setAttribute(
        'aria-label',
        `${project.label}: ${project.items.length} ${sessionsWord(project.items.length)}, ${expanded ? 'свернуть' : 'развернуть'}`,
      );
    },
  };
}

// Плюсик и минус — один значок: вертикальная черта плюса убирается, когда проект
// раскрыт. Разметка своя, а не текстовые «+»/«−»: у них разная высота и толщина.
function toggleIcon() {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('width', '16');
  svg.setAttribute('height', '16');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.8');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('aria-hidden', 'true');
  const line = (d, cls) => {
    const path = document.createElementNS(SVG_NS, 'path');
    path.setAttribute('d', d);
    if (cls) path.setAttribute('class', cls);
    svg.append(path);
  };
  line('M3.5 8h9');
  line('M8 3.5v9', 'project-toggle-bar');
  return svg;
}

// «1 сессия», «2 сессии», «5 сессий» — для подписи проекта.
function sessionsWord(n) {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (mod10 === 1 && mod100 !== 11) return 'сессия';
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return 'сессии';
  return 'сессий';
}

// Сколько строк проекта видно: первые пять и догруженные. Закреплённые в
// проекте не показываются — они стоят в своём блоке сверху, — поэтому счёт идёт
// по остальным. Найденное при фильтре не прячем за «Ещё»: человек ищет
// конкретную сессию, и она должна быть на виду.
function projectRows(agent, project, q) {
  if (q) return Infinity;
  let limit = PROJECT_ROWS + (projectShown.get(project.id) || 0);
  // Открытую сессию её же проект не прячет: строка должна быть видна.
  const mark = selected || lastViewed;
  if (mark && mark.agent === agent.name) {
    const at = project.items.filter((i) => !i.pinned).findIndex((i) => i.s.key === mark.session);
    if (at >= limit) limit = at + 1;
  }
  return limit;
}

// Короткая строка под пятой сессией проекта: шеврон вниз догружает следующую
// пятёрку. Высота у неё меньше строки сессии, чтобы не отбирать место у других
// проектов.
function moreEntry(project, hidden) {
  return {
    id: `more:${project.id}`,
    init(entry) {
      entry.projectId = project.id;
      entry.node = el('button', 'project-more');
      entry.node.type = 'button';
      entry.node.append(icon(['M4 6.5 8 10.5 12 6.5']));
      entry.text = el('span');
      entry.node.append(entry.text);
      entry.node.addEventListener('click', () => loadProjectMore(entry.projectId));
    },
    update(entry) {
      entry.text.textContent = `Ещё ${hidden} ${sessionsWord(hidden)}`;
      entry.node.setAttribute('aria-label', `Показать ещё ${hidden} ${sessionsWord(hidden)}`);
    },
  };
}

function loadProjectMore(id) {
  projectShown.set(id, (projectShown.get(id) || 0) + PROJECT_ROWS);
  renderSessions();
}

function rowEntry(agent, session, inProject) {
  const agentName = agent.name;
  const key = session.key;
  return {
    id: `row:${agentName}\n${key}`,
    init(entry) {
      entry.agentName = agentName;
      entry.key = key;
      entry.renamable = false;

      entry.node = el('div', 'session-row');
      entry.btn = el('button', 'session');
      entry.btn.type = 'button';
      entry.dot = el('span', 'dot');
      entry.name = el('span', 'name');
      entry.when = el('span', 'when');
      entry.meta = el('span', 'meta');
      entry.btn.append(entry.dot, entry.name, entry.when, entry.meta);
      entry.btn.addEventListener('click', () => select(entry.agentName, entry.key));

      // Название открытой сессии правится нажатием на него; остальная строка
      // по-прежнему открывает сессию. Правится именно открытая (selected), а не
      // просто подсвеченная после выхода.
      entry.name.addEventListener('click', (e) => {
        if (!entry.renamable) return;
        e.preventDefault();
        e.stopPropagation();
        startRename('list', entry.agent, entry.session, entry.node);
      });

      entry.action = el('span', 'swipe-action');
      entry.action.setAttribute('aria-hidden', 'true');

      // На компьютере консоль закрывает кнопка рядом с крестиком, на телефоне — свайп.
      // У закрытой сессии закрывать нечего: кнопки нет, а свайп сразу ведёт к удалению.
      entry.stop = el('button', 'session-close');
      entry.stop.type = 'button';
      entry.stop.title = 'Закрыть консоль';
      entry.stop.setAttribute('aria-label', 'Закрыть консоль');
      entry.stop.append(icon(['M12.6 4.2a6 6 0 1 1-9.2 0', 'M8 1.5v6']));
      entry.stop.addEventListener('click', (e) => {
        e.stopPropagation();
        askClose(entry.agentName, entry.session);
      });

      // На компьютере сессия удаляется крестиком, на телефоне — смахиванием влево.
      entry.del = el('button', 'session-delete');
      entry.del.append(icon(['M5 5l6 6', 'M11 5l-6 6']));
      entry.del.type = 'button';
      entry.del.title = 'Удалить сессию';
      entry.del.setAttribute('aria-label', 'Удалить сессию');
      entry.del.addEventListener('click', (e) => {
        e.stopPropagation();
        askDelete(entry.agentName, entry.session);
      });

      // Закрепление: закреплённые сессии всегда сверху и не участвуют в сортировке.
      entry.pin = el('button', 'session-pin');
      entry.pin.type = 'button';
      entry.pin.append(icon(PIN_ICON));
      // Палец, попавший по кнопке закрепа, не должен заодно тянуть свайп
      // закрытия и удаления: жест начинается со строки, а не с кнопки.
      for (const type of ['pointerdown', 'pointermove', 'pointerup', 'pointercancel']) {
        entry.pin.addEventListener(type, (e) => e.stopPropagation());
      }
      entry.pin.addEventListener('click', (e) => {
        e.stopPropagation();
        togglePin(entry.agentName, entry.key);
      });

      entry.node.append(entry.action, entry.btn, entry.del, entry.stop, entry.pin);
      attachSwipe(entry.node, entry.btn, entry.action, {
        get closable() {
          return !!entry.session && entry.session.live;
        },
        onClose: () => askClose(entry.agentName, entry.session),
        onDelete: () => askDelete(entry.agentName, entry.session),
      });
      attachDrag(entry.node, entry);
    },
    update(entry) {
      entry.session = session;
      entry.agent = agent;
      const pinned = isPinned(agentName, key);
      entry.dot.className = dotClass(session, agent.online);
      entry.name.textContent = session.title || key;
      entry.when.textContent = ago(session.updated_at);
      // В строке видно инструмент и место: у git-каталога это репозиторий с
      // веткой, поэтому в одном проекте строки с разными worktree различимы.
      entry.meta.textContent = `${session.tool} · ${placeLabel(session)}`;
      entry.meta.title = placeHint(session);
      // Подсвечивается открытая сессия, а после выхода из неё — та, где только что были.
      const mark = selected || lastViewed;
      entry.btn.classList.toggle('active', !!mark && mark.agent === agentName && mark.session === key);
      entry.renamable = !!selected && selected.agent === agentName && selected.session === key;
      entry.name.classList.toggle('rename-target', entry.renamable);
      // Длинное название обрезается: в подсказке оно целиком.
      entry.name.title = entry.renamable
        ? `${session.title || key}\nНажмите, чтобы переименовать`
        : session.title || key;
      entry.stop.hidden = !session.live;
      entry.node.classList.toggle('pinned', pinned);
      entry.node.classList.toggle('in-project', inProject);
      entry.pin.classList.toggle('on', pinned);
      entry.pin.title = pinned ? 'Открепить сессию' : 'Закрепить сессию';
      entry.pin.setAttribute('aria-label', entry.pin.title);
      entry.pin.setAttribute('aria-pressed', String(pinned));
    },
  };
}

// Раскладывает строки по плану: известные узлы двигаются, новые создаются,
// пропавшие удаляются. Пометки на узлах и фокус переживают обновление.
function applyPlan(box, plan) {
  const wasTop = new Map();
  for (const item of plan) {
    const entry = listNodes.get(item.id);
    if (entry) wasTop.set(entry.node, entry.node.getBoundingClientRect().top);
  }
  let index = 0;
  const keep = new Set();
  for (const item of plan) {
    let entry = listNodes.get(item.id);
    if (!entry) {
      entry = { id: item.id };
      item.init(entry);
      listNodes.set(item.id, entry);
    }
    item.update(entry);
    // insertBefore, а не append: узел переезжает только если стоит не на месте.
    const at = box.children[index];
    if (at !== entry.node) box.insertBefore(entry.node, at || null);
    index += 1;
    keep.add(item.id);
  }
  for (const [id, entry] of listNodes) {
    if (keep.has(id)) continue;
    entry.node.remove();
    listNodes.delete(id);
  }
  flyToPlace(wasTop);
}

// Перемещения строк анимируются: список меняется на глазах, но не рывком.
function flyToPlace(wasTop) {
  if (!wasTop.size || matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  for (const [node, top] of wasTop) {
    const dy = top - node.getBoundingClientRect().top;
    if (Math.abs(dy) < 2) continue;
    node.style.transition = 'none';
    node.style.transform = `translateY(${dy}px)`;
    requestAnimationFrame(() => {
      node.style.transition = 'transform .18s ease';
      node.style.transform = '';
      setTimeout(() => {
        node.style.transition = '';
      }, 220);
    });
  }
}

function renderSessions() {
  // Пока в строке правится название, список не пересобираем: иначе поле
  // потеряет фокус и недопечатанное имя. Новый список придёт после сохранения.
  if (renaming && renaming.place === 'list') return;
  const q = $('filter').value.trim().toLowerCase();
  const plan = [];
  if (!agents.length) {
    plan.push(emptyEntry('empty:agents', 'Нет подключённых агентов. Запустите aipanel-agent на своей машине.'));
  } else {
    revealWaiting();
    for (const a of agents) {
      plan.push(agentEntry(a));
      const list = a.sessions.filter((s) => {
        if (!q) return true;
        const r = s.repo || {};
        return `${s.title} ${s.cwd} ${r.name || ''} ${r.url || ''} ${r.branch || ''}`.toLowerCase().includes(q);
      });
      if (!list.length) {
        plan.push(emptyEntry(`empty:${a.name}`, 'Сессий нет.'));
        continue;
      }
      const { pinned, projects } = orderedList(a, list);
      // Раскрытость считаем заранее: по ней решается, что показывать внутри проекта.
      // Во время поиска проекты раскрыты — иначе совпадение осталось бы за плюсиком.
      const opened = new Map();
      for (const project of projects) opened.set(project.key, !!q || !projectClosed(a.name, project.key));
      // Закреплённые сессии живут отдельным блоком сверху — так их видно всегда,
      // даже когда проект свёрнут. Внутри проекта они не повторяются.
      if (pinned.length) {
        plan.push(groupEntry(a.name, 'pinned', 'Закреплённые'));
        for (const item of pinned) plan.push(rowEntry(a, item.s, false));
      }
      for (const project of projects) {
        plan.push(projectEntry(a, project, opened.get(project.key)));
        if (!opened.get(project.key)) continue;
        // Закреплённые уже стоят сверху, внутри проекта их нет. Из остальных
        // видно не больше пяти: под ними строка «Ещё» догружает следующую пятёрку.
        const rows = project.items.filter((i) => !i.pinned);
        const limit = projectRows(a, project, q);
        for (const item of rows.slice(0, limit)) plan.push(rowEntry(a, item.s, true));
        const hidden = rows.length - Math.min(limit, rows.length);
        if (hidden > 0) plan.push(moreEntry(project, hidden));
      }
    }
  }
  const alive = new Set();
  for (const a of agents) for (const s of a.sessions) alive.add(`${a.name}\n${s.key}`);
  for (const id of listRanks.keys()) if (!alive.has(id)) listRanks.delete(id);
  applyPlan($('sessions'), plan);
}

$('filter').addEventListener('input', renderSessions);

// ------------------------------------------------------------ место списка

// Место прокрутки списка хранится у агента вместе с местом чтения лент
// (`scroll.json`): список открывается там же, где его оставили, — и на телефоне,
// и после перезапуска панели. Живёт оно не у одной сессии, поэтому у агентов
// оно одно на всех: последнее записанное.
function noteListPlace(pos) {
  if (!pos || typeof pos.ts !== 'number' || pos.ts <= listPlaceTs) return;
  listPlaceTs = pos.ts;
  listRestore = pos;
}

// Ставит список на сохранённое место. Скрытый список (на телефоне открыта
// сессия) измерить нельзя: место применится, когда он снова появится.
function restoreListPlace() {
  const box = $('sessions');
  if (!listRestore || !box.clientHeight) return;
  const pos = listRestore;
  listRestore = null;
  listSentPlace = '';
  const node = pos.id ? listNodes.get(pos.id) : null;
  if (pos.end) {
    box.scrollTop = box.scrollHeight;
  } else if (node) {
    const shift = node.node.getBoundingClientRect().top - box.getBoundingClientRect().top - (pos.off || 0);
    box.scrollTop = Math.max(0, box.scrollTop + shift);
  } else if (pos.top > 0) {
    box.scrollTop = pos.top;
  }
}

// Что сейчас у верхней кромки списка: строка и её смещение. В конец — признаком,
// а не пикселями: список растёт, когда приходят новые сессии.
function listPlaceOf(box) {
  if (!box.children.length) return null;
  if (box.scrollHeight - box.scrollTop - box.clientHeight <= 24) {
    return { end: true, id: '', off: 0, top: 0 };
  }
  const top = box.getBoundingClientRect().top;
  for (const node of box.children) {
    const b = node.getBoundingClientRect();
    if (b.bottom <= top + 1) continue;
    return { id: listNodeId(node), off: +(b.top - top).toFixed(1), top: Math.round(box.scrollTop), end: false };
  }
  return { id: '', off: 0, top: Math.round(box.scrollTop), end: false };
}

// id узла списка по самому узлу: план отрисовки знает его, а DOM — нет.
function listNodeId(node) {
  for (const [id, entry] of listNodes) if (entry.node === node) return id;
  return '';
}

function saveListPlace() {
  const box = $('sessions');
  // На телефоне список скрыт, пока открыта сессия: его положение не менялось.
  if (!box.clientHeight) return;
  const pos = listPlaceOf(box);
  if (!pos) return;
  const sig = `${pos.end ? 1 : 0}|${pos.id}|${Math.round(pos.off)}|${Math.round(pos.top)}`;
  if (sig === listSentPlace) return;
  if (send({ t: 'list_scroll', pos })) listSentPlace = sig;
}

$('sessions').addEventListener(
  'scroll',
  () => {
    // Пока сохранённое место не применено, прокрутка — ещё не решение читателя.
    if (listRestore) return;
    clearTimeout(listSaveTimer);
    listSaveTimer = setTimeout(saveListPlace, 700);
  },
  { passive: true },
);
const flushListPlace = () => {
  clearTimeout(listSaveTimer);
  listSaveTimer = null;
  saveListPlace();
};
// На телефоне вкладку могут закрыть, не дав сработать отложенной записи.
window.addEventListener('pagehide', flushListPlace);
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') flushListPlace();
});

// Смахивание влево по строке сессии — жест телефона. Строка едет за пальцем
// и показывает, что будет при отпускании: до порога — синяя «Закрыть»
// (консоль сессии закрывается сразу, без подтверждения), дальше — красная
// «Удалить» с окном подтверждения. У закрытой сессии ступень одна: сразу удаление.
function attachSwipe(row, btn, action, actions) {
  const CLOSE_AT = 88;
  const DELETE_AT = 168;
  // Строка живёт дольше одной отрисовки, поэтому «есть что закрывать» спрашиваем
  // при каждом движении: сессию могли закрыть или снова открыть.
  const delAt = () => (actions.closable ? DELETE_AT : CLOSE_AT);
  const limit = () => delAt() + 16;
  let startX = 0;
  let startY = 0;
  let dx = 0;
  let dy = 0;
  let tracking = false;
  let horizontal = false;
  let swiped = false;
  const showAction = (dist) => {
    const del = !actions.closable || dist >= DELETE_AT;
    action.textContent = del ? 'Удалить' : 'Закрыть';
    action.className = del ? 'swipe-action delete' : 'swipe-action close';
  };
  showAction(0);
  row.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' || e.button !== 0) return;
    startX = e.clientX;
    startY = e.clientY;
    dx = 0;
    dy = 0;
    tracking = true;
    horizontal = false;
    swiped = false;
    showAction(0);
    row.classList.add('dragging');
  });
  row.addEventListener('pointermove', (e) => {
    if (!tracking) return;
    // Строку несут в другой проект: свайп не должен ехать за пальцем.
    if (dragSession) return;
    dx = e.clientX - startX;
    dy = e.clientY - startY;
    if (!horizontal) {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      // Вертикальное движение — это прокрутка списка, а не свайп.
      if (Math.abs(dx) <= Math.abs(dy) * 1.2) {
        tracking = false;
        row.classList.remove('dragging');
        return;
      }
      horizontal = true;
      row.setPointerCapture(e.pointerId);
    }
    const shift = Math.max(-limit(), Math.min(0, dx));
    btn.style.transform = `translateX(${shift}px)`;
    showAction(-shift);
    e.preventDefault();
  });
  const end = () => {
    if (!tracking) return;
    tracking = false;
    row.classList.remove('dragging');
    btn.style.transform = '';
    showAction(0);
    const dist = Math.min(limit(), Math.max(0, -dx));
    if (horizontal && dist > 8) {
      swiped = true;
      // Клик после свайпа не должен открыть сессию.
      setTimeout(() => {
        swiped = false;
      }, 400);
    }
    if (horizontal && dist >= delAt()) {
      // Открываем окно после возможного click: иначе касание, завершившее
      // свайп, попало бы в затемнение и сразу закрыло подтверждение.
      setTimeout(actions.onDelete, 0);
    } else if (horizontal && actions.closable && dist >= CLOSE_AT) {
      actions.onClose();
    }
    horizontal = false;
  };
  row.addEventListener('pointerup', end);
  row.addEventListener('pointercancel', () => {
    tracking = false;
    horizontal = false;
    row.classList.remove('dragging');
    btn.style.transform = '';
    showAction(0);
  });
  row.addEventListener(
    'click',
    (e) => {
      if (!swiped) return;
      e.preventDefault();
      e.stopPropagation();
    },
    true,
  );
}

// ------------------------------------------------------------ перенос в проект

// Сессию opencode можно перенести в другой проект: перетаскивание строки на
// заголовок проекта меняет её `cwd`, а с ним и проект в списке. Claude Code так
// не умеет: его транскрипт лежит в каталоге проекта, и агент отвечает ошибкой.
//
// На компьютере перенос начинается движением мыши, на телефоне — долгим
// нажатием: обычное касание должно по-прежнему прокручивать список, а
// горизонтальное движение — смахивать строку на удаление.
const DRAG_PRESS_MS = 450;
const DRAG_START_PX = 8;
const DRAG_SCROLL_EDGE = 32;
// Перемещение появилось в протоколе агента 17.4: старый агент не знает команду
// `move` и не ответит на неё вовсе, поэтому перенос ему не предлагается.
const MOVE_MIN = [17, 4];

let dragSession = null; // { agent, row, session, ghost, x, y, pointerId }
let dragTarget = null; // узел под указателем, куда упадёт строка
let dragSuppressClick = 0; // click сразу после переноса не открывает сессию
let dragScrollTimer = 0;

// Куда можно бросить строку — только заголовок проекта того же агента: сессия
// переезжает в каталог проекта (основную копию репозитория) и меняет место в
// списке. Строка другой сессии целью не считается: «переехать в другую сессию»
// читается как перенос задачи, а меняется только каталог.
function dragTargetOf(x, y) {
  if (!dragSession) return null;
  const node = document.elementFromPoint(x, y);
  const project = node ? node.closest('.project') : null;
  if (!project || project.dataset.agent !== dragSession.agent) return null;
  const cwd = project.dataset.moveCwd || '';
  if (!cwd || cwdKey(cwd) === cwdKey(dragSession.session.cwd)) return null;
  return project;
}

function setDragTarget(node) {
  if (dragTarget === node) return;
  if (dragTarget) dragTarget.classList.remove('drop-target');
  dragTarget = node;
  if (dragTarget) dragTarget.classList.add('drop-target');
}

function dragLabel(node) {
  return node.dataset.projectLabel || baseName(node.dataset.moveCwd);
}

function beginDrag(row, session, agent, x, y, pointerId) {
  if (session.tool !== 'opencode') {
    setHint('Перемещать можно только сессии opencode: у Claude Code переписка привязана к каталогу проекта.', true);
    return;
  }
  const version = agentByName(agent)?.version;
  const [major, minor] = String(version || '').split('.').map(Number);
  if (!(major > MOVE_MIN[0] || (major === MOVE_MIN[0] && minor >= MOVE_MIN[1]))) {
    setHint(`Агент${version ? ` ${version}` : ''} не умеет перемещать сессии — обновите агент.`, true);
    return;
  }
  const ghost = el('div', 'drag-ghost');
  ghost.setAttribute('aria-hidden', 'true');
  ghost.append(el('span', 'drag-ghost-name', session.title || session.key), el('span', 'drag-ghost-place', placeLabel(session)));
  document.body.append(ghost);
  dragSession = { agent, row, session, ghost, x, y, pointerId };
  row.classList.add('drag-source');
  document.body.classList.add('session-dragging');
  // Движения пальца или мыши приходят строке, даже когда указатель ушёл с неё:
  // иначе перенос обрывался бы на первом же шаге к чужому проекту.
  try {
    row.setPointerCapture(pointerId);
  } catch {}
  // click после отпускания не должен открыть сессию, мимо которой пронесли.
  dragSuppressClick = Date.now() + 600;
  moveGhost(x, y);
  if (!dragScrollTimer) dragScrollTimer = setInterval(dragScrollTick, 16);
  setHint('Отпустите над проектом, куда переместить сессию.');
}

function moveGhost(x, y) {
  if (!dragSession) return;
  dragSession.x = x;
  dragSession.y = y;
  dragSession.ghost.style.left = `${x}px`;
  dragSession.ghost.style.top = `${y - 18}px`;
  setDragTarget(dragTargetOf(x, y));
}

// У края списка он прокручивается сам: до дальнего проекта иначе не дотянуть.
function dragScrollTick() {
  if (!dragSession) {
    clearInterval(dragScrollTimer);
    dragScrollTimer = 0;
    return;
  }
  const box = $('sessions');
  const rect = box.getBoundingClientRect();
  const { x, y } = dragSession;
  if (y < rect.top + DRAG_SCROLL_EDGE) box.scrollTop -= 12;
  else if (y > rect.bottom - DRAG_SCROLL_EDGE) box.scrollTop += 12;
  else return;
  setDragTarget(dragTargetOf(x, y));
}

function endDrag(x, y, drop) {
  if (!dragSession) return;
  const drag = dragSession;
  const target = drop ? dragTargetOf(x, y) : null;
  dragSession = null;
  setDragTarget(null);
  drag.ghost.remove();
  drag.row.classList.remove('drag-source');
  document.body.classList.remove('session-dragging');
  try {
    drag.row.releasePointerCapture(drag.pointerId);
  } catch {}
  dragSuppressClick = Date.now() + 400;
  if (dragScrollTimer) {
    clearInterval(dragScrollTimer);
    dragScrollTimer = 0;
  }
  if (!target) return;
  askMove(drag.agent, drag.session, target.dataset.moveCwd, dragLabel(target));
}

function attachDrag(row, entry) {
  let press = null; // { x, y, pointerId, touch, timer } — нажатие до переноса

  const cancelPress = () => {
    if (!press) return;
    clearTimeout(press.timer);
    press = null;
  };

  row.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || dragSession) return;
    // Кнопки строки (закреп, закрытие, удаление) — не начало переноса.
    const button = e.target.closest('button');
    if (button && !button.classList.contains('session')) return;
    // В поле правки имени мышь и палец должны выделять текст, а не переносить.
    if (e.target.closest('input, textarea')) return;
    const touch = e.pointerType !== 'mouse';
    press = { x: e.clientX, y: e.clientY, pointerId: e.pointerId, touch, timer: 0 };
    if (!touch) return;
    press.timer = setTimeout(() => {
      const held = press;
      press = null;
      if (held && entry.session) beginDrag(row, entry.session, entry.agentName, held.x, held.y, held.pointerId);
    }, DRAG_PRESS_MS);
  });

  row.addEventListener('pointermove', (e) => {
    if (dragSession) {
      if (dragSession.row !== row) return;
      moveGhost(e.clientX, e.clientY);
      e.preventDefault();
      return;
    }
    if (!press) return;
    if (Math.abs(e.clientX - press.x) < DRAG_START_PX && Math.abs(e.clientY - press.y) < DRAG_START_PX) return;
    // Касание ушло в прокрутку или свайп: переноса не будет.
    if (press.touch) {
      cancelPress();
      return;
    }
    const start = press;
    press = null;
    if (entry.session) beginDrag(row, entry.session, entry.agentName, start.x, start.y, start.pointerId);
  });

  row.addEventListener('pointerup', (e) => {
    if (dragSession && dragSession.row === row) {
      endDrag(e.clientX, e.clientY, true);
      return;
    }
    cancelPress();
  });

  row.addEventListener('pointercancel', (e) => {
    if (dragSession && dragSession.row === row) endDrag(e.clientX, e.clientY, false);
    cancelPress();
  });

  // Долгое нажатие на телефоне иначе открывает меню «выделить/сохранить».
  row.addEventListener('contextmenu', (e) => {
    if (press || dragSession) e.preventDefault();
  });
}

// Пока строку несут, страница не прокручивается под пальцем.
document.addEventListener(
  'touchmove',
  (e) => {
    if (dragSession) e.preventDefault();
  },
  { passive: false },
);

// Click после переноса — это конец жеста, а не выбор сессии.
document.addEventListener(
  'click',
  (e) => {
    if (Date.now() >= dragSuppressClick) return;
    e.preventDefault();
    e.stopPropagation();
  },
  true,
);

// Esc отменяет перенос: сессия остаётся там, где была.
document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape' || !dragSession) return;
  e.preventDefault();
  endDrag(dragSession.x, dragSession.y, false);
});

// ------------------------------------------------------------ закрытие консоли

// Закрывает окно консоли сессии на компьютере (или её фоновый процесс),
// не удаляя переписку. Подтверждения нет: жест уже показал, что произойдёт.
function askClose(agent, session) {
  const id = String(++cmdSeq);
  if (!send({ t: 'close', id, agent, session: session.key })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  closeIds.add(id);
  setHint(`Закрываем консоль «${session.title || session.key}»…`);
}

function onCloseResult(msg) {
  closeIds.delete(msg.id);
  setHint(msg.ok ? 'Консоль закрыта.' : `Ошибка: ${msg.error || 'неизвестная'}`, !msg.ok);
}

// ------------------------------------------------------------ перемещение сессии

// Перемещение — команда агенту: он зовёт `/api/session/<id>/move` opencode.
// В панели каталог меняется не сразу, а следующим списком сессий: агент опрашивает
// opencode раз в пару секунд, а занятая сессия переезжает после текущего ответа.
const moveIds = new Map(); // id команды -> { title, label }

function askMove(agentName, session, cwd, label) {
  const agent = agentByName(agentName);
  if (!agent || !agent.online) {
    setHint('Агент не в сети: переместить сессию нельзя.', true);
    return;
  }
  if (session.tool !== 'opencode') {
    setHint('Перемещать можно только сессии opencode: у Claude Code переписка привязана к каталогу проекта.', true);
    return;
  }
  const id = String(++cmdSeq);
  if (!send({ t: 'move', id, agent: agentName, session: session.key, cwd })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  const title = session.title || session.key;
  moveIds.set(id, { title, label });
  const busy = ['busy', 'shell', 'stalled'].includes(session.status);
  setHint(`Перемещаем «${title}» в «${label}»${busy ? ' — на границе шага' : ''}…`);
}

function onMoveResult(msg) {
  const req = moveIds.get(msg.id);
  moveIds.delete(msg.id);
  if (msg.ok) setHint(req ? `Сессия «${req.title}» переехала в «${req.label}».` : 'Сессия перемещена.');
  else setHint(`Ошибка: ${msg.error || 'не удалось переместить сессию'}`, true);
}

// ------------------------------------------------------------ переименование

// Название сессии меняется у самого инструмента: Claude Code получает в
// транскрипт строку custom-title (так же, как от своего /rename), opencode —
// новый заголовок через свой API. Панель правит имя на месте: нажатие на
// название открытой сессии в списке или на заголовок в шапке открывает поле,
// Enter и уход из поля сохраняют, Esc отменяет, пустое имя — тоже отмена.
const RENAME_MAX = 200; // столько же, сколько у сервера и агента
let renaming = null; // { agent, session, place, old, input } — открытое поле

// Убирает поле, ничего не отправляя. Нужно, когда строку или шапку перерисовывает
// другое действие: клик по другой сессии, возврат к списку.
function dropRename() {
  if (!renaming) return;
  const input = renaming.input;
  renaming = null;
  input.remove();
}

function startRename(place, agent, session, row) {
  if (!agent.online) {
    setHint('Агент не в сети: переименовать нельзя.', true);
    return;
  }
  commitRename();
  if (place === 'list' && !row) return;
  const old = session.title || session.key;
  const input = el('input', `rename rename-${place}`);
  input.type = 'text';
  input.value = old;
  input.maxLength = RENAME_MAX;
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.setAttribute('aria-label', 'Название сессии');
  input.setAttribute('enterkeyhint', 'done');
  // Свайп строки не должен ловить движение пальца по полю.
  for (const type of ['pointerdown', 'pointermove', 'pointerup', 'click']) {
    input.addEventListener(type, (e) => e.stopPropagation());
  }
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeRename();
    }
  });
  input.addEventListener('blur', () => {
    if (renaming && renaming.input === input) commitRename();
  });
  renaming = { agent: agent.name, session: session.key, place, old, input };
  // В шапке поле встаёт на место заголовка, в списке — поверх всей строки: на
  // телефоне так удобнее печатать, и не приходится угадывать место под палец.
  if (place === 'header') {
    $('title').replaceChildren(input);
    $('rename').hidden = true;
  } else if (row.isConnected) row.append(input);
  else {
    renaming = null;
    return;
  }
  input.focus({ preventScroll: true });
  input.setSelectionRange(0, input.value.length);
}

// Возвращает список и шапку к обычному виду: их текст снова собирается из данных.
function closeRename() {
  if (!renaming) return;
  dropRename();
  renderSessions();
  renderHeader();
}

function commitRename() {
  const r = renaming;
  if (!r) return;
  const value = r.input.value.trim();
  dropRename();
  // Пустое имя и прежнее имя ничего не меняют: отменяем молча.
  if (value && value !== r.old) {
    const id = String(++cmdSeq);
    if (send({ t: 'rename', id, agent: r.agent, session: r.session, title: value })) {
      renameIds.add(id);
      setHint('Переименовываем…');
    } else {
      setHint('Нет соединения с сервером.', true);
    }
  }
  renderSessions();
  renderHeader();
}

function onRenameResult(msg) {
  renameIds.delete(msg.id);
  // Новое имя придёт следующим списком сессий от агента.
  setHint(msg.ok ? 'Сессия переименована.' : `Ошибка: ${msg.error || 'не удалось переименовать'}`, !msg.ok);
}

$('title').addEventListener('click', () => {
  if (renaming) return;
  const cur = current();
  if (cur) startRename('header', cur.agent, cur.session);
});
// Карандаш рядом с заголовком делает правку имени заметной — в том числе на
// телефоне, где наведение и подсказки недоступны.
$('rename').addEventListener('click', () => {
  const cur = current();
  if (cur) startRename('header', cur.agent, cur.session);
});
// Enter и пробел в шапке делают то же, что нажатие: правка имени с клавиатуры.
$('title').addEventListener('keydown', (e) => {
  if (e.target !== e.currentTarget) return;
  if (e.key === 'Enter' || e.key === ' ') {
    e.preventDefault();
    $('title').click();
  }
});

// ------------------------------------------------------------ удаление сессии

function deleteHint(text, isError) {
  const hint = $('delete-hint');
  hint.textContent = text;
  hint.className = isError ? 'hint error' : 'hint';
}

function askDelete(agent, session) {
  if (deleteId !== null) return;
  deleteRequest = { agent, session: session.key, title: session.title || session.key };
  $('delete-text').textContent =
    `Сессия «${deleteRequest.title}» будет удалена с компьютера без возможности восстановления.`;
  deleteHint('');
  // Галочка включена при каждом открытии: открытую сессию иначе не удалить.
  $('delete-close').checked = true;
  $('delete-close').disabled = false;
  $('delete-ok').disabled = false;
  $('delete-cancel').disabled = false;
  if (!$('delete-session').open) $('delete-session').showModal();
}

function closeDeleteDialog() {
  if (deleteId !== null) return;
  deleteRequest = null;
  $('delete-session').close();
}

$('delete-form').addEventListener('submit', (e) => {
  e.preventDefault();
  if (!deleteRequest || deleteId !== null) return;
  const id = String(++cmdSeq);
  const close = $('delete-close').checked;
  if (!send({ t: 'delete', id, agent: deleteRequest.agent, session: deleteRequest.session, close })) {
    deleteHint('Нет соединения с сервером.', true);
    return;
  }
  deleteId = id;
  $('delete-close').disabled = true;
  $('delete-ok').disabled = true;
  $('delete-cancel').disabled = true;
  deleteHint('Удаляем…');
});

$('delete-cancel').addEventListener('click', closeDeleteDialog);
// Пока команда выполняется, окно не закрываем: ошибку должно быть видно.
$('delete-session').addEventListener('cancel', (e) => {
  if (deleteId !== null) e.preventDefault();
  else deleteRequest = null;
});
$('delete-session').addEventListener('click', (e) => {
  if (e.target === e.currentTarget) closeDeleteDialog();
});

function onDeleteResult(msg) {
  deleteId = null;
  $('delete-close').disabled = false;
  $('delete-ok').disabled = false;
  $('delete-cancel').disabled = false;
  if (!msg.ok) {
    deleteHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  const req = deleteRequest;
  deleteRequest = null;
  $('delete-session').close();
  if (!req) return;
  // Убираем строку сразу, не дожидаясь следующего списка от агента.
  const agent = agents.find((a) => a.name === req.agent);
  if (agent) agent.sessions = agent.sessions.filter((s) => s.key !== req.session);
  // Удалённой сессии больше не к чему возвращать черновик.
  drafts.delete(`${req.agent}\n${req.session}`);
  draftSaved.delete(`${req.agent}\n${req.session}`);
  if (selected && selected.agent === req.agent && selected.session === req.session) closeView();
  else renderSessions();
  setHint('Сессия удалена.');
}

function current() {
  if (!selected) return null;
  const agent = agents.find((a) => a.name === selected.agent);
  const session = agent && agent.sessions.find((s) => s.key === selected.session);
  return agent && session ? { agent, session } : null;
}

// Ключ сессии — `tool:id`; пользователю нужен сам id (для `--resume`, `-s` и т.п.).
function sessionId(key) {
  const colon = key.indexOf(':');
  return colon < 0 ? key : key.slice(colon + 1);
}

// Адрес открытого чата: `#agent=…&session=…`, у ленты подагента — ещё `&sub=…`.
// По нему чат открывается заново после перезагрузки, и у каждого есть своя ссылка.
function sessionHash(agent, session, sub) {
  const params = new URLSearchParams({ agent, session });
  if (sub) params.set('sub', sub);
  return `#${params}`;
}

// Сессия (и подагент), которую просит адрес; null — адрес не про сессию.
function hashTarget() {
  const params = new URLSearchParams(location.hash.slice(1));
  const agent = params.get('agent');
  const session = params.get('session');
  if (!agent || !session) return null;
  return { agent, session, sub: params.get('sub') || '' };
}

// Приводит открытый чат в соответствие с адресом: сюда приходят кнопки
// «Назад»/«Вперёд», ручная правка адреса и ссылка из уведомления.
function syncFromHash() {
  // Адрес заметок открывает их вместо ленты; любой другой адрес заметки
  // прячет и показывает то, что в нём написано.
  if (location.hash === NOTES_HASH) {
    openNotes(true);
    return;
  }
  hideNotes();
  const target = hashTarget();
  if (!target) {
    if (selected) closeView();
    return;
  }
  const sameSession = selected && selected.agent === target.agent && selected.session === target.session;
  if (sameSession) {
    // Ссылка на другую ленту той же сессии: панель подагента или основная лента.
    if (target.sub !== subDrawer) {
      if (target.sub) openLinkedSub(target.sub);
      else closeSubDrawer();
    }
    return;
  }
  const agent = agents.find((a) => a.name === target.agent);
  if (!agent || !agent.sessions.some((s) => s.key === target.session)) {
    // Список ещё не пришёл (или агент не в сети): чат откроет `tryPendingOpen`.
    pendingOpen = target;
    return;
  }
  select(target.agent, target.session, target.sub);
}

// Ссылка на только что открытую сессию. Запись в истории нужна, чтобы системная
// кнопка «Назад» возвращала к списку, а не уводила со страницы. Адрес уже
// совпал (перешли по своей же ссылке) — новую запись не добавляем.
function setSessionUrl(agent, session, sub) {
  const hash = sessionHash(agent, session, sub);
  if (location.hash === hash || (history.state && history.state.viewing)) {
    history.replaceState({ viewing: true }, '', hash);
  } else {
    history.pushState({ viewing: true }, '', hash);
  }
}

// Открытие панели подагента не создаёт запись истории: «Назад» по-прежнему
// ведёт к списку сессий, а адрес показывает открытую ленту.
function updateSessionUrl() {
  if (!selected) return;
  const hash = sessionHash(selected.agent, selected.session, subDrawer);
  if (location.hash !== hash) history.replaceState(history.state, '', hash);
}

function select(agent, session, sub) {
  // Открытие сессии из списка закрывает Release Notes.
  hideNotes();
  // Недописанное в прежней сессии сохраняем до смены selected — ключ берётся
  // по ней, иначе черновик ушёл бы не той сессии. Отправляем сразу: агенту
  // он нужен раньше, чем поле покажет черновик другой сессии.
  saveDraft();
  flushDraft();
  // Место чтения прежней сессии — тоже по её ключу.
  flushFeedPlace();
  // Сессию открыли — её уведомление в строке состояния больше не нужно.
  closeNotice(agent, session);
  dropRename();
  selected = { agent, session };
  lastViewed = selected;
  // Сессию могли открыть не из списка: её проект должен быть раскрыт, иначе
  // строка останется за плюсиком.
  const opened = agents.find((a) => a.name === agent);
  const found = opened && opened.sessions.find((s) => s.key === session);
  if (found) openProject(agent, found);
  nodes.clear();
  feedRequests.clear();
  favorites = [];
  favButtons.clear();
  setFavoritesMenu(false);
  applyFavorites();
  clearPrompts();
  clearTasks();
  // Подагенты относятся к прежней сессии: их карточки и открытая панель тоже.
  resetSubagentView();
  // Ссылка могла вести на ленту подагента: откроем её сразу.
  if (sub) openLinkedSub(sub);
  // В поле — черновик открытой сессии, а не текст предыдущей.
  loadDraft();
  clearTimeout(copyIdTimer);
  $('copy-id').classList.remove('copied');
  $('sm-id').classList.remove('copied');
  $('feed').replaceChildren(el('div', 'empty', 'Загрузка ленты…'));
  resetPin();
  resetOlder();
  // Где человек остановился в этой сессии, знает агент: место приходит раньше
  // ленты, поэтому сначала ждём его, а не открываем конец. Без ответа (старый
  // агент) лента откроется с конца, как раньше.
  feedRestore = { key: sessionTag(agent, session), pos: undefined, ready: false, tries: 0, timer: null };
  feedSentPlace = '';
  askFeedPlace(agent, session);
  setHint('');
  $('app').classList.add('viewing');
  // Адрес — ссылка на этот чат; вкладка подагента уже в нём, если её просили.
  setSessionUrl(agent, session, sub);
  send({ t: 'watch', agent, session });
  // Список избранного приходит от агента: у него он и хранится.
  send({ t: 'favorites', agent, session });
  renderSessions();
  renderHeader();
  // Поле уже видно: черновик сессии может быть многострочным, и высоту ему
  // нужно посчитать сейчас, а не при первом нажатии клавиши.
  autoGrow();
}

function closeView(keepUrl) {
  // Закрытие сессии возвращает к списку, а не к заметкам.
  hideNotes();
  // Возврат к списку не должен терять недописанный текст: он останется
  // за сессией до следующего её открытия — и здесь, и на других устройствах.
  saveDraft();
  flushDraft();
  // Место чтения ленты — тоже до того, как сессия перестала быть открытой.
  flushFeedPlace();
  feedRestore = null;
  clearTimeout(feedSaveTimer);
  feedSaveTimer = null;
  dropRename();
  $('app').classList.remove('viewing');
  selected = null;
  // Поля ввода в списке нет; пустое значение сбрасывает и просмотр истории.
  loadDraft();
  favorites = [];
  favButtons.clear();
  setFavoritesMenu(false);
  applyFavorites();
  clearPrompts();
  clearTasks();
  resetSubagentView();
  resetPin();
  resetOlder();
  send({ t: 'unwatch' });
  // У закрытой сессии своей ссылки больше нет: возвращаем адрес списка. Если
  // сессию закрыли не кнопкой «Назад», текущая запись истории всё ещё помечена
  // как просмотр — снимаем пометку, иначе следующая сессия не добавила бы новую.
  // `keepUrl` — адрес ещё понадобится вызывающему (переход к Release Notes).
  if (!keepUrl) {
    if (location.hash) history.replaceState(null, '', location.pathname + location.search);
    else if (history.state && history.state.viewing) history.replaceState(null, '');
  }
  // Подсказка относилась к открытой сессии; в списке её копия была бы уже не к месту.
  setHint('');
  renderSessions();
  renderHeader();
  // Место списка, сохранённое на другом устройстве, применяем, когда он снова
  // виден: пока была открыта сессия, измерить его было нельзя.
  restoreListPlace();
  // На телефоне список был скрыт; прокручиваем к сессии, из которой вышли.
  const mark = document.querySelector('#sessions .session.active');
  if (mark) mark.scrollIntoView({ block: 'nearest' });
}

$('back').addEventListener('click', () => {
  if (history.state && history.state.viewing) history.back();
  else closeView();
});
// «Назад»/«Вперёд» и ручная правка адреса открывают то, что написано в ссылке.
window.addEventListener('popstate', syncFromHash);
window.addEventListener('hashchange', syncFromHash);
// ------------------------------------------------------------ панель сессии (телефон)

function setSessionMenu(open) {
  $('session-menu').hidden = !open;
  $('more').setAttribute('aria-expanded', String(open));
}

$('more').addEventListener('click', () => setSessionMenu($('session-menu').hidden));
$('sm-handoff').addEventListener('click', () => {
  setSessionMenu(false);
  $('handoff').click();
});
document.addEventListener('click', (e) => {
  if ($('session-menu').hidden) return;
  if (!e.target.closest('#session-menu, #more')) setSessionMenu(false);
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !$('session-menu').hidden) {
    setSessionMenu(false);
    $('more').focus();
  }
});

// ------------------------------------------------------------ продолжение сессии

$('handoff').addEventListener('click', () => {
  const cur = current();
  if (!cur || handoffId !== null) return;
  const tool = cur.session.tool === 'claude' ? 'opencode' : 'claude';
  const name = tool === 'claude' ? 'Claude Code' : 'opencode';
  const id = String(++cmdSeq);
  // Снимок ключей: новая сессия ищется как появившаяся в этом каталоге.
  const keys = new Set(cur.agent.sessions.map((s) => s.key));
  if (!send({ t: 'handoff', id, agent: cur.agent.name, session: cur.session.key, tool })) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  handoffId = id;
  handoffRequest = { agent: cur.agent.name, tool, cwd: cur.session.cwd, keys };
  $('handoff').disabled = true;
  $('sm-handoff').disabled = true;
  setHint(`Собираем выжимку и открываем сессию ${name} в этом каталоге…`);
});

function onHandoffResult(msg) {
  const req = handoffRequest;
  handoffId = null;
  handoffRequest = null;
  $('handoff').disabled = false;
  $('sm-handoff').disabled = false;
  if (!req) return;
  if (!msg.ok) {
    setHint(`Ошибка: ${msg.error || 'неизвестная'}`, true);
    return;
  }
  pendingLaunch = {
    agent: req.agent,
    tool: req.tool,
    cwd: req.cwd,
    key: msg.session || null,
    keys: req.keys,
    until: Date.now() + (msg.session ? 90000 : 20000),
  };
  setHint(req.tool === 'claude' ? 'Открываем окно Claude Code…' : 'Открываем окно opencode…');
  checkPendingLaunch();
}
