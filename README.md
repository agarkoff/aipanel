# AiPanel

Веб-панель для локальных сессий Claude Code и opencode: в браузере видно состояние каждой сессии и её ленту, из браузера можно отправить в сессию новое сообщение или прервать работу.

## Как это устроено

```
браузер ──HTTPS/WSS──> aipanel-server (Docker, хостинг) <──WSS── aipanel-agent (ваша машина)
                                                                    ├─ Claude Code: ~/.claude
                                                                    └─ opencode: локальный HTTP API
```

- **`crates/server`** — сервер панели на Rust (axum). Отдаёт интерфейс (три статических файла встроены в бинарник) и пересылает сообщения между браузером и агентами. Ленты сессий не хранит, в памяти только списки сессий.
- **`crates/agent`** — локальный агент. Сам подключается к серверу исходящим WebSocket, поэтому входящие порты на домашней машине не нужны.
- **`crates/proto`** — общие типы протокола.

Фронтенд написан на обычном JavaScript без сборки: на память хостинга язык фронтенда не влияет, а сборка WASM усложнила бы образ.

Замеры на тестовом запуске: контейнер сервера занимает около 0,7 МБ памяти, образ — 1,7 МБ; агент — около 6 МБ памяти, exe — 1,1 МБ.

## Что агент умеет

| Сессия | Состояние и лента | Отправка сообщения | Прерывание |
|---|---|---|---|
| Claude Code, открыта в терминале | реестр `~/.claude/sessions`, транскрипт `.jsonl` | текст набирается в консоль сессии и отправляется Enter (только Windows) | Esc в консоль |
| Claude Code, закрыта | транскрипт `.jsonl` | на компьютере открывается окно Windows Terminal с `claude --resume <id> -- <текст>`, сообщение уходит первым запросом | — |
| opencode, не активна | API фонового сервиса | на компьютере открывается окно Windows Terminal с `opencode -s <id> --prompt=<текст>` | — |
| opencode, работает или открыта в окне | API фонового сервиса | `POST /api/session/{id}/prompt` | `POST /api/session/{id}/interrupt` |

В панели Enter отправляет сообщение, Shift+Enter переносит строку. На телефоне Enter переносит строку, а отправляет кнопка.

Ограничения:

- Ввод в открытую сессию Claude Code идёт как нажатия клавиш. Если в терминале в этот момент открыт диалог (запрос разрешения, меню), текст попадёт в него. Если вы сами что-то набираете в этой сессии, тексты смешаются.
- Запросы разрешений из панели не видны и не подтверждаются. У opencode сессия с таким запросом показывается со статусом «ждёт разрешения».
- Программа в окне запускается через обёртку `aipanel-agent run-window`: окно закрывается само после выхода из Claude Code или opencode, даже если они вернули ненулевой код. Если программа упала в первые 15 секунд, окно остаётся, чтобы была видна ошибка.
- Окно получает оформление (цветовая схема, шрифт) профиля Windows Terminal по умолчанию, поэтому выглядит как ваши обычные окна. Другой профиль задаётся параметром `terminal_profile` в `agent.toml`. Без указания профиля `wt` берёт общие настройки и окно получается чёрным.
- Окно запускается через `wt.exe`; если Windows Terminal не установлен, откроется обычное окно консоли. Текст сообщения для запуска окна не должен быть длиннее 12 000 символов. Дополнительные аргументы `claude --resume` задаются в `resume_args` (см. `agent.example.toml`).
- Перед отправкой в сессию opencode, которая ничего не выполняет, агент переключает её на последнюю модель, выбранную в окне opencode (файл `~/.local/state/opencode/model.json`), иначе сообщение ушло бы на модель, сохранённую в сессии, или на умолчание сервера. Поведение задаёт параметр `model` в `[opencode]`: `auto`, `session` или явное `провайдер/модель`.
- Без плагина (см. ниже) агент не знает, открыта ли сессия opencode в окне: все сессии показываются как «не активна», а окно, в котором сессию выбрали уже внутри программы, не будет найдено и откроется ещё одно.
- На Linux и macOS агент показывает сессии, закрытые сессии Claude Code продолжаются фоновым `claude -p --resume`, а opencode получает сообщения через API; окна и ввод в открытую сессию Claude Code не реализованы.

## Сервер

```sh
cp .env.example .env      # задать PANEL_PASSWORD и AGENT_TOKEN
docker compose up -d --build
```

По умолчанию порт открыт только на `127.0.0.1:8080` хоста — в расчёте на HTTPS-прокси на том же хосте. Прокси должен пропускать WebSocket и передавать заголовки `Host` и `X-Forwarded-Proto`.

Если прокси нет, в compose есть Caddy с автоматическим сертификатом:

```sh
# в .env: DOMAIN=panel.example.com
docker compose --profile tls up -d --build
```

Без HTTPS панель в интернет выставлять нельзя: пароль и токен агента пойдут открытым текстом, а через панель выполняются команды на вашей машине.

## Агент

```sh
cargo build --release -p aipanel-agent
```

