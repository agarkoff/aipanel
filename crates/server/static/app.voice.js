'use strict';

// Голосовой ввод: браузер пишет звук, распознаёт его агент на машине сессии.
// Поэтому диктовка одинаково работает и с компьютера, и с телефона: телефону
// нужен только микрофон, панель и открытое соединение с сервером.
//
// Кнопка у поля одна, как в Telegram: пустое поле — микрофон, набранный текст —
// отправка, идёт запись — остановка. Отправку по Enter с пустым полем микрофон
// не запускает: в Telegram нажатие Enter на пустом поле тоже ничего не делает.

// Предел записи — как у сервера и агента (`MAX_AUDIO_BYTES` в протоколе).
// В Opus это минуты три речи; дольше диктовка не бывает.
const VOICE_MAX_BYTES = 600000;
// Дольше этого не пишем: длинная запись — это уже не запрос в сессию.
const VOICE_MAX_MS = 120000;
// Уровень ниже которого микрофон считается молчащим: −36 dBFS от полной
// шкалы, речь даже вполголоса даёт заметно больше. Телефон иногда отдаёт
// поток, который замолкает через полсекунды (микрофон заняло другое
// приложение, наушники без микрофона, политика прошивки): в файл пишется
// тишина, а модель на тишине выдаёт правдоподобные, но выдуманные слова.
// Такую запись агенту не отправляем — иначе человек получает «два неверных
// слова» вместо честного «микрофон молчит».
const VOICE_MIN_PEAK = 0.015;
// Столько речи в записи должно быть, чтобы её стоило распознавать.
const VOICE_MIN_SPEECH_S = 0.15;
// Какая доля записи должна быть речью. Телефон иногда отдаёт микрофон, который
// замолкает через полсекунды: тогда в записи три секунды тишины и двести
// миллисекунд голоса, а модель на такой обрезок выдаёт одно-два выдуманных
// слова. Живая диктовка с паузами даёт заметно больше, поэтому порог низкий:
// он отделяет «звука почти нет» от «человек сказал одно слово».
const VOICE_MIN_SPEECH_RATIO = 0.08;
// Как часто читаем уровень и обновляем подсказку под полем.
const VOICE_TICK_MS = 200;

// Сколько ждём ответа агента. Первый раз он скачивает модель, поэтому запас
// большой; подсказки по ходу — ниже.
const VOICE_WAIT_MS = 15 * 60 * 1000;
const VOICE_SLOW_MS = 8000;
const VOICE_DOWNLOAD_MS = 25000;
// Агент старой версии команду не знает и молчит вовсе: об этом лучше сказать
// раньше, чем через 15 минут ожидания.
const VOICE_STALE_MS = 3 * 60 * 1000;

let voiceRec = null; // { recorder, stream, id, agent, session, started, timer }
let voiceWait = null; // { id, agent, session, timers }
// Нажатие на кнопку (а не Enter в поле): только оно включает микрофон.
let voiceTap = false;
// Формат записи выбирается один раз: браузеры поддерживают разные.
let voiceMime = '';

function voiceSupport() {
  return Boolean(
    window.isSecureContext &&
      navigator.mediaDevices &&
      navigator.mediaDevices.getUserMedia &&
      window.MediaRecorder,
  );
}

// Первый поддерживаемый формат: Opus в WebM/Ogg (Chrome, Firefox, Android)
// или AAC в MP4 (Safari на iPhone).
function pickVoiceMime() {
  if (voiceMime) return voiceMime;
  const list = [
    'audio/webm;codecs=opus',
    'audio/ogg;codecs=opus',
    'audio/webm',
    'audio/mp4',
    'audio/ogg',
  ];
  voiceMime = list.find((m) => MediaRecorder.isTypeSupported(m)) || '';
  return voiceMime;
}

