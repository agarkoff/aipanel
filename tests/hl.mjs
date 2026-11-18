// Проверка подсветки (crates/server/static/hl.js) без браузера: лексеры
// языков и распознавание выводов инструментов. DOM здесь не нужен — функции
// tokens, analyze, inputLang и diffTokens работают с текстом.
//
// Запуск: node tests/hl.mjs

import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const H = require('../crates/server/static/hl.js');

let failed = 0;
function check(name, ok, details) {
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${name}`);
  if (!ok) {
    failed++;
    if (details !== undefined) console.log('     ', JSON.stringify(details));
  }
}

// Токены одного класса: `pick(tokens, 'k')` → ['fun', 'val'].
const pick = (toks, cls) => toks.filter(([c]) => c === cls).map(([, s]) => s);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
// Лексер не теряет и не дублирует текст.
const whole = (text, lang) => H.tokens(text, lang).map(([, s]) => s).join('') === text;

function lang(name, text, expect) {
  const toks = H.tokens(text, name);
  check(`${name}: текст сохраняется`, whole(text, name));
  for (const [cls, list] of Object.entries(expect)) {
    const got = pick(toks, cls);
    check(`${name}: ${cls} = ${list.join(' | ')}`, same(got, list), got);
  }
}

// ---------------------------------------------------------------- языки

lang('kotlin', '@Test\nfun `тест`() {\n  val x: String = "a\\"b" // к\n  /* блок */ return Foo(42L)\n}', {
  k: ['fun', 'val', 'return'],
  m: ['@Test'],
  s: ['"a\\"b"'],
  c: ['// к', '/* блок */'],
  n: ['42L'],
  t: ['String', 'Foo'],
  f: ['`тест`'],
});

lang('python', 'def run(x=None):\n    """doc"""\n    return f"{x}" # c\n', {
  k: ['def', 'return'],
  f: ['run'],
  n: ['None'],
  s: ['"""doc"""', 'f"{x}"'],
  c: ['# c'],
});

lang('js', "const a = `t${1}`; // c\nfunction go(b) { return b === null ? 'x' : 0x1f; }", {
  k: ['const', 'function', 'return'],
  s: ['`t${1}`', "'x'"],
  n: ['null', '0x1f'],
  f: ['go'],
  c: ['// c'],
});

lang('rust', "#[derive(Debug)]\nfn f<'a>(s: &'a str) -> Vec<u8> { println!(\"{}\", 'c'); r#\"raw\"# }", {
  m: ['#[derive(Debug)]', "'a", "'a"],
  k: ['fn'],
  f: ['f', 'println'],
  t: ['str', 'Vec', 'u8'],
  s: ['"{}"', "'c'", 'r#"raw"#'],
});

lang('go', 'func main() { var x int = nil; s := `raw` }', {
  k: ['func', 'var'],
  f: ['main'],
  t: ['int'],
  n: ['nil'],
  s: ['`raw`'],
});

lang('sql', "SELECT id, count(*) FROM t WHERE name = 'it''s' -- c\nLIMIT 10;", {
  k: ['SELECT', 'FROM', 'WHERE', 'LIMIT'],
  s: ["'it''s'"],
  c: ['-- c'],
  n: ['10'],
  f: ['count'],
});

lang('shell', 'cd /d/x && FOO=1 git log --oneline | grep -n "a $B" # c\nfor f in *; do echo $f; done', {
  f: ['cd', 'git', 'grep', 'echo'],
  v: ['FOO', '$f'],
  a: ['--oneline', '-n'],
  s: ['"a $B"'],
  c: ['# c'],
  k: ['for', 'in', 'do', 'done'],
});

lang('powershell', "$env:X = 'a'; Get-ChildItem -LiteralPath \"$p\" | Where-Object { $_.Length -gt 10 } # c\n[IO.File]::ReadAllText($f)", {
  v: ['$env:X', '$_', '$f'],
  s: ["'a'", '"$p"'],
  f: ['Get-ChildItem', 'Where-Object'],
  a: ['-LiteralPath'],
  k: ['-gt'],
  n: ['10'],
  c: ['# c'],
  t: ['[IO.File]'],
});

lang('bat', '@echo off\nrem comment\nset X=%PATH%\n:loop\ngoto loop', {
  c: ['rem comment'],
  v: ['%PATH%'],
  m: [':loop'],
  k: ['echo', 'off', 'set', 'goto'],
});

lang('json', '{"a": [1, -2.5e3, true, null], "b": "x\\"y"}', {
  a: ['"a"', '"b"'],
  n: ['1', '-2.5e3', 'true', 'null'],
  s: ['"x\\"y"'],
});

lang('yaml', '# c\nkey: value\nlist:\n  - name: "x"\n    port: 8080\n    on: true\nurl: http://a:b # t\n', {
  c: ['# c', '# t'],
  a: ['key', 'list', 'name', 'port', 'on', 'url'],
  s: ['"x"'],
  n: ['8080', 'true'],
});

lang('ini', '[package]\nname = "aipanel" # c\nversion = 3\n# full\nkey.sub=value', {
  t: ['[package]'],
  a: ['name', 'version', 'key.sub'],
  s: ['"aipanel"'],
  n: ['3'],
  c: ['# full'],
});

lang('xml', '<?xml version="1.0"?>\n<!-- c -->\n<a href="x" disabled>t &amp; <b/></a>', {
  m: ['<?xml version="1.0"?>', '&amp;'],
  c: ['<!-- c -->'],
  t: ['a', 'b', 'a'],
  a: ['href', 'disabled'],
  s: ['"x"'],
});

lang('css', '/* c */\n.a:hover { color: #fff; margin: 0 4px !important; }\n@media (x) {}', {
  c: ['/* c */'],
  a: ['color', 'margin'],
  n: ['#fff', '0', '4px'],
  k: ['!important', '@media'],
});

lang('markdown', '# Заголовок\n\n- пункт `код` и **жирно**\n\n```js\nlet x;\n```\n', {
  k: ['# Заголовок'],
  p: ['-'],
  s: ['`код`', '```js\nlet x;\n```'],
  b: ['**жирно**'],
});

lang('dockerfile', 'FROM rust:1 AS build\n# c\nRUN cargo build --release\nENV A=$B', {
  k: ['FROM', 'AS', 'RUN', 'ENV'],
  c: ['# c'],
  v: ['$B'],
  a: ['--release'],
});

// ---------------------------------------------------------------- языки по пути и имени

check('язык по расширению', H.langOfPath('C:\\x\\Main.kt') === 'kotlin' && H.langOfPath('/a/b.tsx') === 'js' &&
  H.langOfPath('a.ps1') === 'powershell' && H.langOfPath('x.xsd') === 'xml' && H.langOfPath('a.yml') === 'yaml');
check('язык по имени файла', H.langOfPath('/p/Dockerfile') === 'dockerfile' && H.langOfPath('.env.local') === 'ini' &&
  H.langOfPath('Cargo.lock') === 'ini');
check('неизвестное расширение — без подсветки', H.langOfPath('a.output') === null && H.langOfPath('README') === null);
check('язык ```блока```', H.lang('Kotlin') === 'kotlin' && H.lang('sh') === 'shell' && H.lang('text') === null);

// ---------------------------------------------------------------- выводы инструментов

const a = (text, tool, input) => H.analyze(text, tool, input) || {};

let r = a('     1\tpackage x\n     2\t\n     3\tfun main() {}\n', 'Read', 'D:\\p\\Main.kt');
check('Read Claude: строки с номерами, язык по пути', r.mode === 'numbered' && r.lang === 'kotlin', r);

r = a('1\tfn main() {}\n2\t\n<system-reminder>\nWhatever\nmore\n</system-reminder>\n', 'Read',
  '{\n  "file_path": "D:\\\\p\\\\a.rs",\n  "limit": 2\n}');
check('Read Claude с offset/limit и напоминанием', r.mode === 'numbered' && r.lang === 'rust', r);

r = a('<path>E:\\x\\races.js</path>\n<type>file</type>\n<content>\n35:     group: 1,\n36: }\n\n(Showing lines 35-36 of 352.)\n</content>', 'read',
  '{"filePath":"E:\\\\x\\\\races.js","offset":35}');
check('read opencode: путь из вывода', r.mode === 'numbered' && r.lang === 'js', r);

r = a('<path>E:\\x</path>\n<type>directory</type>\n<content>\na\nb\n</content>', 'read', 'E:\\x');
check('read opencode каталога — без подсветки', !r.mode, r);

r = a('diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n ctx\n', 'Bash', 'git diff');
check('git diff — дифф', r.mode === 'diff', r);

r = a('fn main() {}\n', 'Bash', 'cd /d/p && sed -n 1,80p crates/x/src/main.rs');
check('sed по файлу — язык файла', r.mode === 'code' && r.lang === 'rust', r);
r = a('x = 1\n', 'Bash', 'cat a.py | head -20');
check('cat | head — язык файла', r.mode === 'code' && r.lang === 'python', r);
r = a('<a/>\n', 'PowerShell', "Get-Content -LiteralPath 'C:\\x\\a.xml' -Encoding utf8");
check('Get-Content — язык файла', r.mode === 'code' && r.lang === 'xml', r);
r = a('[package]\n', 'Bash', 'git show HEAD:Cargo.toml');
check('git show rev:путь — язык файла', r.mode === 'code' && r.lang === 'ini', r);
r = a('a\nb\n', 'Bash', 'sed -n 1,5p a.rs && grep -n x b.py');
check('несколько команд — без подсветки', !r.mode, r);
r = a('a\n', 'Bash', "sed -i 's/a/b/' a.rs");
check('sed -i — не чтение файла', !r.mode, r);

r = a('{"labels":[{"id":"CHAT"}]}', 'mcp__claude_ai_Gmail__list_labels', '{}');
check('JSON-ответ', r.mode === 'code' && r.lang === 'json', r);
r = a('{"a": [1, 2, 3', 'javascript_tool', '{}');
check('обрезанный JSON', r.mode === 'code' && r.lang === 'json', r);
r = a('{"a":1}\n{"b":2}\n', 'Bash', 'cat log');
check('JSON Lines', r.mode === 'code' && r.lang === 'json', r);
r = a("{'type': 'tun'}", 'Bash', 'python x.py');
check('repr Python — не JSON', !r.mode, r);
r = a('<persisted-output>\nOutput too large\n</persisted-output>', 'Bash', 'ls');
check('служебная обёртка — не XML', !r.mode, r);
r = a('<?xml version="1.0"?>\n<a/>', 'Bash', 'curl -s http://x');
check('XML из curl', r.mode === 'code' && r.lang === 'xml', r);

r = a('src/a.rs:12:fn x() {}\nsrc/b.py:3:def y(): pass\n', 'Grep', '{"pattern":"x","output_mode":"content"}');
check('Grep Claude: путь:номер:текст', r.mode === 'grep', r);
r = a('C:\\p\\a.kt:5:val x = 1\nC:\\p\\a.kt-6-val y = 2\n', 'Bash', 'grep -rn -C1 x C:/p');
check('grep с путями Windows и контекстом', r.mode === 'grep', r);
r = a('Found 2 matches\nE:\\x\\a.md:\n  Line 13: - пункт\n', 'grep', '{"pattern":"x","path":"E:\\\\x"}');
check('grep opencode', r.mode === 'grep' && r.re === null, r);
r = a('12:fn a() {}\n40:fn b() {}\n', 'Grep', '{"pattern":"fn","path":"D:\\\\p\\\\a.rs","-n":true}');
check('Grep по одному файлу: номер:текст', r.mode === 'grep' && r.lang === 'rust', r);
r = a('167:    private fun runner() = 1\n168-        .with()\n', 'Bash', 'grep -n -A1 "fun runner" src/A.kt');
check('grep -n по одному файлу', r.mode === 'grep' && r.lang === 'kotlin', r);
r = a('a/b.py:86:    x = 1\na/c.py:3:y\n', 'Bash', 'grep -n "x" a/*.py | head -20; grep -n "y" a/c.py | head');
check('несколько grep подряд', r.mode === 'grep', r);
r = a('5d0b79c7:src/a.kt:11:    Converter<\n5d0b79c7:src/b.kt:12:    x\n', 'Bash', 'git grep -n -A3 "Converter<$" 5d0b79c7 -- "src/**"');
check('git grep по коммиту', r.mode === 'grep', r);
r = a('src/a.rs\nsrc/b.rs\n', 'Grep', '{"pattern":"x"}');
check('Grep со списком файлов — без подсветки', !r.mode, r);

r = a('total 0\ndrwxr-xr-x a\n', 'Bash', 'ls -la');
check('обычный вывод — без подсветки', !r.mode, r);
r = a('Exit code 1\nerror', 'Bash', 'cargo build');
check('ошибка сборки — без подсветки', !r.mode, r);

check('вход Bash — shell', H.inputLang('git status', 'Bash') === 'shell');
check('вход PowerShell — powershell', H.inputLang('Get-ChildItem', 'PowerShell') === 'powershell');
check('вход bash opencode с командлетами — powershell', H.inputLang("Get-ChildItem -LiteralPath 'C:\\x'", 'bash') === 'powershell');
check('вход bash opencode — shell', H.inputLang('ls -la && git log', 'bash') === 'shell');
check('вход JSON', H.inputLang('{\n  "pattern": "x"\n}', 'Grep') === 'json');
check('путь к файлу — без подсветки', H.inputLang('D:\\p\\a.rs', 'Read') === null);

// ---------------------------------------------------------------- дифф

const diff = [
  '--- a/a.sql',
  '+++ b/a.sql',
  '@@ -1,3 +1,3 @@',
  ' select 1;',
  '--- old comment',
  '+-- new comment',
  ' /* open',
  '\\ No newline at end of file',
];
const dt = H.diffTokens(diff, 'a.sql');
check('дифф: заголовки без токенов', dt[0] === null && dt[1] === null && dt[2] === null);
check('дифф: контекст подсвечен', same(pick(dt[3], 'k'), ['select']), dt[3]);
check('дифф: «--- …» внутри хунка — удалённая строка', same(pick(dt[4], 'c'), ['-- old comment']), dt[4]);
check('дифф: добавленная строка', same(pick(dt[5], 'c'), ['-- new comment']), dt[5]);
check('дифф: «\\ No newline» без токенов', dt[7] === null);

const dt2 = H.diffTokens([
  'diff --git a/x.py b/x.py',
  '@@ -1,2 +1,2 @@',
  '-s = """a',
  '+s = """b',
  ' c"""',
  'diff --git a/y.rs b/y.rs',
  '@@ -1 +1 @@',
  '-fn a() {}',
  '+fn b() {}',
]);
check('дифф: стороны разбираются отдельно', pick(dt2[4], 's').length === 1, dt2[4]);
check('дифф: язык меняется по файлам', same(pick(dt2[8], 'k'), ['fn']), dt2[8]);

// ---------------------------------------------------------------- очистка вывода

// Вывод инструментов приходит из терминала вместе с управляющими
// последовательностями ANSI. В ленте терминала нет, поэтому в тексте они не
// нужны: цвета снимаются, у прогресс-строк остаётся итоговое состояние.
check('очистка: цвета сняты', H.plainText('\u001b[31;1mтекст\u001b[0m') === 'текст');
check('очистка: обычный текст не меняется', H.plainText('строка\n\tотступ') === 'строка\n\tотступ');
check('очистка: прогресс — итоговое состояние', H.plainText(' 0%\r 50%\r100%\nготово') === '100%\nготово');
check('очистка: заголовок окна', H.plainText('\u001b]0;заголовок\u0007текст') === 'текст');
check('очистка: незакрытая OSC', H.plainText('до\u001b]8;;http://x') === 'до');
check('очистка: гиперссылка', H.plainText('\u001b]8;;http://x\u001b\\ссылка\u001b]8;;\u001b\\') === 'ссылка');
check('очистка: курсор и стирание строки', H.plainText('a\u001b[2K\u001b[1A b') === 'a b');
check('очистка: CRLF — перевод строки', H.plainText('a\r\nb') === 'a\nb');
check('очистка: пустое значение', H.plainText('') === '' && H.plainText(null) === '');

// ---------------------------------------------------------------- скорость

const big = Array.from({ length: 2000 }, (_, i) => `    val x${i} = listOf("a", 'b', ${i}) // ${i}`).join('\n');
const t0 = performance.now();
H.tokens(big, 'kotlin');
const ms = performance.now() - t0;
check(`скорость: ${big.length} симв. за ${ms.toFixed(0)} мс`, ms < 500);

if (failed) {
  console.log(`\nПровалено: ${failed}`);
  process.exit(1);
}
console.log('\nВсе проверки прошли.');
