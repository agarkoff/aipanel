'use strict';

// Снимок экрана машины агента: серия кадров, листание и приближение
// щипком. Снимает агент, а не браузер — он знает свои мониторы. Тем же
// полноэкранным просмотром открывается картинка из ленты: один кадр без
// листания, но с тем же щипком, колесом и панорамой.

// ------------------------------------------------------------ снимок экрана

// Значок панели в шапке снимает экран машины агента и открывает снимок поверх
// панели. Снимает агент, а не браузер: браузер не перечисляет мониторы и не
// отдаёт их без разрешения на каждый запрос. Экранов может быть несколько —
// между ними листают стрелками, свайпом или точками, а щипок двумя пальцами
// приближает кадр.

// Компьютер прошлого снимка: когда открытой сессии нет, а агентов несколько,
// панель спрашивает, чей экран, и запоминает выбор.
const SHOT_AGENT_KEY = 'aipanel.shot-agent';
// Старая версия агента команду не знает и молчит вовсе, поэтому ждём не вечно.
const SHOT_TIMEOUT = 25000;
// Сколько экранов принимаем: столько же, сколько пропускает сервер.
const SHOT_MAX = 16;
// Во сколько раз можно приблизить снимок: мельче пикселей он всё равно не станет.
const SHOT_ZOOM_MAX = 6;

let shot = null; // { agent, id, total, index, pages, nodes, timer, done, error }
// Приближение: масштаб и сдвиг от центра. Относится только к текущему экрану —
// при листании и закрытии снимок снова показывается целиком.
let shotZoom = { scale: 1, x: 0, y: 0 };

function openShots() {
  const online = agents.filter((a) => a.online);
  if (!online.length) {
    setHint('Снимок экрана: агент не в сети.', true);
    return;
  }
  // Сначала компьютер открытой сессии: снимок обычно нужен там, где идёт работа.
  const cur = selected && online.find((a) => a.name === selected.agent);
  if (cur) {
    startShots(cur.name);
    return;
  }
  if (online.length === 1) {
    startShots(online[0].name);
    return;
  }
  askShotAgent(online);
}

function askShotAgent(online) {
  const list = $('shot-agent-list');
  const last = localStorage.getItem(SHOT_AGENT_KEY) || '';
  list.replaceChildren();
  // Прошлый выбор — первым: чаще всего снимают ту же машину.
  const sorted = [...online].sort((a, b) => (b.name === last) - (a.name === last));
  for (const a of sorted) {
    const btn = el('button', 'model', a.name);
    btn.type = 'button';
    if (a.name === last) btn.classList.add('current');
    btn.addEventListener('click', () => {
      $('shot-agent').close();
      startShots(a.name);
    });
    list.append(btn);
  }
  $('shot-agent').showModal();
}

function startShots(agent) {
  closeShots();
  $('shot').setAttribute('aria-label', 'Снимок экрана');
  shot = {
    agent,
    id: String(++cmdSeq),
    total: 0,
    index: 0,
    pages: [],
    nodes: [],
    timer: null,
    done: false,
    error: '',
  };
  localStorage.setItem(SHOT_AGENT_KEY, agent);
  renderShot();
  if (!$('shot').open) $('shot').showModal();
  if (!send({ t: 'screens', id: shot.id, agent })) {
    shotError('нет соединения с сервером');
    return;
  }
  shot.timer = setTimeout(() => {
    if (shot && !shot.done) {
      shotError('агент не ответил: возможно, его нужно обновить до версии со снимком экрана');
    }
  }, SHOT_TIMEOUT);
}

function closeShots() {
  if (shot) clearTimeout(shot.timer);
  shot = null;
  resetShotZoom();
  if ($('shot').open) $('shot').close();
  $('shot-strip').replaceChildren();
  $('shot-dots').replaceChildren();
  $('shot-label').textContent = '';
  $('shot-hint').textContent = '';
}

