'use strict';

// Запросы opencode: разрешения и формы с вариантами над полем ввода.
// Это полный список — новый запрос заменяет показанные карточки.

// ------------------------------------------------------------ запросы opencode

// Разрешения и формы opencode приходят отдельным сообщением и показываются над
// полем ввода. Это полный список: новый заменяет показанные карточки.

function clearPrompts() {
  promptSig = '';
  promptState.clear();
  promptControls.clear();
  promptCards.clear();
  $('prompts').replaceChildren();
}

function defaultsFor(prompt) {
  const out = {};
  for (const f of prompt.fields || []) {
    if (f.default !== undefined) out[f.key] = f.default;
  }
  return out;
}

function applyPrompts(list) {
  const sig = JSON.stringify(list);
  if (sig === promptSig) return;
  promptSig = sig;
  // Уже введённые значения переносим в новое дерево: пришёл ещё один запрос
  // или изменился список, но заполнять форму заново не нужно.
  const saved = new Map(promptState);
  for (const id of promptCards.keys()) saved.set(id, readControls(id));
  promptState.clear();
  promptControls.clear();
  promptCards.clear();
  const box = $('prompts');
  box.replaceChildren();
  const seen = new Set();
  for (const prompt of list) {
    if (!prompt || !prompt.id || seen.has(prompt.id)) continue;
    seen.add(prompt.id);
    promptState.set(prompt.id, saved.get(prompt.id) || defaultsFor(prompt));
    box.append(buildPrompt(prompt));
  }
}

function buildPrompt(prompt) {
  const card = el('div', `prompt ${prompt.kind}`);
  card.dataset.prompt = prompt.id;
  card.append(
    el('div', 'prompt-title', prompt.title || (prompt.kind === 'permission' ? 'opencode просит разрешение' : 'Вопрос opencode')),
  );
  if (prompt.text) card.append(el('div', 'prompt-text', prompt.text));
  if (prompt.kind === 'permission') buildPermission(card, prompt);
  else buildForm(card, prompt);
  card.append(el('div', 'prompt-error'));
  promptCards.set(prompt.id, card);
  setPromptBusy(prompt.id, promptSending.has(prompt.id));
  return card;
}

function buildPermission(card, prompt) {
  const actions = el('div', 'prompt-actions');
  const button = (label, cls, decision, title) => {
    const b = el('button', cls, label);
    b.type = 'button';
    if (title) b.title = title;
    b.addEventListener('click', () => sendPromptReply(prompt, { decision }));
    actions.append(b);
  };
  button('Разрешить', 'primary', 'once');
  if (prompt.save && prompt.save.length) {
    button('Разрешить всегда', '', 'always', `Сохранит правило: ${prompt.save.join(', ')}`);
  }
  button('Отклонить', 'danger', 'reject');
  card.append(actions);
  // Видно, что именно будет разрешено навсегда, — как в окне opencode.
  if (prompt.save && prompt.save.length) {
    card.append(el('div', 'prompt-save', `Всегда разрешит: ${prompt.save.join(', ')}`));
  }
}

function buildForm(card, prompt) {
  const values = promptState.get(prompt.id) || defaultsFor(prompt);
  const controls = new Map();
  promptControls.set(prompt.id, controls);
  const fields = el('div', 'prompt-fields');
  const changed = () => {
    promptState.set(prompt.id, readControls(prompt.id));
    updateConditions(prompt);
  };
  for (const f of prompt.fields || []) {
    const control = buildField(f, values[f.key], changed);
    controls.set(f.key, control);
    fields.append(control.node);
  }
  card.append(fields);
  // Enter в текстовом поле отправляет ответ, как в окне opencode.
  fields.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter' || e.isComposing || e.target.tagName !== 'INPUT') return;
    if (e.target.type === 'radio' || e.target.type === 'checkbox') return;
    e.preventDefault();
    submitPrompt(prompt);
  });
  const actions = el('div', 'prompt-actions');
  const ok = el('button', 'primary', 'Отправить');
  ok.type = 'button';
  ok.addEventListener('click', () => submitPrompt(prompt));
  const cancel = el('button', 'link', 'Отменить');
  cancel.type = 'button';
  cancel.addEventListener('click', () => sendPromptReply(prompt, { cancel: true }));
  actions.append(ok, cancel);
  card.append(actions);
  updateConditions(prompt);
}

// Обёртка поля: заголовок, пояснение и сам элемент управления.
function fieldWrap(f, title) {
  const wrap = el('div', 'prompt-field');
  const label = el('div', 'prompt-label');
  label.append(document.createTextNode(title || f.key));
  if (f.required) label.append(el('span', 'req', ' *'));
  wrap.append(label);
  if (f.description) wrap.append(el('div', 'prompt-desc', f.description));
  return wrap;
}