async function startVoice() {
  if (voiceRec) return;
  if (!selected) {
    setHint('Голосовой ввод: сначала откройте сессию.', true);
    return;
  }
  const agent = agents.find((a) => a.name === selected.agent);
  if (!agent || !agent.online) {
    setHint('Голосовой ввод: агент не в сети.', true);
    return;
  }
  let stream;
  try {
    // Моно и обработка звука — то, что нужно распознаванию: стереопоток
    // телефона модель всё равно сводит в моно, а эхоподавление убирает гул
    // динамика. `ideal` — чтобы устройство без поддержки не отказало в
    // микрофоне целиком.
    stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: { ideal: 1 },
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
  } catch (e) {
    const denied = e && (e.name === 'NotAllowedError' || e.name === 'SecurityError');
    setHint(
      denied
        ? 'Голосовой ввод: браузер не дал доступ к микрофону.'
        : `Голосовой ввод: микрофон недоступен (${(e && e.name) || 'ошибка'}).`,
      true,
    );
    return;
  }
  const mime = pickVoiceMime();
  const rec = new MediaRecorder(stream, mime ? { mimeType: mime } : undefined);
  const chunks = [];
  const st = {
    recorder: rec,
    stream,
    agent: selected.agent,
    session: selected.session,
    started: Date.now(),
    timer: null,
    // Уровень записи: см. `voiceMeter`. Пик нужен только для подсказки по
    // ходу записи, точный вывод делается по самому файлу (см. `voiceLoudness`).
    meter: voiceMeter(stream),
    peak: 0,
    shown: -1,
  };
  voiceRec = st;
  rec.addEventListener('dataavailable', (e) => {
    if (e.data && e.data.size) chunks.push(e.data);
  });
  rec.addEventListener('stop', () => voiceStopped(st, chunks));
  // Микрофон может замолчать уже во время записи (его заняло другое
  // приложение или звонок): тогда в файл идёт тишина, и об этом лучше сказать
  // сразу, чем после отправки.
  for (const track of stream.getAudioTracks()) {
    track.addEventListener('mute', () => {
      if (voiceRec === st) {
        setHint('Голосовой ввод: микрофон замолчал — его заняло другое приложение.', true);
      }
    });
  }
  // Отсчёт в подсказке: по нему видно, что запись идёт, и сколько уже сказано.
  st.timer = setInterval(() => voiceTick(st), VOICE_TICK_MS);
  st.limit = setTimeout(stopVoice, VOICE_MAX_MS);
  rec.start();
  renderVoice();
  setHint('Говорю… 0:00. Микрофон — закончить.');
}

// Подсказка по ходу записи: пик уровня за прошедшее время и секунды. Наверх
// выводим раз в секунду, чтобы не дёргать экран телефона.
function voiceTick(st) {
  if (st.meter && st.meter.ready()) {
    const peak = st.meter.peak();
    if (peak > st.peak) st.peak = peak;
  }
  const secs = Math.floor((Date.now() - st.started) / 1000);
  if (secs === st.shown) return;
  st.shown = secs;
  const time = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
  // Молчащий микрофон видно уже через пару секунд: пусть человек не
  // договаривает фразу в пустоту. Это только подсказка — окончательное
  // решение принимается по самому файлу.
  if (secs >= 2 && st.meter && st.meter.ready() && st.peak < VOICE_MIN_PEAK) {
    setHint(
      `Говорю… ${time}. Микрофон молчит: звука не слышно. Проверьте, не занят ли он другим приложением.`,
      true,
    );
    return;
  }
  setHint(`Говорю… ${time}. Микрофон — закончить.`);
}

// Точная проверка записи: декодируем сам файл и смотрим, есть ли в нём звук.
// Анализатор во время записи видит только отдельные окна — по нему легко
// принять паузу между словами за тишину целиком, а по готовой записи видно
// всё. `null` — проверить не удалось (браузер не декодирует свой же файл),
// тогда отправляем как раньше.
async function voiceLoudness(blob) {
  const Ctx = window.AudioContext || window.webkitAudioContext;
  if (!Ctx) return null;
  let ctx;
  try {
    ctx = new Ctx();
  } catch {
    return null;
  }
  try {
    const data = await ctx.decodeAudioData(await blob.arrayBuffer());
    const samples = data.getChannelData(0);
    let peak = 0;
    let loud = 0;
    for (const s of samples) {
      const level = Math.abs(s);
      if (level > peak) peak = level;
      if (level >= VOICE_MIN_PEAK) loud += 1;
    }
    return { peak, secs: loud / data.sampleRate, total: samples.length / data.sampleRate };
  } catch {
    return null;
  } finally {
    if (ctx.state !== 'closed') ctx.close();
  }
}

