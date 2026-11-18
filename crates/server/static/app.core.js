'use strict';

// Общее для всех частей панели: поиск элементов, сборка узлов, иконки,
// подписи состояний и всё состояние интерфейса. Остальные части — тоже
// обычные скрипты из `index.html`, поэтому видят его напрямую.


const $ = (id) => document.getElementById(id);
const el = (tag, cls, text) => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
};

// Иконка из путей SVG: текстовый символ вроде «⏻» есть не во всех шрифтах,
// а в кружке ещё и стоит по метрикам шрифта, а не по центру — от шрифта к
// шрифту он съезжает по-разному (телефон и компьютер).
const SVG_NS = 'http://www.w3.org/2000/svg';
// Звезда избранного: залитая применяется и в шапке, и у сообщения.
const STAR = 'M8 2L9.5 6.1L13.9 6.3L10.5 9L11.6 13.2L8 10.8L4.4 13.2L5.5 9L2.1 6.3L6.5 6.1Z';

const icon = (paths) => {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('width', '15');
  svg.setAttribute('height', '15');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.5');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('aria-hidden', 'true');
  for (const d of paths) {
    const path = document.createElementNS(SVG_NS, 'path');
    path.setAttribute('d', d);
    svg.append(path);
  }
  return svg;
};

// Где работает сессия. Путь сам по себе путает: агент может перевести сессию в
// worktree (`.claude/worktrees/…`, `~/.local/share/opencode/worktree/…`), и она
// выглядит как чужой каталог. Поэтому для git-каталога показываем репозиторий
// и ветку, а путь — в подсказке; для обычного каталога — его имя.
const baseName = (path) => String(path || '').replace(/[\\/]+$/, '').split(/[\\/]/).pop() || String(path || '');

// Коротко, для списка сессий: `ai-panel ⎇ refactor-master` или `Баланс`.
function placeLabel(s) {
  const r = s.repo;
  if (!r) return baseName(s.cwd);
  return r.branch ? `${r.name} ⎇ ${r.branch}` : r.name;
}

// Полностью, для шапки сессии: адрес репозитория вместо имени.
function placeFull(s) {
  const r = s.repo;
  if (!r) return s.cwd || '';
  const where = r.url || r.root || r.name;
  return r.branch ? `${where} ⎇ ${r.branch}` : where;
}

// Подсказка: настоящий каталог и, для worktree, основная копия.
function placeHint(s) {
  const r = s.repo;
  const lines = [s.cwd || ''];
  if (r && r.worktree) lines.push(`worktree репозитория ${r.root || r.name}`);
  return lines.filter(Boolean).join('\n');
}

const STATUS = {
  busy: 'работает',
  idle: 'ждёт ввода',
  waiting: 'ждёт ответа',
  shell: 'выполняет команду',
  stalled: 'принято, нет ответа',
  stopped: 'не запущена',
  ready: 'не активна',
};
// Подсказки к состояниям, в которых окно выглядит зависшим, хотя сессия занята.
const STALLED_HINT =
  'opencode принял сообщение, но ещё не начал ответ. «Прервать» остановит ход и растолкает очередь.';
// Подсказки только про необычную доставку. Обычные случаи (открытый терминал,
// API opencode) очевидны по самой сессии и строку не занимают.
const DELIVERY = {
  window: 'Сессия не открыта: на компьютере откроется окно консоли с этой сессией, сообщение уйдёт в него первым запросом.',
  headless: 'Сессия не открыта: сообщение выполнит фоновый процесс claude -p --resume в каталоге сессии.',
  none: 'Отправка в эту сессию недоступна на этой платформе.',
};

let ws = null;
let agents = [];
let selected = null; // { agent, session }
let lastViewed = null; // последняя открытая сессия: остаётся подсвеченной в списке после выхода
let cmdSeq = 0;
const nodes = new Map(); // id элемента ленты -> DOM-узел

// Закреп в шапке ленты: показываем последнее своё сообщение, по нажатию переходим
// к нему, а закреп переключается на предыдущее своё — как в Telegram. При обычной
// прокрутке закреп сам следует за лентой.
let pinIdx = -1; // индекс своего сообщения в ленте, на который ведёт закреп
let pinNewestId = ''; // id самого нового своего сообщения при прошлой отрисовке
// Пока закреп везёт ленту к сообщению, прокрутка не должна переключать закреп:
// после нажатия он уже показывает следующее сообщение, как в Telegram.
let pinJumping = false;
let pinJumpTimer = null;
// Нажатие на самый старый закреп догружает ранние сообщения: здесь — узел, до
// которого нажали, и сколько страниц подряд уже запрошено ради этого. Как только
// в догруженном появится предыдущее своё сообщение, лента поедет к нему.
let pinOlderFrom = null;
let pinOlderPages = 0;
let flashId = ''; // id сообщения, к которому сейчас подсвечен переход
let flashTimer = null;
// Пока идёт переход по закрепу, авто-прилипание к концу ленты выключено:
// иначе первый же ответ агента вернул бы ленту вниз, не дав дочитать сообщение.
let scrollHoldUntil = 0;