// Картинка из ленты открывается тем же полноэкранным просмотром, что и снимок
// экрана: щипок, колесо и панорама уже готовы, остаётся подставить один кадр.
// Адрес — готовый (`blob:` у картинки с машины агента, `http(s):` у чужой).
function viewImage(url, label) {
  if (!url) return;
  closeShots();
  $('shot').setAttribute('aria-label', 'Просмотр картинки');
  shot = {
    agent: '',
    id: '',
    total: 1,
    index: 0,
    pages: [{ label: String(label || 'Картинка'), width: 0, height: 0, url: String(url) }],
    nodes: [],
    timer: null,
    done: true,
    error: '',
    image: true,
  };
  if (!$('shot').open) $('shot').showModal();
  renderShot();
}

// Один экран пришёл: ставим его на свою страницу, остальные могут ещё ехать.
function onScreen(msg) {
  if (!shot || msg.id !== shot.id) return;
  const total = Math.max(1, Math.min(SHOT_MAX, Number(msg.count) || 1));
  const index = Math.max(0, Math.min(total - 1, Number(msg.index) || 0));
  const data = String(msg.data || '');
  if (!data) return;
  shot.total = total;
  shot.pages[index] = {
    label: String(msg.label || `Экран ${index + 1}`),
    width: Number(msg.width) || 0,
    height: Number(msg.height) || 0,
    url: `data:image/png;base64,${data}`,
  };
  if (shot.index >= total) shot.index = total - 1;
  renderShot();
}

// Серия снимков закончилась: либо всё снято, либо агент сообщил об ошибке.
function onScreens(msg) {
  if (!shot || msg.id !== shot.id) return;
  shot.done = true;
  clearTimeout(shot.timer);
  shot.timer = null;
  if (msg.error) shot.error = String(msg.error);
  else if (!shot.pages.some(Boolean)) shot.error = 'агент не прислал ни одного экрана';
  renderShot();
}

function shotError(text) {
  if (!shot) return;
  shot.done = true;
  shot.error = text;
  clearTimeout(shot.timer);
  shot.timer = null;
  renderShot();
}

function gotoShot(i) {
  if (!shot) return;
  const count = Math.max(1, shot.total);
  const next = Math.max(0, Math.min(count - 1, i));
  // Приближение относится к прошлому кадру: новый экран показываем целиком.
  if (next !== shot.index) resetShotZoom();
  shot.index = next;
  renderShot();
}

// Картинка текущего экрана: пока агент не ответил, на её месте надпись.
function shotImage() {
  if (!shot) return null;
  const node = shot.nodes[shot.index];
  return node && node.slot instanceof HTMLImageElement ? node.slot : null;
}

function resetShotZoom() {
  shotZoom.scale = 1;
  shotZoom.x = 0;
  shotZoom.y = 0;
}

// Сдвиг не показывает пустоту за краем снимка: за границы картинки панорама не
// уходит, а если снимок у́же или ниже окна — остаётся по центру этой оси.
function clampShotZoom() {
  const stage = $('shot-stage');
  const img = shotImage();
  if (!img) return;
  const maxX = Math.max(0, (img.clientWidth * shotZoom.scale - stage.clientWidth) / 2);
  const maxY = Math.max(0, (img.clientHeight * shotZoom.scale - stage.clientHeight) / 2);
  shotZoom.x = Math.max(-maxX, Math.min(maxX, shotZoom.x));
  shotZoom.y = Math.max(-maxY, Math.min(maxY, shotZoom.y));
}

// Приближение вокруг точки окна: она остаётся на месте, остальное расходится.
// Координаты — от центра сцены, как и сдвиг снимка.
function zoomShotAt(px, py, scale) {
  const next = Math.max(1, Math.min(SHOT_ZOOM_MAX, scale));
  const k = next / shotZoom.scale;
  shotZoom.x = px - k * (px - shotZoom.x);
  shotZoom.y = py - k * (py - shotZoom.y);
  shotZoom.scale = next;
  clampShotZoom();
  applyShotZoom();
}