function buildField(f, initial, changed) {
  const wrap = fieldWrap(f, f.title);
  let read = () => undefined;
  let set = () => {};

  if (f.kind === 'external') {
    // Ссылку из формы открываем, только если это обычный http(s)-адрес.
    let href = '';
    try {
      const url = new URL(f.url, location.origin);
      if (url.protocol === 'http:' || url.protocol === 'https:') href = url.href;
    } catch {}
    if (href) {
      const link = el('a', 'prompt-external', 'Открыть');
      link.href = href;
      link.target = '_blank';
      link.rel = 'noopener noreferrer';
      wrap.append(link);
    } else if (f.url) {
      wrap.append(el('div', 'prompt-desc', f.url));
    }
  } else if (f.kind === 'string' && f.options && f.options.length) {
    // Выбор одного варианта; при custom можно ввести своё значение.
    const box = el('div', 'choices');
    const name = `pf${++promptUid}`;
    const radios = [];
    let customRadio = null;
    let customInput = null;
    let customRow = null;
    const addChoice = (value, label, description) => {
      const line = el('label', 'choice');
      const input = document.createElement('input');
      input.type = 'radio';
      input.name = name;
      input.value = value;
      const text = el('span', 'choice-text');
      text.append(el('span', 'choice-label', label || value));
      if (description) text.append(el('span', 'choice-desc', description));
      line.append(input, text);
      input.addEventListener('change', () => { syncCustom(); changed(); });
      box.append(line);
      const entry = { input, value };
      radios.push(entry);
      return entry;
    };
    for (const o of f.options) addChoice(o.value, o.label, o.description);
    if (f.custom) {
      customRadio = addChoice('__custom__', 'Другой вариант', '');
      const row = el('div', 'prompt-custom');
      customInput = document.createElement('input');
      customInput.type = 'text';
      customInput.placeholder = 'Своё значение';
      customInput.addEventListener('input', changed);
      row.append(customInput);
      box.append(row);
      customRow = row;
    }
    const syncCustom = () => {
      if (customRow) customRow.hidden = !(customRadio && customRadio.input.checked);
    };
    read = () => {
      const sel = radios.find((r) => r.input.checked);
      if (!sel) return undefined;
      if (sel.value === '__custom__') return customInput.value.trim();
      return sel.value;
    };
    set = (v) => {
      if (v === undefined || v === null) return;
      const text = String(v);
      const found = radios.find((r) => r.value === text);
      if (found) {
        found.input.checked = true;
      } else if (customRadio) {
        customRadio.input.checked = true;
        customInput.value = text;
      }
      syncCustom();
    };
    wrap.append(box);
    set(initial);
    syncCustom();
  } else if (f.kind === 'multiselect') {
    // Несколько вариантов; при custom можно добавить своё значение.
    const box = el('div', 'choices');
    const boxes = [];
    let customBox = null;
    let customInput = null;
    let customRow = null;
    for (const o of f.options || []) {
      const line = el('label', 'choice');
      const input = document.createElement('input');
      input.type = 'checkbox';
      input.value = o.value;
      const text = el('span', 'choice-text');
      text.append(el('span', 'choice-label', o.label || o.value));
      if (o.description) text.append(el('span', 'choice-desc', o.description));
      line.append(input, text);
      input.addEventListener('change', changed);
      box.append(line);
      boxes.push(input);
    }
    const syncCustom = () => {
      if (customRow) customRow.hidden = !(customBox && customBox.checked);
    };
    if (f.custom) {
      const line = el('label', 'choice');
      customBox = document.createElement('input');
      customBox.type = 'checkbox';
      const text = el('span', 'choice-text');
      text.append(el('span', 'choice-label', 'Другой вариант'));
      line.append(customBox, text);
      customBox.addEventListener('change', () => { syncCustom(); changed(); });
      box.append(line);
      const row = el('div', 'prompt-custom');
      customInput = document.createElement('input');
      customInput.type = 'text';
      customInput.placeholder = 'Своё значение';
      customInput.addEventListener('input', changed);
      row.append(customInput);
      box.append(row);
      customRow = row;
    }
    read = () => {
      const out = boxes.filter((b) => b.checked).map((b) => b.value);
      if (customBox && customBox.checked) {
        const text = customInput.value.trim();
        if (text) out.push(text);
      }
      return out;
    };
    set = (v) => {
      const items = Array.isArray(v) ? v : [];
      const known = new Set((f.options || []).map((o) => o.value));
      for (const b of boxes) b.checked = items.includes(b.value);
      const custom = items.filter((x) => !known.has(x));
      if (customBox && custom.length) {
        customBox.checked = true;
        customInput.value = custom.join(', ');
      }
      syncCustom();
    };
    wrap.append(box);
    set(initial);
    syncCustom();
  } else if (f.kind === 'boolean') {
    const select = document.createElement('select');
    for (const [value, label] of [['', 'Не выбрано'], ['true', 'Да'], ['false', 'Нет']]) {
      const opt = el('option', '', label);
      opt.value = value;
      select.append(opt);
    }
    if (initial !== undefined && initial !== null) select.value = String(initial);
    select.addEventListener('change', changed);
    read = () => (select.value === '' ? undefined : select.value === 'true');
    set = (v) => { select.value = v === undefined || v === null ? '' : String(v); };
    wrap.append(select);
  } else if (f.kind === 'number' || f.kind === 'integer') {
    const input = document.createElement('input');
    input.type = 'number';
    input.step = f.kind === 'integer' ? '1' : 'any';
    if (f.min !== undefined) input.min = String(f.min);
    if (f.max !== undefined) input.max = String(f.max);
    if (initial !== undefined && initial !== null) input.value = String(initial);
    input.addEventListener('input', changed);
    read = () => (input.value === '' ? undefined : Number(input.value));
    set = (v) => { input.value = v === undefined || v === null ? '' : String(v); };
    wrap.append(input);
  } else {
    // Строка без готовых вариантов: обычное поле ввода.
    const input = document.createElement('input');
    input.type =
      f.format === 'email' ? 'email'
      : f.format === 'uri' ? 'url'
      : f.format === 'date' ? 'date'
      : f.format === 'date-time' ? 'datetime-local'
      : 'text';
    if (f.placeholder) input.placeholder = f.placeholder;
    if (f.min_length !== undefined) input.minLength = f.min_length;
    if (f.max_length !== undefined) input.maxLength = f.max_length;
    if (f.pattern) input.pattern = f.pattern;
    if (initial !== undefined && initial !== null) input.value = String(initial);
    input.addEventListener('input', changed);
    read = () => input.value;
    set = (v) => { input.value = v === undefined || v === null ? '' : String(v); };
    wrap.append(input);
  }

  return { node: wrap, read, set };
}

