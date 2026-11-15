'use strict';

// Небольшой рендерер Markdown. Строит DOM-узлы напрямую и нигде не использует
// innerHTML, поэтому HTML из текста сообщения остаётся обычным текстом.

const renderMarkdown = (() => {
  const node = (tag, text) => {
    const n = document.createElement(tag);
    if (text !== undefined) n.textContent = text;
    return n;
  };

  // Символы имени файла или каталога в пути: латиница, цифры, кириллица, `.-_+@`.
  const PATH_CH = '\\w\\u0400-\\u04FF.@+\\-';

  const INLINE = new RegExp(
    [
      '\\\\([\\\\`*_{}\\[\\]()#+\\-.!|~>])', // 1: экранированный символ
      '(`+)([\\s\\S]*?[^`])\\2(?!`)', // 2, 3: код
      '\\*\\*([^\\s*](?:[\\s\\S]*?[^\\s])?)\\*\\*', // 4: жирный
      '__([^\\s_](?:[\\s\\S]*?[^\\s])?)__(?![A-Za-zА-Яа-яЁё0-9])', // 5: жирный
      '~~([^\\s~](?:[\\s\\S]*?[^\\s])?)~~', // 6: зачёркнутый
      '\\*([^\\s*](?:[^*]*?[^\\s*])?)\\*', // 7: курсив
      '(?<![A-Za-zА-Яа-яЁё0-9_])_([^\\s_](?:[^_]*?[^\\s_])?)_(?![A-Za-zА-Яа-яЁё0-9_])', // 8: курсив
      '\\[([^\\]\\n]+)\\]\\(([^)\\s]+)\\)', // 9, 10: ссылка
      '(https?:\\/\\/[^\\s<>"\'`]*[^\\s<>"\'`.,;:!?)\\]])', // 11: адрес
      '!\\[([^\\]\\n]*)\\]\\(([^)\\s]+)\\)', // 12, 13: картинка
      // 14: путь к файлу картинки на машине агента (`C:\x\shot.png`, `./a.png`, `/tmp/b.jpg`)
      `(?<![${PATH_CH}/\\\\:~])((?:[A-Za-z]:[\\\\/]|~[\\\\/]|\\.{1,2}[\\\\/]|\\/)?(?:[${PATH_CH}]+[\\\\/])*` +
        `[${PATH_CH}]+\\.(?:png|jpe?g|gif|webp|avif|bmp|svg|ico))(?![\\w\\u0400-\\u04FF/\\\\])`,
    ].join('|'),
    'gi',
  );

  const RE_IMAGE_PATH = /\.(png|jpe?g|gif|webp|avif|bmp|svg|ico)$/i;
  // Путь целиком (в `коде` или в ссылке): тут допустимы и пробелы.
  const RE_LOCAL_PATH = /^(?:file:\/\/\/?)?[^<>"|*?\n]*[^\s<>"|*?\\/]\.(png|jpe?g|gif|webp|avif|bmp|svg|ico)$/i;

  // Адрес картинки с машины агента; задаётся на время одного вызова renderMarkdown.
  let localImage = null;
  // С пробелами — только явный путь (`C:\My Pictures\a.png`), иначе
  // `convert a.png b.png` из команды стал бы одним «путём».
  const RE_EXPLICIT_PATH = /^(?:file:\/\/\/?)?(?:[A-Za-z]:[\\/]|\/|~[\\/]|\.{1,2}[\\/])/;
  const isLocalPath = (s) => !!localImage && !/^[a-z][a-z0-9+.-]+:\/\//i.test(s.replace(/^file:/i, '')) &&
    RE_LOCAL_PATH.test(s) && (!/\s/.test(s) || RE_EXPLICIT_PATH.test(s));

  // Путь к картинке становится ссылкой-переключателем на /api/image.
  function localLink(content, path) {
    const a = node('a');
    a.append(content);
    a.href = localImage(path);
    a.target = '_blank';
    a.rel = 'noopener noreferrer';
    a.dataset.local = '1';
    imageToggle(a);
    return a;
  }

  // Ссылка ведёт на картинку, если путь в адресе кончается расширением картинки
  // (параметры запроса и якорь не мешают: `photo.jpg?size=large`).
  function isImageUrl(href) {
    try {
      const url = new URL(href);
      return /^https?:$/.test(url.protocol) && RE_IMAGE_PATH.test(url.pathname);
    } catch {
      return false;
    }
  }

  function link(text, href) {
    if (isLocalPath(href)) return localLink(text, href);
    if (!/^(https?:|mailto:)/i.test(href)) return document.createTextNode(text);
    const a = node('a', text);
    a.href = href;
    a.target = '_blank';
    a.rel = 'noopener noreferrer';
    if (isImageUrl(href)) imageToggle(a);
    return a;
  }

  // Ссылка на картинку раскрывает под собой блок с ней. Картинка грузится только
  // по клику: чужой сайт не узнаёт об открытии ленты. Ctrl/Cmd/Shift-клик и
  // средняя кнопка по-прежнему открывают адрес в новой вкладке.
  function imageToggle(a) {
    a.classList.add('img-link');
    a.setAttribute('aria-expanded', 'false');
    a.title = 'Показать картинку';
    a.addEventListener('click', (e) => {
      if (e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
      e.preventDefault();
      setImageOpen(a, a.getAttribute('aria-expanded') !== 'true');
    });
  }

  function setImageOpen(a, open) {
    if (!a.classList.contains('img-link')) return;
    let box = a.imgPreview;
    if (open && !box) {
      box = node('span');
      box.className = 'img-preview';
      const full = node('a');
      full.href = a.href;
      full.target = '_blank';
      full.rel = 'noopener noreferrer';
      full.title = 'Открыть в новой вкладке';
      const img = node('img');
      img.alt = a.textContent;
      img.referrerPolicy = 'no-referrer';
      img.decoding = 'async';
      const fail = (why) => box.replaceChildren(node('span', why || 'Не удалось загрузить картинку'));
      img.addEventListener('error', () => fail(), { once: true });
      if (a.dataset.local) {
        // Картинку с машины агента берём через fetch: так видна причина ошибки
        // (файла нет, агент не в сети), а не просто сломанная картинка.
        box.append(node('span', 'Загрузка…'));
        fetch(a.href, { credentials: 'same-origin' })
          .then(async (r) => {
            if (!r.ok) throw new Error((await r.text()) || `ошибка ${r.status}`);
            return r.blob();
          })
          .then((blob) => {
            img.src = URL.createObjectURL(blob);
            box.replaceChildren(full);
          })
          .catch((e) => fail(`Не удалось загрузить картинку: ${e.message}`));
      } else {
        img.src = a.href;
        box.append(full);
      }
      full.append(img);
      a.after(box);
      a.imgPreview = box;
    }
    if (box) box.hidden = !open;
    a.setAttribute('aria-expanded', String(open));
    a.title = open ? 'Скрыть картинку' : 'Показать картинку';
  }

  function imageLink(alt, href) {
    if (isLocalPath(href)) return localLink(alt || href.split(/[\\/]/).pop(), href);
    const a = link(alt || href.split(/[?#]/)[0].split('/').pop() || 'картинка', href);
    // `![](…)` без расширения в адресе — тоже картинка, раз так написал автор.
    if (a.nodeType === Node.ELEMENT_NODE && !a.classList.contains('img-link') && /^https?:/i.test(href)) {
      imageToggle(a);
    }
    return a;
  }

  function inline(parent, text) {
    let last = 0;
    // matchAll работает с копией выражения: рекурсивные вызовы не сбивают позицию поиска.
    for (const m of text.matchAll(INLINE)) {
      if (m.index > last) parent.append(text.slice(last, m.index));
      last = m.index + m[0].length;
      if (m[1] !== undefined) parent.append(m[1]);
      else if (m[3] !== undefined) {
        const code = node('code', m[3].trim() || m[3]);
        parent.append(isLocalPath(code.textContent) ? localLink(code, code.textContent) : code);
      }
      else if (m[4] !== undefined || m[5] !== undefined) inline(parent.appendChild(node('strong')), m[4] ?? m[5]);
      else if (m[6] !== undefined) inline(parent.appendChild(node('del')), m[6]);
      else if (m[7] !== undefined || m[8] !== undefined) inline(parent.appendChild(node('em')), m[7] ?? m[8]);
      else if (m[9] !== undefined) parent.append(link(m[9], m[10]));
      else if (m[11] !== undefined) parent.append(link(m[11], m[11]));
      else if (m[13] !== undefined) parent.append(imageLink(m[12], m[13]));
      else if (m[14] !== undefined) parent.append(localImage ? localLink(m[14], m[14]) : m[14]);
    }
    if (last < text.length) parent.append(text.slice(last));
  }

  // Делит строку таблицы на ячейки; `|` внутри кода и после `\` не считается разделителем.
  function cells(line) {
    const out = [];
    let cur = '';
    let code = false;
    const s = line.trim();
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      if (c === '\\' && s[i + 1] === '|') {
        cur += '|';
        i++;
      } else if (c === '`') {
        code = !code;
        cur += c;
      } else if (c === '|' && !code) {
        out.push(cur);
        cur = '';
      } else {
        cur += c;
      }
    }
    out.push(cur);
    if (s.startsWith('|')) out.shift();
    if (s.endsWith('|') && !s.endsWith('\\|')) out.pop();
    return out.map((c) => c.trim());
  }

  const RE_FENCE = /^\s{0,3}(```+|~~~+)\s*([^\s`]*)/;
  const RE_HEADING = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
  const RE_HR = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/;
  const RE_LIST = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
  const RE_QUOTE = /^\s{0,3}>\s?(.*)$/;
  const RE_TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

  const isTableStart = (lines, i) =>
    lines[i].includes('|') && i + 1 < lines.length && lines[i + 1].includes('-') && RE_TABLE_SEP.test(lines[i + 1]) &&
    cells(lines[i]).length === cells(lines[i + 1]).length;

  const startsBlock = (lines, i) =>
    RE_FENCE.test(lines[i]) || RE_HEADING.test(lines[i]) || RE_HR.test(lines[i]) || RE_LIST.test(lines[i]) ||
    RE_QUOTE.test(lines[i]) || isTableStart(lines, i);

  function table(lines, i, parent) {
    const aligns = cells(lines[i + 1]).map((c) =>
      c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'right' : '');
    const tbl = node('table');
    const row = (line, tag) => {
      const tr = node('tr');
      const data = cells(line);
      aligns.forEach((align, k) => {
        const cell = node(tag);
        if (align) cell.classList.add(align);
        inline(cell, data[k] || '');
        tr.append(cell);
      });
      return tr;
    };
    tbl.appendChild(node('thead')).append(row(lines[i], 'th'));
    const body = tbl.appendChild(node('tbody'));
    i += 2;
    while (i < lines.length && lines[i].trim() && lines[i].includes('|')) body.append(row(lines[i++], 'td'));
    const wrap = node('div');
    wrap.className = 'table-wrap';
    wrap.append(tbl);
    parent.append(wrap);
    return i;
  }

  function list(lines, i, parent) {
    // Стек открытых списков: отступ маркера и элемент списка.
    const stack = [];
    let item = null;
    while (i < lines.length) {
      const m = RE_LIST.exec(lines[i]);
      if (m) {
        const indent = m[1].replace(/\t/g, '    ').length;
        const ordered = /\d/.test(m[2]);
        while (stack.length > 1 && indent < stack[stack.length - 1].indent) stack.pop();
        let top = stack[stack.length - 1];
        const deeper = !top || indent >= top.indent + 2;
        if (deeper || top.ordered !== ordered) {
          // Новый уровень вложенности либо смена вида списка на том же уровне.
          const host = deeper ? item || parent : top.host;
          if (!deeper) stack.pop();
          const el = node(ordered ? 'ol' : 'ul');
          if (ordered && parseInt(m[2], 10) !== 1) el.start = parseInt(m[2], 10);
          host.append(el);
          top = { indent, el, ordered, host };
          stack.push(top);
        }
        item = node('li');
        const task = /^\[([ xX])\]\s+(.*)$/.exec(m[3]);
        if (task) {
          const box = node('input');
          box.type = 'checkbox';
          box.disabled = true;
          box.checked = task[1] !== ' ';
          item.append(box, ' ');
          inline(item, task[2]);
        } else {
          inline(item, m[3]);
        }
        top.el.append(item);
        i++;
      } else if (lines[i].trim() && /^\s+/.test(lines[i]) && item && !RE_FENCE.test(lines[i])) {
        item.append('\n');
        inline(item, lines[i].trim());
        i++;
      } else if (!lines[i].trim() && i + 1 < lines.length && RE_LIST.test(lines[i + 1])) {
        i++; // пустая строка между пунктами одного списка
      } else {
        break;
      }
    }
    return i;
  }

  function blocks(lines, parent) {
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      let m;
      if (!line.trim()) {
        i++;
      } else if ((m = RE_FENCE.exec(line))) {
        const close = new RegExp(`^\\s{0,3}${m[1][0]}{${m[1].length},}\\s*$`);
        const code = [];
        i++;
        while (i < lines.length && !close.test(lines[i])) code.push(lines[i++]);
        i++; // закрывающая строка; незакрытый блок идёт до конца текста
        const pre = node('pre');
        pre.append(node('code', code.join('\n')));
        parent.append(pre);
      } else if ((m = RE_HEADING.exec(line))) {
        inline(parent.appendChild(node(`h${m[1].length}`)), m[2]);
        i++;
      } else if (RE_HR.test(line)) {
        parent.append(node('hr'));
        i++;
      } else if (isTableStart(lines, i)) {
        i = table(lines, i, parent);
      } else if (RE_QUOTE.test(line)) {
        const inner = [];
        while (i < lines.length && (m = RE_QUOTE.exec(lines[i]))) {
          inner.push(m[1]);
          i++;
        }
        blocks(inner, parent.appendChild(node('blockquote')));
      } else if (RE_LIST.test(line)) {
        i = list(lines, i, parent);
      } else {
        const para = [line];
        i++;
        while (i < lines.length && lines[i].trim() && !startsBlock(lines, i)) para.push(lines[i++]);
        inline(parent.appendChild(node('p')), para.join('\n'));
      }
    }
  }

  // opts.localImage(path) — адрес картинки с машины агента для пути из текста;
  // без него пути к файлам остаются обычным текстом.
  const render = (text, opts = {}) => {
    const root = document.createDocumentFragment();
    localImage = typeof opts.localImage === 'function' ? opts.localImage : null;
    try {
      blocks(text.replace(/\r\n?/g, '\n').split('\n'), root);
    } finally {
      localImage = null;
    }
    return root;
  };
  // Сообщение при обновлении строится заново: так раскрытые картинки переносятся
  // из старого узла в новый.
  render.keepOpenImages = (from, to) => {
    const was = [...from.querySelectorAll('a.img-link')];
    const now = [...to.querySelectorAll('a.img-link')];
    was.forEach((a, i) => {
      if (a.getAttribute('aria-expanded') === 'true' && now[i] && now[i].href === a.href) setImageOpen(now[i], true);
    });
  };
  return render;
})();