function applyShotZoom() {
  // Мельче целого экрана снимок не нужен: такой масштаб считаем сбросом.
  if (shotZoom.scale <= 1.001) resetShotZoom();
  const img = shotImage();
  // Снимок мог прийти уже после щипка: панораму в границы ставим здесь.
  if (img) clampShotZoom();
  if (shot) {
    // Приближён только текущий экран: остальные возвращаем к целому кадру.
    for (const node of shot.nodes) {
      if (node.slot instanceof HTMLImageElement && node.slot !== img && node.slot.style.transform) {
        node.slot.style.transform = '';
      }
    }
  }
  if (img) {
    img.style.transform = shotZoom.scale > 1
      ? `translate(${shotZoom.x}px, ${shotZoom.y}px) scale(${shotZoom.scale})`
      : '';
  }
  updateShotLabel();
}

function updateShotLabel() {
  if (!shot) return;
  const count = Math.max(1, shot.total);
  const page = shot.pages[shot.index];
  const zoomed = shotZoom.scale > 1 ? ` · ${Math.round(shotZoom.scale * 100)}%` : '';
  // У картинки из ленты размеров до загрузки нет: подпись — только её имя.
  const size = page && page.width && page.height ? ` · ${page.width}×${page.height}` : '';
  let text;
  if (page) text = `${page.label}${size}${zoomed}`;
  else if (count > 1) text = `Экран ${shot.index + 1} из ${count}${zoomed}`;
  else text = `Снимаю экран…${zoomed}`;
  // Подпись обновляется на каждом движении пальца: лишнюю перерисовку не делаем.
  const label = $('shot-label');
  if (label.textContent !== text) label.textContent = text;
}

function renderShot() {
  if (!shot) return;
  const strip = $('shot-strip');
  const count = Math.max(1, shot.total);
  // Страницы создаются, когда известно число экранов: до первого ответа она одна
  // и показывает, что идёт съёмка.
  if (shot.nodes.length !== count) {
    strip.replaceChildren();
    shot.nodes = [];
    resetShotZoom();
    for (let i = 0; i < count; i++) {
      const page = el('div', 'shot-page');
      const slot = el('span', 'shot-wait', 'Снимаю экран…');
      page.append(slot);
      strip.append(page);
      shot.nodes.push({ page, slot, url: '' });
    }
  }
  for (let i = 0; i < shot.pages.length; i++) {
    const data = shot.pages[i];
    const node = shot.nodes[i];
    if (!data || !node || node.url === data.url) continue;
    const img = document.createElement('img');
    img.src = data.url;
    img.alt = data.label;
    img.draggable = false;
    // Размер настоящей картинки дописываем в подпись после загрузки: у снимка
    // его присылает агент, а у картинки из ленты он виден только здесь.
    img.addEventListener('load', () => {
      if (!shot || shot.pages[i] !== data) return;
      if (data.width && data.height) return;
      data.width = img.naturalWidth;
      data.height = img.naturalHeight;
      if (shot.index === i) updateShotLabel();
    });
    node.slot.replaceWith(img);
    node.slot = img;
    node.url = data.url;
  }
  strip.classList.remove('dragging');
  strip.style.transform = `translateX(${-shot.index * 100}%)`;

  const many = count > 1;
  // У одиночной картинки снимать заново нечего: кнопка остаётся только у экрана.
  $('shot-refresh').hidden = !!shot.image;
  $('shot-prev').hidden = !many || shot.index <= 0;
  $('shot-next').hidden = !many || shot.index >= count - 1;

  const dots = $('shot-dots');
  if (dots.children.length !== (many ? count : 0)) {
    dots.replaceChildren();
    for (let i = 0; i < count; i++) {
      const dot = el('button', 'shot-dot');
      dot.type = 'button';
      dot.setAttribute('aria-label', `Экран ${i + 1}`);
      dot.addEventListener('click', () => gotoShot(i));
      dots.append(dot);
    }
  }
  [...dots.children].forEach((dot, i) => dot.classList.toggle('active', i === shot.index));

  // Подпись экрана пишет applyShotZoom: в неё входит и текущий масштаб.
  applyShotZoom();
  $('shot-hint').textContent = shot.error;
}