function readControls(id) {
  const controls = promptControls.get(id);
  const out = {};
  if (!controls) return out;
  for (const [key, control] of controls) {
    const value = control.read();
    if (value !== undefined) out[key] = value;
  }
  return out;
}

// Условия показа поля, как в окне opencode: поле с невыполненным условием
// не показывается и в ответ не попадает (даже его значение по умолчанию).
function conditionsMet(f, values) {
  if (f.kind === 'external' || !f.when || !f.when.length) return true;
  return f.when.every((when) => {
    const value = values[when.key];
    if (value === undefined) return false;
    const equal = Array.isArray(value) ? value.includes(String(when.value)) : value === when.value;
    return when.op === 'neq' ? !equal : equal;
  });
}

function fieldVisible(f, values) {
  if (f.kind === 'external') return true;
  return !f.hidden && conditionsMet(f, values);
}

function updateConditions(prompt) {
  const values = readControls(prompt.id);
  promptState.set(prompt.id, values);
  const controls = promptControls.get(prompt.id);
  if (!controls) return;
  for (const f of prompt.fields || []) {
    const control = controls.get(f.key);
    if (control) control.node.hidden = !fieldVisible(f, values);
  }
}

function collectAnswer(prompt) {
  const values = readControls(prompt.id);
  const answer = {};
  for (const f of prompt.fields || []) {
    if (f.kind === 'external') continue;
    if (!conditionsMet(f, values)) continue;
    // Скрытое поле передаёт значение по умолчанию, как в окне opencode.
    if (f.hidden) {
      if (f.default !== undefined) answer[f.key] = f.default;
      continue;
    }
    const value = values[f.key];
    if (f.kind === 'multiselect') {
      if (Array.isArray(value) && value.length) answer[f.key] = value;
    } else if (f.kind === 'boolean') {
      if (typeof value === 'boolean') answer[f.key] = value;
    } else if (f.kind === 'number' || f.kind === 'integer') {
      if (typeof value === 'number' && Number.isFinite(value)) answer[f.key] = value;
    } else if (typeof value === 'string' && value !== '') {
      answer[f.key] = value;
    }
  }
  return answer;
}