// Ранние сообщения: агент присылает только последние, остальные — по кнопке в
// начале ленты. Здесь её состояние: запрос отправлен, ответа ещё нет; есть ли
// что грузить дальше; страховка на случай, если агент не ответил (старая версия
// не знает команду).
let olderPending = false;
let olderMore = true;
let olderTimer = null;
// Ссылку на кнопку держим сами: очистка ленты выбрасывает её из документа, и
// поиск по id после этого её уже не находит.
const olderBtn = $('older');

// Запуск новой сессии: id команды, её параметры и ожидаемая сессия.
let launchId = null;
let launchRequest = null;
let pendingLaunch = null;
// Начало ошибки агента «такого каталога нет» (`crates/agent/src/util.rs`,
// `DIR_MISSING`): по нему панель предлагает создать каталог и повторить запуск.
const DIR_MISSING = 'каталог не найден: ';
// Запуск, который ждёт ответа на вопрос о создании каталога.
let mkdirRequest = null;
// Модели для окна «Новая сессия»: список по «компьютер + инструмент».
const launchModelCache = new Map();
let launchModelsRequest = null; // { id, key } — ждём ответ для этой пары
// Последняя выбранная в окне запуска модель, отдельно для каждого инструмента.
const launchModelChoice = { claude: '', opencode: '' };
// Продолжение сессии в другом инструменте: id команды и её параметры.
let handoffId = null;
let handoffRequest = null;
// Сессия, для которой открыта панель под стрелкой: при переходе к другой панель закрывается.
let menuKey = null;
let copyIdTimer = null;
// Избранное открытой сессии. Список хранит агент (`favorites.json` в его
// каталоге), поэтому звёзды и плашка в шапке одинаковы во всех браузерах.
let favorites = []; // [{ id, ts, role, text }]
const favButtons = new Map(); // id элемента ленты -> звезда в его строке
let cancelQueuedId = null;
// Удаление сессии: id команды и удаляемая сессия. Диалог остаётся открытым,
// пока агент не ответит, чтобы ошибку было видно.
let deleteId = null;
let deleteRequest = null;
// Закрытие консоли сессии без подтверждения: id команд, ожидающих ответа.
const closeIds = new Set();
// Переименование сессии: id команд, ожидающих ответа агента.
const renameIds = new Set();
// Прерывание хода: id команды, чтобы показать результат именно для неё.
let interruptId = null;
// Запросы opencode, ожидающие ответа: разрешения и формы с вариантами.
let promptSig = '';
let promptUid = 0;
const promptState = new Map(); // id запроса -> значения полей
const promptControls = new Map(); // id запроса -> Map(ключ поля -> элемент)
const promptCards = new Map(); // id запроса -> карточка в DOM
const promptSending = new Set(); // id запросов, ответ на которые отправляется
const promptReplyIds = new Map(); // id команды -> id запроса
// Каталоги, показанные в подсказках для текущего выбранного компьютера.
let launchDirCache = { agent: '', dirs: [] };

// Команды сессий (`/init`, `/clear`) для списка, который открывается по `/`:
// ключ — «агент\nсессия». Список присылает агент, он же выполняет команду.
const sessionCommands = new Map();
// Когда команды уже запрошены: чтобы не спрашивать агента на каждое нажатие.
const commandsAsked = new Map();
// Модели сессий opencode для окна выбора в `/models`.
const sessionModels = new Map();
const modelsAsked = new Map();
// Для какой сессии открыто окно выбора модели и что в нём показано.
let modelsSession = null; // { agent, session, current }
// Отправленная смена модели: id команды и подпись выбранной модели.
let modelSet = null;

// История запросов для стрелок вверх/вниз в поле ввода.
// Основной источник — лента сессии, поэтому в историю попадают и сообщения,
// отправленные из окна консоли или с другого устройства.
const feedRequests = new Map(); // id элемента ленты -> текст запроса
// Вызов инструмента, к которому относится результат: по нему видно, что за
// вывод (файл из Read, команда Bash) и как его подсветить.
const toolCalls = new Map(); // id вызова -> элемент ленты tool_use
const resultCalls = new Map(); // id результата -> элемент ленты tool_use
let pendingCalls = []; // вызовы Claude Code, результата которых ещё не было

// Подагенты открытой сессии. Их вызовы в ленту не попадают (их прячет агент),
// поэтому список над строкой хода — единственный вход в ленту подагента.
let subagents = []; // [{ id, title, agent, running, started_at }]
let subTab = ''; // '' — основная лента сессии, иначе id подагента в активной вкладке
let subTabs = []; // id подагентов, открытых вкладками, в порядке открытия
// Прокрутка и раскрытые вызовы по вкладкам: при переключении лента собирается
// заново из кэша, и место чтения должно сохраниться. Ключ — '' или id подагента.
const tabScroll = new Map();
const tabOpen = new Map();
// Отправленные из этого браузера, но ещё не пришедшие в ленту.
const sentRequests = new Map(); // "агент\nсессия" -> [текст, ...]
let histPos = 0; // 0 — черновик, 1 — последний запрос, 2 — предыдущий…
let histDraft = ''; // что было в поле до начала просмотра истории
