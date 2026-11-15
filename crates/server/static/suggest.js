/* Выпадающие подсказки под полем ввода.
 *
 * Замена <datalist>: браузер показывает его варианты только пока введённое
 * значение совпадает с началом пункта, поэтому как только в поле появляется
 * путь, артикул или название, список исчезает. Здесь варианты ищутся по
 * подстроке (без учёта регистра, разделителей пути и лишних пробелов),
 * открываются по клику и при вводе, выбираются мышью и клавиатурой.
 *
 * Зависимостей нет, стили (.sg-*) модуль добавляет сам и подстраивает под
 * переменные --panel, --line, --text, --muted, --tool, если они заданы.
 *
 *   const suggest = Suggest.attach(input, {
 *     items: () => [{ value: 'D:\Projects\demo', label: 'D:\Projects\demo', hint: 'opencode' }],
 *     // необязательные настройки:
 *     filter: (item, query) => true,      // query уже приведён к нижнему регистру
 *     render: (item, query) => '<b>…</b>',
 *     onSelect: (item) => {},
 *     minChars: 1,                        // сколько символов ввести до открытия
 *     limit: 12,                          // сколько пунктов показывать (0 — все)
 *     openOnFocus: true,                  // открывать при получении фокуса
 *     openAllWhenExact: false,            // клик по полю с готовым значением — весь список
 *     emptyText: 'Ничего не найдено',     // пункт-заглушка, '' — не показывать
 *     emptyOnQueryless: false,            // заглушка и при пустом запросе («/» — все пункты)
 *     token: (input) => ({ query, start, end }), // режим части поля: подсказки по слову под
 *                                         // курсором, выбор заменяет только его (null — нет слова)
 *     text: (item) => '/init ',           // что вставить вместо слова (по умолчанию value)
 *   });
 *
 * Возвращает { open, close, isOpen, refresh, element, destroy }.
 */