function validateAnswer(prompt) {
  const values = readControls(prompt.id);
  for (const f of prompt.fields || []) {
    if (f.kind === 'external' || !fieldVisible(f, values)) continue;
    const value = values[f.key];
    const label = f.title || f.key;
    const empty = value === undefined || value === '' || (Array.isArray(value) && !value.length);
    if (f.required && empty) {
      return `${label}: ${f.kind === 'multiselect' ? 'выберите хотя бы один вариант' : 'укажите значение'}`;
    }
    if (empty) continue;
    if (f.kind === 'multiselect') {
      if (f.min_items !== undefined && value.length < f.min_items) return `${label}: выберите не меньше ${f.min_items}`;
      if (f.max_items !== undefined && value.length > f.max_items) return `${label}: выберите не больше ${f.max_items}`;
    } else if (f.kind === 'number' || f.kind === 'integer') {
      if (!Number.isFinite(value)) return `${label}: нужно число`;
      if (f.kind === 'integer' && !Number.isInteger(value)) return `${label}: нужно целое число`;
      if (f.min !== undefined && value < f.min) return `${label}: не меньше ${f.min}`;
      if (f.max !== undefined && value > f.max) return `${label}: не больше ${f.max}`;
    } else if (typeof value === 'string') {
      if (f.min_length !== undefined && value.length < f.min_length) return `${label}: не короче ${f.min_length} символов`;
      if (f.max_length !== undefined && value.length > f.max_length) return `${label}: не длиннее ${f.max_length} символов`;
      if (f.pattern) {
        try {
          if (!new RegExp(f.pattern).test(value)) return `${label}: не соответствует формату`;
        } catch {
          // Некорректное выражение проверит сам opencode.
        }
      }
      if (f.format === 'email' && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value)) return `${label}: нужен адрес почты`;
      if (f.format === 'uri') {
        try {
          new URL(value);
        } catch {
          return `${label}: нужна ссылка`;
        }
      }
      if (f.format === 'date' && !/^\d{4}-\d{2}-\d{2}$/.test(value)) return `${label}: нужна дата в виде ГГГГ-ММ-ДД`;
      if (f.format === 'date-time' && Number.isNaN(new Date(value).getTime())) {
        return `${label}: нужны дата и время`;
      }
    }
  }
  return '';
}

function submitPrompt(prompt) {
  const card = promptCards.get(prompt.id);
  const error = validateAnswer(prompt);
  if (error) {
    if (card) card.querySelector('.prompt-error').textContent = error;
    return;
  }
  if (card) card.querySelector('.prompt-error').textContent = '';
  sendPromptReply(prompt, { answer: collectAnswer(prompt) });
}

function sendPromptReply(prompt, payload) {
  const cur = current();
  if (!cur || promptSending.has(prompt.id)) return;
  const id = String(++cmdSeq);
  const msg = {
    t: 'prompt_reply',
    id,
    agent: cur.agent.name,
    session: cur.session.key,
    prompt: prompt.id,
    kind: prompt.kind,
    ...payload,
  };
  if (!send(msg)) {
    setHint('Нет соединения с сервером.', true);
    return;
  }
  promptSending.add(prompt.id);
  promptReplyIds.set(id, prompt.id);
  setPromptBusy(prompt.id, true);
  setHint(payload.cancel ? 'Отменяем запрос…' : 'Отправляем ответ…');
}

function setPromptBusy(id, busy) {
  const card = promptCards.get(id);
  if (!card) return;
  card.classList.toggle('sending', busy);
  for (const node of card.querySelectorAll('button, input, select, textarea')) node.disabled = busy;
}

function onPromptResult(msg) {
  const promptId = promptReplyIds.get(msg.id);
  promptReplyIds.delete(msg.id);
  if (msg.ok) {
    // Карточка исчезнет со следующим обновлением списка. До этого не даём
    // отправить ответ второй раз; таймер — на случай, если обновление не придёт.
    if (promptId) {
      setTimeout(() => {
        promptSending.delete(promptId);
        setPromptBusy(promptId, false);
      }, 8000);
    }
    const cur = current();
    setHint(cur && cur.session.tool === 'claude' ? 'Ответ введён в окно Claude Code.' : 'Ответ отправлен в opencode.');
    return;
  }
  if (promptId) {
    promptSending.delete(promptId);
    setPromptBusy(promptId, false);
  }
  const error = `Ошибка: ${msg.error || 'неизвестная'}`;
  setHint(error, true);
  const card = promptId && promptCards.get(promptId);
  if (card) card.querySelector('.prompt-error').textContent = msg.error || 'Не удалось отправить ответ.';
}