Конфигурация `agent.toml` (образец — `agent.example.toml`) ищется по порядку: путь из `--config`, рядом с exe, в текущей папке, в `~/.aipanel/agent.toml`. Запускайте агент от того же пользователя, под которым работают Claude Code и opencode. На Linux для сборки нужен пакет разработки OpenSSL (`libssl-dev`).

Чтобы агент был доступен как команда, скопируйте exe в каталог из `PATH`, а конфигурацию положите в `~/.aipanel/agent.toml` (Windows):

```powershell
Disable-ScheduledTask -TaskName AiPanelAgent   # если настроен автозапуск (см. ниже): иначе он поднимет агента во время копирования
aipanel-agent stop                             # запущенный exe нельзя заменить
Copy-Item target\release\aipanel-agent.exe $env:USERPROFILE\.local\bin\
Enable-ScheduledTask -TaskName AiPanelAgent
aipanel-agent status
```

После каждой пересборки копирование нужно повторить.

### Управление агентом

```sh
aipanel-agent status             # показать состояние; если агент не запущен — запустить его
aipanel-agent status --no-start  # только показать состояние
aipanel-agent stop               # остановить агента
```

`status` печатает pid, время работы, адрес сервера, состояние соединения и число сессий. Если агент не запущен, команда запускает его в фоне (без окна, переживает закрытие консоли), ждёт подключения и показывает результат; при ошибке (например, неверный `agent.toml`) печатает конец лога `~/.aipanel/agent.log`. Код выхода: 0 — работает и подключён, 2 — работает, но не подключён, 1 — не запущен или не запустился. Второй агент на той же машине не стартует: он вытеснял бы первого на сервере.

Состояние агент пишет в `~/.aipanel/agent.json` раз в 2 секунды (файл каждый раз перезаписывается целиком, его размер не растёт).

Лог агента — `~/.aipanel/agent.log`. Он ограничен: при превышении 512 КБ старое содержимое удаляется и лог начинается заново, старые логи не хранятся. Одна и та же ошибка подключения к серверу пишется один раз, а не при каждой попытке. Время в логе — UTC.

#### Автозапуск (Windows)

Задача планировщика `AiPanelAgent` запускает `aipanel-agent status` при входе в систему и каждые 5 минут. `status` стартует агента, только если он не запущен, поэтому задача заодно поднимает агента после падения. Окно не показывается (`conhost --headless`), ограничения по времени выполнения нет. Создание:

```powershell
$user = "$env:USERDOMAIN\$env:USERNAME"
$action = New-ScheduledTaskAction -Execute 'conhost.exe' -Argument "--headless `"$env:USERPROFILE\.local\bin\aipanel-agent.exe`" status"
$logon = New-ScheduledTaskTrigger -AtLogOn -User $user
$repeat = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 5) -RepetitionDuration (New-TimeSpan -Days 3650)
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero)
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName AiPanelAgent -Action $action -Trigger @($logon, $repeat) -Settings $settings -Principal $principal
```

Удаление: `Unregister-ScheduledTask -TaskName AiPanelAgent -Confirm:$false`. Остановить агента насовсем можно только вместе с задачей: иначе она запустит его снова не позже чем через 5 минут.

Для opencode должен работать его фоновый сервис (`opencode service status`).

### Плагин для окон opencode

Плагин сообщает агенту, какие сессии открыты в окнах opencode. С ним у открытой сессии зелёный кружок и статус «ждёт ввода», а повторное сообщение не открывает второе окно, даже если сессию выбрали уже внутри программы. Без него всё работает, но без этих признаков.

Установка (на той машине, где работает opencode; нужен собранный агент):

```sh
target/release/aipanel-agent install-opencode-plugin
```

Команда копирует плагин `plugins/opencode/aipanel-tui-session.js` в `~/.config/opencode/plugins/aipanel-tui/tui.js`; opencode подхватывает его сам, конфигурацию править не нужно. Открытые окна opencode нужно перезапустить. Удаление:

```sh
target/release/aipanel-agent uninstall-opencode-plugin
```

Пока окно открыто, плагин раз в несколько секунд пишет `~/.aipanel/opencode/<pid>.json` (id сессии и время). Агент читает эти файлы, а отчёты завершившихся процессов удаляет. Каталог можно сменить переменной `AIPANEL_HOME`, каталог конфигурации opencode — `OPENCODE_CONFIG_DIR`.

## Безопасность

- Вход в панель по паролю, cookie `HttpOnly; SameSite=Strict`, ограничение числа неудачных попыток.
- Агент подключается с отдельным токеном. Сервер не запустится, если пароль или токен короче 8 символов.
- Тот, кто знает пароль панели, может отправлять команды в ваши сессии, то есть выполнять код на вашей машине. Используйте длинный случайный пароль.

## Разработка

```sh
cargo test
PANEL_PASSWORD=dev-password AGENT_TOKEN=dev-agent-token BIND=127.0.0.1:8080 cargo run -p aipanel-server
AIPANEL_SERVER=ws://127.0.0.1:8080/agent AIPANEL_TOKEN=dev-agent-token cargo run -p aipanel-agent
```
