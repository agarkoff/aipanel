// Плагин AiPanel для TUI opencode.
//
// Пока окно opencode открыто, раз в несколько секунд пишет файл
// ~/.aipanel/opencode/<pid>.json с id сессии, которая сейчас показана в окне.
// Агент AiPanel читает эти файлы и по ним знает, какие сессии открыты в окнах.
// Файл удаляется при закрытии окна; если окно убили, агент увидит, что процесса нет.
//
// Установка: aipanel-agent install-opencode-plugin (см. README).

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const DIR = path.join(process.env.AIPANEL_HOME || path.join(os.homedir(), ".aipanel"), "opencode");
const FILE = path.join(DIR, `${process.pid}.json`);
const POLL_MS = 500;
const HEARTBEAT_MS = 5000;

function start(currentSession) {
  let last;
  let lastWrite = 0;
  let stopped = false;

  function write(sessionID) {
    try {
      fs.mkdirSync(DIR, { recursive: true });
      const tmp = `${FILE}.tmp`;
      fs.writeFileSync(
        tmp,
        JSON.stringify({ pid: process.pid, sessionID: sessionID ?? null, cwd: process.cwd(), updated: Date.now() }),
      );
      fs.renameSync(tmp, FILE);
    } catch {
      // Отчёт агенту не должен мешать работе opencode.
    }
  }

  function tick() {
    if (stopped) return;
    let id;
    try {
      id = currentSession();
    } catch {
      id = undefined;
    }
    const now = Date.now();
    if (id !== last || now - lastWrite >= HEARTBEAT_MS) {
      last = id;
      lastWrite = now;
      write(id);
    }
  }

  tick();
  const timer = setInterval(tick, POLL_MS);
  timer.unref?.();
  return () => {
    stopped = true;
    clearInterval(timer);
    try {
      fs.unlinkSync(FILE);
    } catch {
      // файла может не быть
    }
  };
}

export default {
  id: "aipanel.tui-session",
  // opencode V2: setup возвращает функцию остановки.
  setup(api) {
    return start(() => {
      const route = api.ui.router.current();
      return route.type === "session" ? route.sessionID : undefined;
    });
  },
  // opencode V1.
  tui(api) {
    const stop = start(() => {
      const route = api.route.current;
      return route?.name === "session" ? route.params?.sessionID : undefined;
    });
    api.lifecycle.onDispose(stop);
  },
};