(function (global) {
  'use strict';

  let seq = 0;

  const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
  const esc = (text) => String(text).replace(/[&<>"']/g, (c) => ESC[c]);

  // Сравнение не должно зависеть от регистра, типа разделителя пути и пробелов:
  // «D:\Projects\», «d:/projects» и «D:/ Projects» — одно и то же.
  const norm = (text) => String(text).toLowerCase().replace(/[\\/]+/g, '/').replace(/\s+/g, ' ').trim();

  const valueOf = (item) => (item && typeof item === 'object' ? (item.value == null ? '' : String(item.value)) : String(item));
  const labelOf = (item) => (item && typeof item === 'object' ? (item.label == null ? valueOf(item) : String(item.label)) : String(item));
  const hintOf = (item) => (item && typeof item === 'object' && item.hint ? String(item.hint) : '');

  // Подсветка найденного фрагмента: текст экранируется по частям.
  function mark(text, query) {
    if (!query) return esc(text);
    const src = String(text);
    const needle = query.toLowerCase();
    let out = '';
    let from = 0;
    for (;;) {
      const at = src.toLowerCase().indexOf(needle, from);
      if (at < 0) {
        out += esc(src.slice(from));
        break;
      }
      out += esc(src.slice(from, at)) + '<b>' + esc(src.slice(at, at + needle.length)) + '</b>';
      from = at + needle.length;
    }
    return out;
  }

  // Стили добавляются один раз на документ. Тег <style> не годится: CSP вида
  // `default-src 'self'` (так настроена панель) блокирует инлайн-стили, и список
  // остаётся без отступов и рамки. Constructable stylesheet под CSP разрешён.
  let sheet = null;
  function injectStyle() {
    if (sheet) return;
    const css = `
.sg-host { position: relative; }
.sg-list {
  /* Координаты ставит JS: список не должен обрезаться контейнером с прокруткой
     (у <dialog> свой overflow), поэтому position: fixed, а не absolute. */
  position: fixed; left: 0; top: 0; z-index: 1000;
  max-height: min(60vh, 340px); overflow: auto; padding: 4px 0;
  background: var(--panel, #fff); color: var(--text, #1c1c1a);
  border: 1px solid var(--line, #dcdcd6); border-radius: 8px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, .18);
  font-size: 14px; line-height: 1.4;
}
.sg-item { padding: 9px 12px; }
.sg-empty { padding: 9px 12px; }
.sg-item { cursor: pointer; }
.sg-item.on, .sg-item:hover { background: var(--tool, #f0f0ec); }
.sg-item b { font-weight: 600; overflow-wrap: anywhere; }
.sg-item small {
  display: block; color: var(--muted, #6d6d66); font-size: 12px;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.sg-empty { color: var(--muted, #6d6d66); font-size: 13px; cursor: default; }
`;
    if (typeof CSSStyleSheet === 'function' && CSSStyleSheet.prototype.replaceSync && 'adoptedStyleSheets' in document) {
      sheet = new CSSStyleSheet();
      sheet.replaceSync(css);
      document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
      return;
    }
    sheet = document.createElement('style');
    sheet.textContent = css;
    document.head.appendChild(sheet);
  }

  function attach(input, options) {
    const opts = options || {};
    injectStyle();

    const id = 'sg' + ++seq;
    const host = opts.container || input.parentElement || document.body;
    host.classList.add('sg-host');

    const list = document.createElement('div');
    list.className = 'sg-list';
    list.id = id + '-list';
    list.hidden = true;
    list.setAttribute('role', 'listbox');
    host.appendChild(list);

    input.setAttribute('role', 'combobox');
    input.setAttribute('aria-autocomplete', 'list');
    input.setAttribute('aria-expanded', 'false');
    input.setAttribute('aria-controls', list.id);
    if (!input.hasAttribute('autocomplete')) input.setAttribute('autocomplete', 'off');

    const minChars = opts.minChars == null ? 1 : opts.minChars;
    const limit = opts.limit == null ? 12 : opts.limit;

    let rows = [];      // пункты, прошедшие фильтр
    let pos = -1;       // выбранный стрелками пункт
    let open = false;
    let token = 0;      // отбрасывает устаревшие ответы асинхронного items()
    let alive = true;

    // В режиме token подсказки относятся к слову под курсором (например, к
    // команде после `/`), а не ко всему значению поля. Если слова нет — null.
    function word() {
      if (!opts.token) return { query: norm(input.value), start: 0, end: input.value.length };
      let t;
      try {
        t = opts.token(input);
      } catch (e) {
        return null;
      }
      if (!t) return null;
      return { query: norm(t.query), start: t.start, end: t.end };
    }

    function matches(item, q) {
      if (opts.filter) return !!opts.filter(item, q);
      return norm(labelOf(item) + ' ' + hintOf(item) + ' ' + valueOf(item)).includes(q);
    }

    function paint() {
      const nodes = list.querySelectorAll('.sg-item');
      nodes.forEach((node, n) => {
        const on = n === pos;
        node.classList.toggle('on', on);
        node.setAttribute('aria-selected', on ? 'true' : 'false');
      });
      if (pos >= 0 && nodes[pos]) {
        input.setAttribute('aria-activedescendant', nodes[pos].id);
        nodes[pos].scrollIntoView({ block: 'nearest' });
      } else {
        input.removeAttribute('aria-activedescendant');
      }
    }

    function setPos(n) {
      pos = n;
      paint();
    }

    function close() {
      list.hidden = true;
      list.replaceChildren();
      rows = [];
      pos = -1;
      open = false;
      input.setAttribute('aria-expanded', 'false');
      input.removeAttribute('aria-activedescendant');
    }

    function choose(n) {
      const item = rows[n];
      if (n < 0 || !item) return;
      if (opts.token) {
        const w = word();
        if (!w) return;
        const text = opts.text ? opts.text(item) : valueOf(item);
        input.value = input.value.slice(0, w.start) + text + input.value.slice(w.end);
        const caret = w.start + text.length;
        close();
        try {
          input.setSelectionRange(caret, caret);
        } catch {}
        // Программная вставка не порождает input: другие обработчики поля
        // (рост высоты, сброс истории) должны узнать об изменении.
        input.dispatchEvent(new Event('input', { bubbles: true }));
      } else {
        input.value = valueOf(item);
        close();
      }
      if (opts.onSelect) opts.onSelect(item);
    }

    function show(list0, q) {
      rows = list0;
      pos = -1;
      list.replaceChildren();

      if (!rows.length) {
        const emptyText = typeof opts.emptyText === 'function' ? opts.emptyText() : opts.emptyText;
        if (!emptyText || (!q && !opts.emptyOnQueryless)) {
          close();
          return;
        }
        const empty = document.createElement('div');
        empty.className = 'sg-empty';
        empty.textContent = emptyText;
        list.appendChild(empty);
      } else {
        rows.forEach((item, n) => {
          const node = document.createElement('div');
          node.className = 'sg-item';
          node.id = id + '-i' + n;
          node.setAttribute('role', 'option');
          node.setAttribute('aria-selected', 'false');
          node.dataset.n = String(n);
          node.innerHTML = opts.render
            ? opts.render(item, q)
            : '<b>' + mark(labelOf(item), q) + '</b>' + (hintOf(item) ? '<small>' + esc(hintOf(item)) + '</small>' : '');
          list.appendChild(node);
        });
      }

      list.hidden = false;
      open = true;
      input.setAttribute('aria-expanded', 'true');
      input.removeAttribute('aria-activedescendant');
      place();
    }

    // Список позиционируется по окну просмотра, а не по родителю: у <dialog> и
    // других контейнеров свой overflow, absolute-список в них обрезался бы и
    // получал внутреннюю прокрутку. Высоту ограничиваем свободным местом, а если
    // внизу его мало — раскрываем вверх.
    function place() {
      if (list.hidden) return;
      const r = input.getBoundingClientRect();
      const vh = window.innerHeight;
      const below = vh - r.bottom - 10;
      const above = r.top - 10;
      const up = below < 160 && above > below;
      const room = Math.min(340, Math.max(96, up ? above : below));
      list.style.width = Math.round(r.width) + 'px';
      list.style.left = Math.round(r.left) + 'px';
      list.style.maxHeight = Math.round(room) + 'px';
      if (up) {
        list.style.top = 'auto';
        list.style.bottom = Math.round(vh - r.top + 4) + 'px';
      } else {
        list.style.bottom = 'auto';
        list.style.top = Math.round(r.bottom + 4) + 'px';
      }
    }

    function openList(forceAll) {
      if (!alive) return;
      const w = word();
      if (!w || w.query.length < minChars) {
        close();
        return;
      }
      let source;
      try {
        source = opts.items ? opts.items() : [];
      } catch (e) {
        close();
        return;
      }
      const mine = ++token;
      if (source && typeof source.then === 'function') {
        source.then((all) => {
          if (alive && mine === token) build(all, forceAll);
        }, () => {
          if (alive && mine === token) close();
        });
        return;
      }
      build(source, forceAll);
    }

    // forceAll: поле уже заполнено готовым значением (клик или фокус) — показываем
    // весь список, а не единственный пункт, совпавший с введённым целиком.
    function build(all, forceAll) {
      const items = Array.isArray(all) ? all : [];
      const w = word();
      if (!w) {
        close();
        return;
      }
      let q = w.query;
      if (forceAll && opts.openAllWhenExact && items.some((it) => norm(valueOf(it)) === q || norm(labelOf(it)) === q)) {
        q = '';
      }
      let list0 = items.filter((item) => matches(item, q));
      if (typeof opts.rank === 'function') {
        list0 = list0.slice().sort((a, b) => opts.rank(a, q) - opts.rank(b, q));
      }
      if (limit > 0) list0 = list0.slice(0, limit);
      show(list0, q);
    }

    function onInput() {
      openList(false);
    }
    function onFocus() {
      if (opts.openOnFocus !== false) openList(true);
    }
    function onClick() {
      openList(true);
    }
    function onKeyDown(e) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        // Поле в режиме слова и не на слове: стрелки принадлежат ему (история
        // сообщений в поле ввода), список тут ни при чём.
        if (!open && opts.token && !word()) return;
        e.preventDefault();
        if (!open) {
          openList(true);
          return;
        }
        if (!rows.length) return;
        setPos((pos + (e.key === 'ArrowDown' ? 1 : -1) + rows.length) % rows.length);
      } else if (e.key === 'Enter') {
        // Без стрелок Enter отправляет форму: так поле не отнимает привычный submit.
        if (open && pos >= 0 && rows[pos]) {
          e.preventDefault();
          choose(pos);
        }
      } else if (e.key === 'Escape') {
        if (open) {
          // Иначе Escape закрыл бы родительский <dialog>.
          e.preventDefault();
          e.stopPropagation();
          close();
        }
      } else if (e.key === 'Tab') {
        close();
      }
    }
    function onPointerDown(e) {
      if (e.target === input || host.contains(e.target)) return;
      close();
    }
    function onViewportChange() {
      place();
    }
    function onListDown(e) {
      const node = e.target.closest('.sg-item');
      if (!node) return;
      e.preventDefault(); // фокус остаётся в поле ввода
      choose(Number(node.dataset.n));
    }
    // На тач-экранах mousedown приходит не всегда: дублируем выбор по click.
    function onListClick(e) {
      const node = e.target.closest('.sg-item');
      if (!node || list.hidden) return;
      choose(Number(node.dataset.n));
    }

    input.addEventListener('input', onInput);
    input.addEventListener('focus', onFocus);
    input.addEventListener('click', onClick);
    input.addEventListener('keydown', onKeyDown);
    list.addEventListener('mousedown', onListDown);
    list.addEventListener('click', onListClick);
    document.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('resize', onViewportChange);
    window.addEventListener('scroll', onViewportChange, true);
    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', onViewportChange);
      window.visualViewport.addEventListener('scroll', onViewportChange);
    }

    return {
      open: () => openList(true),
      close,
      isOpen: () => open,
      refresh() {
        if (open) openList(false);
      },
      element: list,
      destroy() {
        alive = false;
        close();
        input.removeEventListener('input', onInput);
        input.removeEventListener('focus', onFocus);
        input.removeEventListener('click', onClick);
        input.removeEventListener('keydown', onKeyDown);
        list.removeEventListener('mousedown', onListDown);
        list.removeEventListener('click', onListClick);
        document.removeEventListener('pointerdown', onPointerDown, true);
        window.removeEventListener('resize', onViewportChange);
        window.removeEventListener('scroll', onViewportChange, true);
        if (window.visualViewport) {
          window.visualViewport.removeEventListener('resize', onViewportChange);
          window.visualViewport.removeEventListener('scroll', onViewportChange);
        }
        list.remove();
        input.removeAttribute('role');
        input.removeAttribute('aria-autocomplete');
        input.removeAttribute('aria-expanded');
        input.removeAttribute('aria-controls');
      },
    };
  }

  global.Suggest = { attach };
})(window);