// Уровень записи: анализатор на том же потоке, который пишется в файл. По нему
// видно, слышит ли микрофон хоть что-то, и можно не отправлять агенту тишину.
// Если браузер не даёт AudioContext, метра нет — тогда всё как раньше.
function voiceMeter(stream) {
  const Ctx = window.AudioContext || window.webkitAudioContext;
  if (!Ctx) return null;
  try {
    const ctx = new Ctx();
    if (ctx.state === 'suspended') ctx.resume();
    const src = ctx.createMediaStreamSource(stream);
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 1024;
    // Выход заглушен: микрофон не должен звучать в динамике телефона, но
    // цепочка должна доходить до выхода — иначе браузер её не считает.
    const mute = ctx.createGain();
    mute.gain.value = 0;
    src.connect(analyser);
    analyser.connect(mute);
    mute.connect(ctx.destination);
    const buf = new Uint8Array(analyser.fftSize);
    return {
      ready: () => ctx.state === 'running',
      peak() {
        analyser.getByteTimeDomainData(buf);
        let max = 0;
        for (const v of buf) {
          const level = Math.abs(v - 128);
          if (level > max) max = level;
        }
        return max / 128;
      },
      close() {
        if (ctx.state !== 'closed') ctx.close();
      },
    };
  } catch {
    return null;
  }
}

function stopVoice() {
  const st = voiceRec;
  if (!st) return;
  clearInterval(st.timer);
  clearTimeout(st.limit);
  // Кнопку гасим сразу: повторного нажатия во время остановки быть не должно.
  renderVoice(true);
  if (st.recorder.state !== 'inactive') st.recorder.stop();
}

// Запись закончилась: отправляем её агенту. Дальше в поле ввода ждёт текст.
async function voiceStopped(st, chunks) {
  for (const track of st.stream.getTracks()) track.stop();
  if (voiceRec === st) voiceRec = null;
  renderVoice();
  if (st.meter) st.meter.close();

  const blob = new Blob(chunks, { type: voiceMime || 'audio/webm' });
  if (!blob.size) {
    setHint('Голосовой ввод: пустая запись, микрофон ничего не услышал.', true);
    return;
  }
  if (blob.size > VOICE_MAX_BYTES) {
    setHint('Голосовой ввод: запись слишком длинная — говорите короче.', true);
    return;
  }
  // Микрофон мог молчать всю запись: агенту отправлять нечего. Модель на
  // тишине выдаёт выдуманные слова, и они выглядели бы как неверное
  // распознавание — «два неверных слова» вместо честной подсказки.
  const loudness = await voiceLoudness(blob);
  const dead =
    loudness &&
    (loudness.peak < VOICE_MIN_PEAK ||
      (loudness.total >= 1 && loudness.secs < loudness.total * VOICE_MIN_SPEECH_RATIO));
  if (dead) {
    setHint(
      'Голосовой ввод: телефон записал тишину — микрофон молчит. Проверьте, не занят ли он другим приложением или наушниками.',
      true,
    );
    return;
  }
  if (loudness && loudness.secs < VOICE_MIN_SPEECH_S) {
    setHint('Голосовой ввод: в записи почти нет речи — говорите ближе к микрофону.', true);
    return;
  }
  const audio = await blobToBase64(blob);
  if (!audio) {
    setHint('Голосовой ввод: не удалось прочитать запись.', true);
    return;
  }
  startVoiceWait(st.agent, st.session);
  if (!send({ t: 'transcribe', id: voiceWait.id, agent: st.agent, audio })) {
    endVoiceWait('Голосовой ввод: нет соединения с сервером.', true);
  }
}

function blobToBase64(blob) {
  return new Promise((done) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result || '');
      done(url.slice(url.indexOf(',') + 1));
    };
    reader.onerror = () => done('');
    reader.readAsDataURL(blob);
  });
}

function startVoiceWait(agent, session) {
  endVoiceWait();
  voiceWait = { id: String(++cmdSeq), agent, session, timers: [] };
  setHint('Распознаю…');
  voiceWait.timers.push(
    setTimeout(() => {
      if (voiceWait) setHint('Всё ещё распознаю: агент считает запись на своём процессоре.');
    }, VOICE_SLOW_MS),
  );
  voiceWait.timers.push(
    setTimeout(() => {
      if (voiceWait) setHint('Агент готовит первую запись: скачивает модель. Это один раз.');
    }, VOICE_DOWNLOAD_MS),
  );
  voiceWait.timers.push(
    setTimeout(() => {
      if (voiceWait) {
        setHint('Агент молчит: возможно, он версии без голосового ввода — проверьте, что он обновлён.');
      }
    }, VOICE_STALE_MS),
  );
  voiceWait.timers.push(
    setTimeout(() => {
      endVoiceWait('Голосовой ввод: агент не ответил за 15 минут.', true);
    }, VOICE_WAIT_MS),
  );
}