// Жест на снимке: щипок двумя пальцами приближает кадр, приближённый кадр
// двигается одним пальцем, а целый — листается. Короткий сдвиг виден сразу,
// отпускание доводит листание до соседней страницы. Клик мимо картинки
// закрывает просмотр.
function attachShotSwipe(stage) {
  const pointers = new Map(); // pointerId → где палец сейчас
  let startX = 0;
  let startY = 0;
  let baseX = 0; // сдвиг снимка в начале жеста
  let baseY = 0;
  let dx = 0;
  let tracking = false;
  let horizontal = false;
  let panning = false;
  let swiped = false;
  let pinch = null; // { dist, mx, my, scale, x, y } — снимок в начале щипка

  // Палец после жеста не должен закрыть просмотр кликом.
  const markGesture = () => {
    swiped = true;
    setTimeout(() => {
      swiped = false;
    }, 400);
  };

  const stageCenter = () => {
    const r = stage.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  };

  const startPinch = () => {
    const [a, b] = [...pointers.values()];
    pinch = {
      dist: Math.hypot(a.x - b.x, a.y - b.y) || 1,
      mx: (a.x + b.x) / 2,
      my: (a.y + b.y) / 2,
      scale: shotZoom.scale,
      x: shotZoom.x,
      y: shotZoom.y,
    };
    tracking = false;
    horizontal = false;
    panning = false;
    $('shot-strip').classList.remove('dragging');
  };

  const movePinch = () => {
    const [a, b] = [...pointers.values()];
    const dist = Math.hypot(a.x - b.x, a.y - b.y) || 1;
    const scale = Math.max(1, Math.min(SHOT_ZOOM_MAX, pinch.scale * (dist / pinch.dist)));
    const k = scale / pinch.scale;
    const c = stageCenter();
    // Точка, что была под серединой щипка, остаётся под ней и после масштаба.
    shotZoom.scale = scale;
    shotZoom.x = (a.x + b.x) / 2 - c.x - k * (pinch.mx - c.x - pinch.x);
    shotZoom.y = (a.y + b.y) / 2 - c.y - k * (pinch.my - c.y - pinch.y);
    clampShotZoom();
    applyShotZoom();
  };

  stage.addEventListener('pointerdown', (e) => {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size === 2) {
      startPinch();
      return;
    }
    if (pointers.size > 2) {
      // Третий палец ничего не меняет: щипок уже идёт.
      tracking = false;
      return;
    }
    startX = e.clientX;
    startY = e.clientY;
    baseX = shotZoom.x;
    baseY = shotZoom.y;
    dx = 0;
    tracking = true;
    horizontal = false;
    panning = false;
    swiped = false;
  });
  stage.addEventListener('pointermove', (e) => {
    const p = pointers.get(e.pointerId);
    if (!p) return;
    p.x = e.clientX;
    p.y = e.clientY;
    if (pinch) {
      if (pointers.size < 2) {
        pinch = null;
      } else {
        movePinch();
        if (e.cancelable) e.preventDefault();
        return;
      }
    }
    if (!tracking) return;
    const moveX = e.clientX - startX;
    const moveY = e.clientY - startY;
    // Приближённый снимок палец двигает, а не листает: иначе панорама
    // перескакивала бы на соседний экран.
    if (shotZoom.scale > 1) {
      if (!panning) {
        if (Math.abs(moveX) < 4 && Math.abs(moveY) < 4) return;
        panning = true;
        try {
          stage.setPointerCapture(e.pointerId);
        } catch {}
      }
      if (e.cancelable) e.preventDefault();
      shotZoom.x = baseX + moveX;
      shotZoom.y = baseY + moveY;
      clampShotZoom();
      applyShotZoom();
      return;
    }
    dx = moveX;
    if (!horizontal) {
      if (Math.abs(dx) < 8 && Math.abs(moveY) < 8) return;
      // Вертикальное движение оставляем браузеру: это не листание.
      if (Math.abs(dx) <= Math.abs(moveY)) {
        tracking = false;
        return;
      }
      horizontal = true;
      try {
        stage.setPointerCapture(e.pointerId);
      } catch {}
      $('shot-strip').classList.add('dragging');
    }
    if (e.cancelable) e.preventDefault();
    if (shot) $('shot-strip').style.transform = `translateX(calc(${-shot.index * 100}% + ${dx}px))`;
  });
  stage.addEventListener('pointerup', (e) => {
    if (!pointers.has(e.pointerId)) return;
    pointers.delete(e.pointerId);
    if (pinch) {
      pinch = null;
      markGesture();
      // Оставшийся на экране палец продолжает двигать приближённый снимок.
      const rest = [...pointers.values()][0];
      if (rest && shotZoom.scale > 1) {
        startX = rest.x;
        startY = rest.y;
        baseX = shotZoom.x;
        baseY = shotZoom.y;
        tracking = true;
        return;
      }
      tracking = false;
      renderShot();
      return;
    }
    if (!tracking) {
      renderShot();
      return;
    }
    tracking = false;
    if (horizontal && shot) {
      const count = Math.max(1, shot.total);
      if (dx <= -40 && shot.index < count - 1) shot.index += 1;
      else if (dx >= 40 && shot.index > 0) shot.index -= 1;
      // Клик, которым закончился свайп, не должен закрыть просмотр.
      markGesture();
    }
    horizontal = false;
    renderShot();
  });
  stage.addEventListener('pointercancel', (e) => {
    pointers.delete(e.pointerId);
    pinch = null;
    tracking = false;
    horizontal = false;
    panning = false;
    renderShot();
  });
  // Колесо с Ctrl — это щипок на тачпаде; приближённый снимок масштабируется
  // колесом и без него. В просмотре картинки колесо приближает всегда: под ним
  // нет ленты, которую надо было бы прокручивать.
  stage.addEventListener('wheel', (e) => {
    if (!shotImage()) return;
    if (!shot.image && !e.ctrlKey && shotZoom.scale <= 1) return;
    e.preventDefault();
    const c = stageCenter();
    const step = Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.002));
    zoomShotAt(e.clientX - c.x, e.clientY - c.y, shotZoom.scale * step);
  }, { passive: false });
  stage.addEventListener('click', (e) => {
    if (swiped || !shot) return;
    if (e.target.closest('img, button')) return;
    closeShots();
  });
}

$('shots').addEventListener('click', openShots);
$('shot-close').addEventListener('click', closeShots);
$('shot-refresh').addEventListener('click', () => {
  if (shot) startShots(shot.agent);
});
$('shot-prev').addEventListener('click', () => gotoShot((shot ? shot.index : 0) - 1));
$('shot-next').addEventListener('click', () => gotoShot((shot ? shot.index : 0) + 1));
// Esc в полноэкранном диалоге закрывает его сам; здесь важно только снять
// ожидание ответа и очистить недогруженные картинки.
$('shot').addEventListener('cancel', (e) => {
  e.preventDefault();
  closeShots();
});
$('shot').addEventListener('keydown', (e) => {
  if (!shot) return;
  if (e.key === 'ArrowLeft') {
    e.preventDefault();
    gotoShot(shot.index - 1);
  } else if (e.key === 'ArrowRight') {
    e.preventDefault();
    gotoShot(shot.index + 1);
  }
});
$('shot-agent-cancel').addEventListener('click', () => $('shot-agent').close());
$('shot-agent').addEventListener('click', (e) => {
  if (e.target === $('shot-agent')) $('shot-agent').close();
});
attachShotSwipe($('shot-stage'));