function endVoiceWait(error, isError) {
  if (voiceWait) {
    for (const t of voiceWait.timers) clearTimeout(t);
    voiceWait = null;
  }
  if (error) setHint(error, Boolean(isError));
}

// Ответ агента: текст в поле ввода. Ошибку показывает подсказка под полем.
function onTranscript(msg) {
  if (!voiceWait || msg.id !== voiceWait.id) return;
  const { agent, session } = voiceWait;
  endVoiceWait();
  if (msg.error) {
    setHint(`Голосовой ввод: ${msg.error}`, true);
    return;
  }
  const text = String(msg.text || '').trim();
  if (!text) {
    setHint('Голосовой ввод: в записи не разобрать слов.', true);
    return;
  }
  // Пока шло распознавание, человек мог уйти в другую сессию: текст тогда
  // вставлять некуда, и он не должен пропасть молча.
  if (!selected || selected.agent !== agent || selected.session !== session) {
    setHint('Голосовой ввод: распознано, но открыта другая сессия — текст не вставлен.', true);
    return;
  }
  insertVoiceText(text);
  setHint('');
}

// Текст встаёт туда, где стояла каретка. Пробелы по краям — чтобы слова не
// склеивались, если каретка была посреди фразы.
function insertVoiceText(text) {
  const area = $('text');
  const start = area.selectionStart ?? area.value.length;
  const end = area.selectionEnd ?? area.value.length;
  const before = area.value.slice(0, start);
  const after = area.value.slice(end);
  const lead = before && !/\s$/.test(before) ? ' ' : '';
  const tail = after && !/^\s/.test(after) ? ' ' : '';
  const inserted = lead + text + tail;
  area.value = before + inserted + after;
  const caret = start + inserted.length;
  area.setSelectionRange(caret, caret);
  resetHistoryNav();
  autoGrow();
  // Подсказка команд открыта по `/` в начале поля: вставленный текст её меняет.
  if (slash.isOpen()) slash.refresh();
  updateSendButton();
  // Распознанный текст — такой же черновик, как набранный руками: он должен
  // дойти до других устройств и пережить перезапуск панели.
  saveDraft();
  if (!touchInput) area.focus();
}

// Кнопка показывает то, что сейчас уместно: микрофон — когда поле пустое,
// отправку — когда есть текст, остановку — пока идёт запись.
function updateSendButton() {
  const btn = $('send');
  if (!btn) return;
  const recording = Boolean(voiceRec);
  const empty = !$('text').value.trim();
  btn.classList.toggle('recording', recording);
  btn.classList.toggle('voice', !recording && empty);
  const [label, title] = recording
    ? ['Закончить', 'Закончить запись и распознать']
    : empty
      ? ['Голосом', 'Голосовой ввод']
      : ['Отправить', 'Отправить (Enter)'];
  btn.setAttribute('aria-label', title);
  btn.title = title;
  const span = btn.querySelector('.label');
  if (span) span.textContent = label;
}

function renderVoice(busy) {
  const btn = $('send');
  btn.disabled = Boolean(busy);
  updateSendButton();
}

function initVoice() {
  const btn = $('send');
  const supported = voiceSupport();
  // Без микрофона кнопка остаётся кнопкой отправки: прятать её нельзя, иначе
  // отправить сообщение с телефона будет нечем.
  if (!supported) {
    updateSendButton();
    return;
  }
  btn.addEventListener('click', () => {
    // Отметка нужна до submit: форма отправляется сразу после клика.
    voiceTap = true;
  });
  $('text').addEventListener('input', updateSendButton);
  // Запись прерывается, когда панель уходит в фон: на телефоне браузер всё
  // равно гасит микрофон, и лучше сказать об этом, чем потерять запись молча.
  document.addEventListener('visibilitychange', () => {
    if (document.hidden && voiceRec) {
      stopVoice();
      setHint('Голосовой ввод: запись остановлена — панель свернули.');
    }
  });
  window.addEventListener('pagehide', () => {
    if (voiceRec) stopVoice();
  });
  updateSendButton();
}

initVoice();
