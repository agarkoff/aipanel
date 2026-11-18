'use strict';

// Подсветка синтаксиса для ленты: выводы инструментов, блоки кода в Markdown
// и диффы. Зависимостей нет. Разметка строится из DOM-узлов (span с классом
// `hl-*`), innerHTML не используется, поэтому текст остаётся текстом.
//
// Языки подобраны по выводам инструментов в реальных сессиях Claude Code и
// opencode: Kotlin, Java, JS/TS, Python, Rust, Go, C-подобные, SQL, Shell,
// PowerShell, bat, YAML, JSON, TOML/INI/properties/.env, XML/HTML, CSS,
// Markdown, Dockerfile, diff.
//
// Классы токенов: k — ключевое слово, t — тип, s — строка, n — число и
// литерал, c — комментарий, f — функция и команда, v — переменная,
// a — ключ, атрибут, параметр, m — аннотация и метаданные, p — пунктуация
// разметки, b — жирный, da/dd/dh — строки диффа.

const Highlight = (() => {
  const words = (s) => new Set(s.split(/\s+/).filter(Boolean));

  // ------------------------------------------------------------ лексер

  // Язык — список правил [класс, выражение со флагом y] и разбор
  // идентификаторов. Класс правила — строка, массив классов по группам
  // выражения или функция от совпадения, возвращающая класс или токены.
  function lex(text, def) {
    const out = [];
    let plain = '';
    const push = (cls, s) => {
      if (!s) return;
      if (!cls) {
        plain += s;
        return;
      }
      if (plain) {
        out.push(['', plain]);
        plain = '';
      }
      out.push([cls, s]);
    };
    const n = text.length;
    const ws = /[ \t\r\n]+/y;
    let i = 0;
    let prev = '';
    outer: while (i < n) {
      const ch = text.charCodeAt(i);
      if (ch === 32 || ch === 9 || ch === 10 || ch === 13) {
        ws.lastIndex = i;
        const m = ws.exec(text);
        plain += m[0];
        i += m[0].length;
        continue;
      }
      for (const [cls, re] of def.rules) {
        re.lastIndex = i;
        const m = re.exec(text);
        if (!m || !m[0].length) continue;
        if (typeof cls === 'function') {
          const r = cls(m);
          if (typeof r === 'string') push(r, m[0]);
          else for (const [c, s] of r) push(c, s);
        } else if (Array.isArray(cls)) {
          cls.forEach((c, k) => push(c, m[k + 1] || ''));
        } else {
          push(cls, m[0]);
        }
        i += m[0].length;
        prev = '';
        continue outer;
      }
      if (def.ident) {
        def.ident.lastIndex = i;
        const m = def.ident.exec(text);
        if (m) {
          const w = m[0];
          push(classify(def, w, text, i + w.length, prev), w);
          prev = def.ci ? w.toLowerCase() : w;
          i += w.length;
          continue;
        }
      }
      prev = '';
      plain += text[i++];
    }
    if (plain) out.push(['', plain]);
    return out;
  }

  // После этих слов идёт имя: функции или типа.
  const FN_DEFS = words('fun def function fn func sub');
  const TYPE_DEFS = words('class interface struct enum trait object type record impl typealias');

  function classify(def, w, text, end, prev) {
    const key = def.ci ? w.toLowerCase() : w;
    if (def.kw.has(key)) return 'k';
    if (def.lits && def.lits.has(key)) return 'n';
    if (def.types && def.types.has(key)) return 't';
    if (def.names) {
      if (FN_DEFS.has(prev)) return 'f';
      if (TYPE_DEFS.has(prev)) return 't';
    }
    if (def.macro && text[end] === '!' && text[end + 1] !== '=') return 'f';
    if (def.consts && w.length > 1 && /^[A-Z][A-Z0-9_]*$/.test(w) && /[A-Z]/.test(w.slice(1))) return 'n';
    // Имя с заглавной — тип, в том числе в вызове конструктора `Foo(…)`.
    if (def.caps && /^[A-Z]/.test(w)) return 't';
    if (def.call) {
      let j = end;
      while (text[j] === ' ') j++;
      if (text[j] === '(') return 'f';
    }
    return '';
  }

  // ------------------------------------------------------------ языки

  const LINE_C = ['c', /\/\/.*/y];
  const BLOCK_C = ['c', /\/\*[\s\S]*?(?:\*\/|$)/y];
  const HASH_C = ['c', /#.*/y];
  const DQ = ['s', /"(?:[^"\\\n]|\\.)*"?/y];
  const SQ = ['s', /'(?:[^'\\\n]|\\.)*'?/y];
  const CHAR = ['s', /'(?:[^'\\\n]|\\.{1,8})'/y];
  const NUM = ['n', /(?<![\w$])(?:0[xX][\da-fA-F_]+|0[bBoO][0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)[a-zA-Z]{0,4}(?![\w$])/y];
  const ANNOT = ['m', /@[A-Za-z_][\w.]*/y];
  const IDENT = /[A-Za-z_$][\w$]*/y;

  const clike = (o) => ({
    ident: IDENT,
    call: true,
    names: true,
    caps: true,
    consts: true,
    ...o,
    kw: words(o.kw),
    types: words(o.types || ''),
    lits: words(o.lits || 'true false null'),
  });

  const LANGS = {};

  LANGS.kotlin = clike({
    kw: `package import class interface object fun val var if else when for while do return break continue throw
      try catch finally in is as typealias this super override open abstract sealed data enum companion private
      protected public internal inline suspend lateinit const vararg reified operator infix tailrec by init
      constructor annotation inner out where external crossinline noinline expect actual value get set`,
    types: 'Int Long Short Byte Double Float Boolean Char String Unit Any Nothing',
    rules: [
      LINE_C, BLOCK_C,
      ['s', /"""[\s\S]*?(?:"""|$)/y], DQ, CHAR,
      ['f', /`[^`\n]+`/y],
      ANNOT, NUM,
    ],
  });

  LANGS.java = clike({
    kw: `abstract assert break case catch class const continue default do else enum extends final finally for goto
      if implements import instanceof interface native new package private protected public return static strictfp
      super switch synchronized this throw throws transient try volatile while var record sealed permits yield
      def`,
    types: 'boolean byte char double float int long short void String Object Integer Long Boolean',
    rules: [LINE_C, BLOCK_C, ['s', /"""[\s\S]*?(?:"""|$)/y], DQ, SQ, ANNOT, NUM],
  });

  LANGS.js = clike({
    kw: `break case catch class const continue debugger default delete do else export extends finally for function
      if import in instanceof let new return super switch this throw try typeof var void while with yield async
      await of static get set from as type interface enum implements private protected public readonly declare
      namespace abstract keyof infer satisfies is`,
    types: 'string number boolean any unknown never object bigint symbol',
    lits: 'true false null undefined NaN Infinity',
    rules: [
      LINE_C, BLOCK_C, DQ, SQ,
      ['s', /`(?:[^`\\]|\\[\s\S])*`?/y],
      ANNOT, NUM,
    ],
  });

  LANGS.rust = clike({
    kw: `as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move
      mut pub ref return self Self static struct super trait type unsafe use where while union`,
    types: `i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str`,
    lits: 'true false',
    macro: true,
    consts: true,
    rules: [
      LINE_C, BLOCK_C,
      ['s', /b?r(#*)"[\s\S]*?"\1/y],
      ['s', /b?"(?:[^"\\]|\\[\s\S])*"?/y],
      ['s', /b?'(?:[^'\\\n]|\\.[^'\n]{0,7})'/y],
      ['m', /'[A-Za-z_]\w*(?!')/y],
      ['m', /#!?\[[^\]\n]*\]/y],
      NUM,
    ],
  });

  LANGS.go = clike({
    kw: `break case chan const continue default defer else fallthrough for func go goto if import interface map
      package range return select struct switch type var`,
    types: `bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8
      uint16 uint32 uint64 uintptr any`,
    lits: 'true false nil iota',
    caps: false,
    rules: [LINE_C, BLOCK_C, DQ, ['s', /`[^`]*`?/y], CHAR, NUM],
  });

  LANGS.c = clike({
    kw: `auto break case catch class const constexpr continue default delete do else enum explicit extern for friend
      goto if inline namespace new noexcept operator private protected public register return sizeof static struct
      switch template this throw try typedef typename union using virtual volatile while override final async await
      base event foreach in is lock out params readonly ref sealed var where yield get set`,
    types: `void bool char short int long float double signed unsigned size_t string object decimal byte uint ulong
      auto`,
    lits: 'true false null nullptr NULL',
    rules: [LINE_C, BLOCK_C, ['m', /#\s*\w+.*/y], DQ, CHAR, NUM],
  });

  LANGS.python = clike({
    kw: `and as assert async await break class continue def del elif else except finally for from global if import
      in is lambda nonlocal not or pass raise return try while with yield match case self cls`,
    types: 'int float str bool bytes list dict set tuple object',
    lits: 'True False None',
    rules: [
      HASH_C,
      ['s', /[rRbBuUfF]{0,2}("""|''')[\s\S]*?(?:\1|$)/y],
      ['s', /[rRbBuUfF]{0,2}(["'])(?:(?!\1)[^\\\n]|\\.)*\1?/y],
      ['m', /@[A-Za-z_][\w.]*/y],
      NUM,
    ],
  });

  const SHELL_KW = `if then else elif fi for while until do done case esac in function select return exit export local
    readonly declare unset shift break continue source alias time eval exec trap`;
  const shellRules = () => [
    ['c', /(?<=^|[\s;&|(])#.*/my],
    ['s', /'[^']*'?/y],
    ['s', /"(?:[^"\\]|\\[\s\S])*"?/y],
    ['v', /\$(?:\{[^}\n]*\}?|[A-Za-z_]\w*|[0-9@#?$!*-])/y],
    // Присваивание и имя команды в начале команды.
    ['v', /(?<=(?:^|[;&|(`{]|\$\()[ \t]*)[A-Za-z_]\w*(?==)/my],
    [
      (m) => (SHELL_WORDS.has(m[0]) ? 'k' : 'f'),
      /(?<=(?:^|[;&|(`{!]|\$\(|\b(?:then|do|else|sudo|xargs|exec|time))[ \t]*(?:[A-Za-z_]\w*=\S*[ \t]+)*)[A-Za-z_.~\/][\w.~\/+-]*/my,
    ],
    ['a', /(?<=\s)--?[A-Za-z0-9][\w-]*/y],
  ];
  const SHELL_WORDS = words(SHELL_KW);

  LANGS.shell = {
    ident: /[A-Za-z_][\w-]*/y,
    kw: SHELL_WORDS,
    lits: words('true false'),
    rules: shellRules(),
  };

  LANGS.dockerfile = {
    ...LANGS.shell,
    rules: [
      ['k', /(?<=^[ \t]*)(?:FROM|RUN|CMD|LABEL|MAINTAINER|EXPOSE|ENV|ADD|COPY|ENTRYPOINT|VOLUME|USER|WORKDIR|ARG|ONBUILD|STOPSIGNAL|HEALTHCHECK|SHELL)\b/imy],
      ['k', /\bAS\b/y],
      ...shellRules().filter(([c]) => typeof c !== 'function'),
    ],
  };

  const PS_OPS = words(`eq ne gt ge lt le like notlike match notmatch contains notcontains in notin replace split
    join and or not xor band bor bxor bnot is isnot as f ceq cne cgt clt ieq ine clike ilike cmatch imatch creplace
    ireplace shl shr`);
  LANGS.powershell = {
    ident: /[A-Za-z_][\w]*/y,
    ci: true,
    call: false,
    kw: words(`if else elseif foreach for while do until switch function return param begin process end try catch
      finally throw break continue in filter trap exit class enum using`),
    lits: words('$true $false $null'),
    rules: [
      ['c', /<#[\s\S]*?(?:#>|$)/y],
      ['c', /#.*/y],
      ['s', /@(['"])\r?\n[\s\S]*?(?:\n\1@|$)/y],
      ['s', /"(?:[^"`]|`[\s\S]|"")*"?/y],
      ['s', /'(?:[^']|'')*'?/y],
      [(m) => (/^\$(?:true|false|null)$/i.test(m[0]) ? 'n' : 'v'), /\$(?:\{[^}]*\}?|[\w:?]+|[$^_])/y],
      ['t', /\[[A-Za-z][\w.`]*(?:\[\])?(?:,\s*[\w.]+)*\]/y],
      [(m) => (PS_OPS.has(m[0].slice(1).toLowerCase()) ? 'k' : 'a'), /(?<![\w-])-[A-Za-z][\w]*/y],
      ['f', /(?<![\w-])[A-Za-z]+-[A-Za-z][\w]*/y],
      NUM,
    ],
  };

  LANGS.bat = {
    ident: /[A-Za-z_][\w]*/y,
    ci: true,
    kw: words(`if else for in do goto call set setlocal endlocal echo exit not exist defined errorlevel equ neq lss
      leq gtr geq shift start pushd popd cd rem off on`),
    rules: [
      ['c', /(?<=^[ \t@]*)(?:rem\b|::).*/imy],
      ['m', /(?<=^[ \t]*):\w+/my],
      ['v', /%~?[\w]+%?|%%~?\w|![\w]+!/y],
      ['s', /"[^"\n]*"?/y],
    ],
  };

  LANGS.sql = {
    ident: /[A-Za-z_][\w$]*/y,
    ci: true,
    call: true,
    kw: words(`select from where and or not insert into values update set delete create table index view drop alter
      add column primary key foreign references unique default constraint check join inner left right full outer
      cross on as group by order having limit offset union all distinct case when then else end is null in exists
      between like ilike returning with recursive begin commit rollback transaction grant revoke if cascade
      sequence trigger function procedure returns language declare asc desc nulls first last fetch next rows only
      over partition window lateral using natural schema database truncate explain analyze merge matched do
      nothing conflict excluded`),
    types: words(`int integer bigint smallint serial bigserial numeric decimal real double precision float boolean
      bool char varchar text uuid date time timestamp timestamptz interval json jsonb bytea`),
    lits: words('true false null'),
    rules: [
      ['c', /--.*/y], BLOCK_C,
      ['s', /'(?:[^']|'')*'?/y],
      ['v', /"[^"\n]*"?/y],
      ['v', /[:@$]\w+/y],
      NUM,
    ],
  };

  LANGS.json = {
    ident: /[A-Za-z_]\w*/y,
    kw: new Set(),
    lits: words('true false null'),
    rules: [
      LINE_C, BLOCK_C,
      [['a', '', ''], /("(?:[^"\\\n]|\\.)*")(\s*)(:)/y],
      DQ,
      ['n', /-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/y],
    ],
  };

  LANGS.yaml = {
    ident: /[A-Za-z_][\w.-]*/y,
    kw: new Set(),
    lits: words('true false null yes no on off True False Null'),
    rules: [
      ['c', /(?<=^|\s)#.*/my],
      ['m', /(?<=^)(?:---|\.\.\.)(?=\s|$)/my],
      [['a', ''], /(?<=^[ \t]*(?:-[ \t]+)*)((?:"[^"\n]*"|'[^'\n]*'|[^\s#'"\-?:,\[\]{}][^\n:#]*?|-[^\s\n:#][^\n:#]*?))(\s*:)(?=[ \t]|$)/my],
      ['s', /"(?:[^"\\]|\\.)*"?/y],
      ['s', /'(?:[^']|'')*'?/y],
      ['m', /[&*][\w-]+|!!?[\w-]*/y],
      ['n', /(?<![\w.])-?\d+(?:\.\d+)?(?![\w.])/y],
    ],
  };

  LANGS.ini = {
    ident: /[A-Za-z_][\w-]*/y,
    kw: new Set(),
    lits: words('true false'),
    rules: [
      ['c', /(?<=^[ \t]*)[#;!].*/my],
      ['t', /(?<=^[ \t]*)\[\[?[^\]\n]*\]\]?/my],
      [['k', 'a', 'p'], /(?<=^[ \t]*)(export\s+)?([^=:\s#;!\[][^=:\n]*?)(\s*[=:])/my],
      ['s', /"""[\s\S]*?(?:"""|$)/y],
      ['s', /"(?:[^"\\\n]|\\.)*"?/y],
      ['s', /'[^'\n]*'?/y],
      ['v', /\$\{[^}\n]*\}?/y],
      ['n', /(?<![\w.-])-?\d[\d_]*(?:\.\d+)?(?![\w.:-])/y],
    ],
  };

  LANGS.css = {
    ident: /-?[A-Za-z_][\w-]*/y,
    call: true,
    kw: new Set(),
    rules: [
      BLOCK_C,
      ['k', /@[\w-]+|!important/y],
      DQ, SQ,
      [['a', ''], /(--?[\w-]+|[a-zA-Z-]+)(\s*:)(?=[^;{}]*[;}])/y],
      ['n', /#[\da-fA-F]{3,8}(?![\w-])/y],
      ['n', /(?<![\w-])-?\d*\.?\d+(?:[a-zA-Z]+|%)?/y],
    ],
  };

  function tagTokens(m) {
    const s = m[0];
    const head = /^(<\/?)([^\s>\/]+)/.exec(s);
    const out = [['p', head[1]], ['t', head[2]]];
    const re = /(\s+)|([^\s=>\/"']+)|(=)|("[^"]*"|'[^']*')|(\/?>)|([\s\S])/g;
    re.lastIndex = head[0].length;
    let p;
    while ((p = re.exec(s))) {
      if (p[1]) out.push(['', p[1]]);
      else if (p[2]) out.push(['a', p[2]]);
      else if (p[3]) out.push(['p', p[3]]);
      else if (p[4]) out.push(['s', p[4]]);
      else if (p[5]) out.push(['p', p[5]]);
      else out.push(['', p[6]]);
    }
    return out;
  }

  LANGS.xml = {
    ident: null,
    kw: new Set(),
    rules: [
      ['', /[^<&\s]+/y],
      ['c', /<!--[\s\S]*?(?:-->|$)/y],
      ['s', /<!\[CDATA\[[\s\S]*?(?:\]\]>|$)/y],
      ['m', /<[!?][^>]*>?/y],
      [tagTokens, /<\/?[A-Za-z_][\w:.-]*(?:[^>"'<]|"[^"]*"|'[^']*')*>/y],
      ['m', /&[#\w]+;/y],
    ],
  };

  LANGS.markdown = {
    ident: null,
    kw: new Set(),
    rules: [
      ['s', /(?<=^[ \t]*)(```|~~~)[^\n]*(?:\n[\s\S]*?(?:\n[ \t]*\1[^\n]*|(?![\s\S])))?/my],
      ['k', /(?<=^[ \t]*)#{1,6}[ \t].*/my],
      ['c', /(?<=^[ \t]*)>.*/my],
      ['p', /(?<=^[ \t]*)(?:[-*+]|\d{1,9}[.)])(?=[ \t])/my],
      ['m', /(?<=^[ \t]*)(?:[-*_][ \t]*){3,}$/my],
      ['s', /`[^`\n]+`/y],
      ['b', /\*\*[^*\n]+\*\*|__[^_\n]+__/y],
      ['a', /!?\[[^\]\n]*\]\([^)\n]*\)/y],
      ['a', /https?:\/\/[^\s<>()]+/y],
      ['', /[^\s`*_\[!h#>\-+\d|]+/y],
    ],
  };

  LANGS.diff = {
    ident: null,
    kw: new Set(),
    rules: [
      ['m', /(?<=^)(?:diff |index |\+\+\+ |--- |new file|deleted file|similarity|rename ).*/my],
      ['dh', /(?<=^)@@.*/my],
      ['da', /(?<=^)\+.*/my],
      ['dd', /(?<=^)-.*/my],
      ['', /.+/y],
    ],
  };

  // Псевдонимы: расширения файлов и имена языков в ```блоках```.
  const ALIASES = {
    kotlin: 'kt kts kotlin',
    java: 'java groovy gradle scala sc jenkinsfile',
    js: 'js mjs cjs jsx javascript node ts tsx mts cts typescript vue svelte',
    rust: 'rs rust',
    go: 'go golang',
    c: 'c h cpp cc cxx hpp hh cs csharp c++ objc m swift dart php',
    python: 'py pyw pyi python python3 py3 gyp',
    shell: 'sh bash zsh ksh fish shell shellscript console terminal bashrc zshrc profile',
    powershell: 'ps1 psm1 psd1 powershell pwsh ps posh',
    bat: 'bat cmd batch',
    sql: 'sql psql plsql pgsql mysql sqlite tsql ddl',
    json: 'json jsonc json5 jsonl ndjson geojson webmanifest har ipynb lock-json',
    yaml: 'yaml yml',
    ini: 'toml ini cfg conf properties env dotenv editorconfig gitconfig npmrc desktop service lock',
    xml: `xml html htm xhtml svg xsd xsl xslt vcxproj csproj fsproj vbproj props targets plist wsdl pom fxml xaml
      resx config nuspec manifest`,
    css: 'css scss sass less',
    markdown: 'md markdown mdx mkd',
    dockerfile: 'dockerfile docker containerfile',
    diff: 'diff patch',
  };
  const ALIAS = new Map();
  for (const [lang, list] of Object.entries(ALIASES)) for (const a of words(list)) ALIAS.set(a, lang);
  // Файлы без расширения или с особым именем.
  const NAMES = new Map([
    ['dockerfile', 'dockerfile'], ['containerfile', 'dockerfile'], ['jenkinsfile', 'java'],
    ['.bashrc', 'shell'], ['.zshrc', 'shell'], ['.profile', 'shell'], ['.bash_profile', 'shell'],
    ['cargo.lock', 'ini'], ['.gitconfig', 'ini'], ['.editorconfig', 'ini'], ['.npmrc', 'ini'],
    ['package-lock.json', 'json'], ['makefile', null], ['.gitignore', null],
  ]);

  const lang = (name) => (name ? ALIAS.get(String(name).trim().toLowerCase()) || null : null);

  function langOfPath(path) {
    if (!path) return null;
    const base = String(path).trim().replace(/^["']|["']$/g, '').split(/[\\/]/).pop().toLowerCase();
    if (NAMES.has(base)) return NAMES.get(base);
    if (/^\.env(\.|$)/.test(base)) return 'ini';
    if (/^dockerfile[.-]/.test(base) || /\.dockerfile$/.test(base)) return 'dockerfile';
    const dot = base.lastIndexOf('.');
    return dot > 0 || (dot === 0 && base.length > 1) ? ALIAS.get(base.slice(dot + 1)) || null : null;
  }

  // ------------------------------------------------------------ DOM

  const tokens = (text, name) => {
    const def = LANGS[name];
    return def ? lex(text, def) : [['', text]];
  };

  function appendTokens(parent, toks) {
    for (const [cls, s] of toks) {
      if (!cls) {
        parent.append(s);
      } else {
        const span = document.createElement('span');
        span.className = `hl-${cls}`;
        span.textContent = s;
        parent.append(span);
      }
    }
    return parent;
  }

  const fragment = (text, name) => appendTokens(document.createDocumentFragment(), tokens(text, name));

  // Текст по строкам с общим состоянием лексера: многострочный комментарий
  // или строка остаются подсвеченными на всех своих строках.
  function lineTokens(text, name) {
    const lines = [[]];
    for (const [cls, s] of tokens(text, name)) {
      const parts = s.split('\n');
      parts.forEach((p, k) => {
        if (k) lines.push([]);
        if (p) lines[lines.length - 1].push([cls, p]);
      });
    }
    return lines;
  }

  const lines = (text, name) =>
    lineTokens(text, name).map((toks) => appendTokens(document.createDocumentFragment(), toks));

  // ------------------------------------------------------------ дифф

  const diffPath = (line) => {
    let m = /^\+\+\+ (?:b\/)?(.+?)\s*$/.exec(line);
    if (m && m[1] !== '/dev/null') return m[1];
    m = /^diff --git a\/.+? b\/(.+?)\s*$/.exec(line);
    return m ? m[1] : null;
  };

  // Содержимое строк диффа (без первого символа) по токенам. Старая и новая
  // стороны каждого хунка разбираются отдельно, чтобы многострочные
  // комментарии и строки не путались между удалённым и добавленным.
  // Возвращает массив той же длины: токены строки или null для служебных.
  function diffTokens(list, path) {
    const out = new Array(list.length).fill(null);
    let name = langOfPath(path);
    let hunk = [];
    const flush = () => {
      if (!name || !hunk.length) {
        hunk = [];
        return;
      }
      for (const side of ['-', '+']) {
        const idx = hunk.filter((k) => list[k][0] === ' ' || list[k][0] === side || list[k] === '');
        const toks = lineTokens(idx.map((k) => list[k].slice(1)).join('\n'), name);
        idx.forEach((k, j) => {
          if (list[k][0] === side || side === '+') out[k] = toks[j] || [];
        });
      }
      hunk = [];
    };
    // Сколько строк старой и новой стороны осталось в текущем хунке: по
    // счётчикам из `@@ -a,b +c,d @@` строка `--- x` внутри хунка — удалённая
    // строка, а не заголовок файла.
    let oldLeft = 0;
    let newLeft = 0;
    for (let k = 0; k < list.length; k++) {
      const line = list[k];
      if (oldLeft > 0 || newLeft > 0) {
        const c = line[0];
        if (c === '\\') continue; // «\ No newline at end of file»
        if (c === '-') oldLeft--;
        else if (c === '+') newLeft--;
        else if (c === ' ' || line === '') {
          oldLeft--;
          newLeft--;
        } else {
          oldLeft = newLeft = 0;
          flush();
          k--; // строка не из хунка: разобрать её как заголовок
          continue;
        }
        hunk.push(k);
        if (oldLeft <= 0 && newLeft <= 0) flush();
        continue;
      }
      const h = /^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@/.exec(line);
      if (h) {
        flush();
        oldLeft = h[1] === undefined ? 1 : Number(h[1]);
        newLeft = h[2] === undefined ? 1 : Number(h[2]);
        continue;
      }
      const p = diffPath(line);
      if (p) name = langOfPath(p);
    }
    flush();
    return out;
  }

  function diffLines(list, path) {
    return diffTokens(list, path).map((toks) => (toks ? appendTokens(document.createDocumentFragment(), toks) : null));
  }

  // ------------------------------------------------------------ выводы инструментов

  const READ_TOOLS = words('read view read_file readfile cat');
  const GREP_TOOLS = words('grep search_files ripgrep rg');
  const SHELL_TOOLS = words('bash shell powershell sh zsh terminal run_command execute_command monitor');

  function parseInput(text) {
    const t = (text || '').trim();
    if (t.startsWith('{')) {
      try {
        const v = JSON.parse(t);
        if (v && typeof v === 'object') return v;
      } catch {}
    }
    return { raw: t };
  }

  // Похоже ли на PowerShell: переменные окружения, командлеты Verb-Noun.
  const RE_PS = /\$env:|(?<![\w-])(?:Get|Set|New|Remove|Select|Where|ForEach|Test|Start|Stop|Write|Out|Invoke|Copy|Move|Join|Split|Resolve|Import|Export|ConvertTo|ConvertFrom|Measure|Sort|Group|Format|Add|Clear|Wait|Push|Pop)-[A-Z][A-Za-z]+|\[(?:IO|System|Text)\.|-LiteralPath\b|\$LASTEXITCODE/;
  const shellLang = (tool, command) =>
    tool === 'powershell' || RE_PS.test(command) ? 'powershell' : 'shell';

  // Простое деление команды на слова: кавычки снимаются.
  const argv = (s) => (s.match(/"[^"]*"|'[^']*'|\S+/g) || []).map((w) => w.replace(/^(["'])([\s\S]*)\1$/, '$2'));

  const READERS = words('cat bat batcat head tail less more type gc get-content nl sed');
  const FILTERS = words('head tail select-object select more less out-string cat');
  const SKIP_STMT = /^(?:cd|set-location|sl|pushd|export|set|\$\w+\s*=|[A-Za-z_]\w*=\S*$|\$ErrorActionPreference|\$LASTEXITCODE\s*=|chcp|\[Console\])/i;

  // Команды, которые печатают вывод: без `cd …`, присваиваний и т.п.
  // Каждая — слова первой команды конвейера; null, если в конвейере после
  // неё стоит не фильтр вроде `head`.
  function statements(command) {
    return command
      .split(/&&|\|\||;|\r?\n/)
      .map((s) => s.trim())
      .filter((s) => s && !SKIP_STMT.test(s))
      .map((s) => {
        const pipe = s.split(/\s\|\s?/);
        return pipe.slice(1).every((p) => FILTERS.has(cmdName(argv(p)[0]))) ? argv(pipe[0]) : null;
      });
  }

  const cmdName = (w) => (w || '').split(/[\\/]/).pop().toLowerCase().replace(/\.exe$/, '');
  const fileLangs = (args) => new Set(args.filter((a) => !a.startsWith('-')).map(langOfPath).filter(Boolean));
  const single = (set) => (set.size === 1 ? [...set][0] : null);

  // Язык файла, который читает команда: `cat`, `sed -n`, `Get-Content`,
  // `git show rev:путь`. Несколько чтений подряд — если язык у всех один.
  function readerLang(command) {
    const stmts = statements(command);
    if (!stmts.length || stmts.length > 6) return null;
    const langs = new Set();
    for (const w of stmts) {
      if (!w) return null;
      const cmd = cmdName(w[0]);
      if (cmd === 'git' && w[1] === 'show') {
        const spec = w.slice(2).find((a) => /^[^-][^:]*:./.test(a) && !/^[A-Za-z]:[\\/]/.test(a));
        const name = spec && langOfPath(spec.slice(spec.indexOf(':') + 1));
        if (!name) return null;
        langs.add(name);
        continue;
      }
      if (!READERS.has(cmd) || (cmd === 'sed' && w.some((a) => /^-[a-z]*i/.test(a)))) return null;
      const one = single(fileLangs(w.slice(1)));
      if (!one) return null;
      langs.add(one);
    }
    return single(langs);
  }

  const GREPS = words('grep egrep rg select-string sls findstr');
  // Все команды — поиск: grep, rg, git grep, Select-String. Возвращает язык
  // файлов, по которым искали (для вывода без путей), или '' — если неясен.
  function grepCommand(command) {
    const stmts = statements(command);
    if (!stmts.length || stmts.some((w) => !w)) return null;
    const langs = new Set();
    for (const w of stmts) {
      const cmd = cmdName(w[0]);
      const git = cmd === 'git' && w[1] === 'grep';
      if (!GREPS.has(cmd) && !git) return null;
      // Первое слово без «-» — шаблон, остальные — файлы.
      const rest = w.slice(git ? 2 : 1).filter((a) => !a.startsWith('-'));
      for (const l of fileLangs(rest.slice(1))) langs.add(l);
    }
    return single(langs) || '';
  }

  const RE_NUMBERED_CLAUDE = /^\s*(\d+)(?:\t|→)(.*)$/;
  const RE_NUMBERED_OC = /^(\d+): ?(.*)$/;
  const RE_OC_READ = /^<path>([^<\n]*)<\/path>\n<type>(\w+)<\/type>\n<content>\n/;
  // `путь:12:текст`, строки контекста `путь-12-текст`; у `git grep` по
  // коммиту или ветке впереди ревизия: `5d0b79c7:путь:12:текст`.
  const RE_GREP_LINE = /^((?:[^\s:\\]+:(?![\\/]))?(?:[A-Za-z]:[\\/])?[^:\n]+?)([:-])(\d+)\2(.*)$/;
  const RE_GREP_NUM = /^(\d+)([:-])(.*)$/;

  const share = (list, re) => {
    const lines = list.filter((l) => l.trim());
    return lines.length > 0 && lines.filter((l) => re.test(l)).length / lines.length >= 0.6;
  };

  const looksJson = (t) => {
    if (!/^[\[{]/.test(t)) return false;
    try {
      JSON.parse(t);
      return true;
    } catch {}
    // Обрезанный агентом JSON не разбирается, но выглядит узнаваемо.
    if (/^(?:\{\s*"[^"\n]*"\s*:|\[\s*(?:\{|"|-?\d|true|false|null|\]))/.test(t)) return true;
    const rows = t.split('\n').filter((l) => l.trim());
    return rows.length > 1 && rows.every((l) => {
      try {
        return typeof JSON.parse(l) === 'object';
      } catch {
        return false;
      }
    });
  };

  const looksDiff = (t) => /^diff --git /m.test(t) || /^--- \S.*\n\+\+\+ \S.*\n@@ /m.test(t);
  const looksXml = (t) => /^(?:<\?xml|<!DOCTYPE|<(?:html|svg|project|configuration|beans|Project|Package)\b)/i.test(t);

  // Что за вывод и как его показать. Чистая функция — проверяется тестами.
  // Режимы: code (весь текст на одном языке), numbered (строки с номерами,
  // как у Read), grep (строки «путь:номер:текст»), diff.
  function analyze(text, tool, inputText) {
    const t = String(tool || '').toLowerCase().replace(/^.*__/, '');
    const input = parseInput(inputText);
    const path = input.file_path || input.filePath || input.path || (READ_TOOLS.has(t) ? input.raw : '');
    const command = typeof input.command === 'string' ? input.command : SHELL_TOOLS.has(t) ? input.raw || '' : '';
    const body = text.replace(/…\n\[обрезано, всего \d+ симв\.\]$/, '');
    const trimmed = body.trim();
    if (!trimmed) return null;
    const list = body.split('\n');

    if (READ_TOOLS.has(t) || RE_OC_READ.test(body)) {
      const oc = RE_OC_READ.exec(body);
      if (oc) {
        if (oc[2] !== 'file') return null;
        return { mode: 'numbered', lang: langOfPath(oc[1]), re: RE_NUMBERED_OC, path: oc[1] };
      }
      // Claude Code дописывает после файла напоминания — их строки не считаем.
      if (share(body.split('\n<system-reminder>')[0].split('\n'), RE_NUMBERED_CLAUDE)) {
        return { mode: 'numbered', lang: langOfPath(path), re: RE_NUMBERED_CLAUDE, path };
      }
    }
    if (looksDiff(trimmed)) return { mode: 'diff', lang: 'diff' };
    const grepLang = command ? grepCommand(command) : null;
    if (GREP_TOOLS.has(t) || grepLang !== null) {
      const fileLang = langOfPath(path) || grepLang || null;
      if (share(list, RE_GREP_LINE)) return { mode: 'grep', re: RE_GREP_LINE };
      if (/^(?:Found \d+ match|[^\n]+:\n {2}Line \d+: )/.test(body)) return { mode: 'grep', re: null };
      if (fileLang && share(list, RE_GREP_NUM)) return { mode: 'grep', re: RE_GREP_NUM, lang: fileLang };
    }
    if (command) {
      const name = readerLang(command);
      if (name) return { mode: 'code', lang: name };
    }
    if (looksJson(trimmed)) return { mode: 'code', lang: 'json' };
    if (looksXml(trimmed)) return { mode: 'code', lang: 'xml' };
    return null;
  }

  // Язык вызова инструмента: команды — shell или PowerShell, вход — JSON.
  function inputLang(text, tool) {
    const t = String(tool || '').toLowerCase().replace(/^.*__/, '');
    const s = (text || '').trim();
    if (!s) return null;
    if (/^[\[{]/.test(s) && looksJson(s)) return 'json';
    if (SHELL_TOOLS.has(t)) return shellLang(t, s);
    return null;
  }

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
  }

  function row(num, content) {
    const r = el('div', 'hl-row');
    r.append(el('span', 'hl-ln', num), content);
    return r;
  }

  function renderNumbered(body, info) {
    const box = el('div', 'hl-lines');
    const list = body.split('\n');
    let start = 0;
    if (info.re === RE_NUMBERED_OC) {
      // Заголовок opencode: <path>…</path>, <type>…</type>, <content>.
      const head = RE_OC_READ.exec(body);
      start = head ? head[0].split('\n').length - 1 : 0;
      box.append(el('div', 'hl-note', info.path));
    }
    // Номера строк — отдельно, код — одним куском, чтобы сохранить состояние лексера.
    const nums = [];
    const code = [];
    const rows = [];
    for (let k = start; k < list.length; k++) {
      const m = info.re.exec(list[k]);
      if (m) {
        rows.push({ num: m[1], idx: code.length });
        nums.push(m[1]);
        code.push(m[2]);
      } else if (list[k] !== '</content>' && (list[k].trim() || k < list.length - 1)) {
        rows.push({ note: list[k] });
      }
    }
    const frags = lines(code.join('\n'), info.lang);
    for (const r of rows) {
      if (r.note !== undefined) {
        box.append(el('div', 'hl-note', r.note));
      } else {
        const c = el('span', 'hl-code');
        c.append(frags[r.idx] || '');
        box.append(row(r.num, c));
      }
    }
    return box;
  }

  function renderGrep(body, info, langHint) {
    const box = el('div', 'hl-lines');
    let file = null;
    for (const line of body.split('\n')) {
      let m;
      if (info.re === null) {
        // opencode: «путь:» и под ним «  Line N: текст».
        if ((m = /^ {2}Line (\d+): ?(.*)$/.exec(line))) {
          const c = el('span', 'hl-code');
          c.append(fragment(m[2], file));
          box.append(row(m[1], c));
        } else if ((m = /^(.+):$/.exec(line))) {
          file = langOfPath(m[1]);
          box.append(el('div', 'hl-note hl-file', m[1]));
        } else if (line.trim()) {
          box.append(el('div', 'hl-note', line));
        }
      } else if ((m = info.re.exec(line))) {
        const c = el('span', 'hl-code');
        if (info.re === RE_GREP_NUM) {
          c.append(fragment(m[3], info.lang || langHint));
          box.append(row(m[1], c));
        } else {
          c.append(fragment(m[4], langOfPath(m[1])));
          const r = el('div', 'hl-row');
          r.append(el('span', 'hl-ln hl-path', `${m[1]}${m[2]}${m[3]}${m[2]}`), c);
          box.append(r);
        }
      } else {
        box.append(el('div', 'hl-note', line));
      }
    }
    return box;
  }

  function renderDiff(body) {
    const box = el('div', 'hl-diff');
    const list = body.split('\n');
    if (list[list.length - 1] === '') list.pop();
    const frags = diffLines(list);
    list.forEach((line, k) => {
      const cls = diffLineClass(line);
      const d = el('div', cls ? `diff-line ${cls}` : 'diff-line');
      if (frags[k]) d.append(line.slice(0, 1), frags[k]);
      else d.textContent = line;
      box.append(d);
    });
    return box;
  }

  function diffLineClass(line) {
    if (/^(?:\+\+\+|---|Index:|===|diff --git |index [0-9a-f])/.test(line)) return 'meta';
    if (line.startsWith('@@')) return 'hunk';
    if (line.startsWith('+')) return 'add';
    if (line.startsWith('-')) return 'del';
    return '';
  }

  // Узел с подсвеченным результатом или null, если подсвечивать нечего.
  function toolResult(text, tool, inputText) {
    const info = analyze(text, tool, inputText);
    if (!info) return null;
    if (info.mode === 'numbered') return renderNumbered(text, info);
    if (info.mode === 'grep') return renderGrep(text, info, langOfPath(parseInput(inputText).path));
    if (info.mode === 'diff') return renderDiff(text);
    return fragment(text, info.lang);
  }

  function toolInput(text, tool) {
    const name = inputLang(text, tool);
    return name ? fragment(text, name) : null;
  }

  // ------------------------------------------------------------ очистка вывода

  // Вывод инструментов приходит из терминала как есть — вместе с управляющими
  // последовательностями ANSI: цветами, перерисовкой прогресс-строк, заголовком
  // окна. Терминала в ленте нет, поэтому эти команды остались бы видимым
  // текстом («[31;1m» вместо красного). Снимаем их: у перерисованных строк
  // оставляем итоговое состояние (то, что после последнего \r), прочие
  // управляющие символы выбрасываем — кроме перевода строки и табуляции.
  const RE_OSC = /\u001b\][\s\S]*?(?:\u0007|\u001b\\)/g; // заголовок окна, гиперссылка
  const RE_OSC_TAIL = /\u001b\][\s\S]*$/; // OSC без закрывающего BEL/ST
  const RE_CSI = /\u001b\[[0-?]*[ -/]*[@-~]/g; // цвета, курсор, стирание строки
  const RE_ESC = /\u001b[ -/]*[0-~]/g; // прочие ESC-последовательности
  const RE_CTRL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g;
  // Быстрая проверка: обычный текст (в том числе многострочный) не разбираем.
  const RE_SUSPECT = /[\u0000-\u0008\u000b\u000c\r\u000e-\u001f\u007f-\u009f]/;

  function plainText(text) {
    const s = String(text == null ? '' : text);
    if (!RE_SUSPECT.test(s)) return s;
    let out = s.replace(RE_OSC, '').replace(RE_OSC_TAIL, '').replace(RE_CSI, '').replace(RE_ESC, '');
    if (out.includes('\r')) {
      out = out
        .replace(/\r\n/g, '\n')
        .split('\n')
        .map((line) => (line.includes('\r') ? line.split('\r').pop() : line))
        .join('\n');
    }
    return out.replace(RE_CTRL, '');
  }

  return {
    lang,
    langOfPath,
    tokens,
    fragment,
    lines,
    diffTokens,
    diffLines,
    diffLineClass,
    analyze,
    inputLang,
    toolResult,
    toolInput,
    plainText,
  };
})();

if (typeof module !== 'undefined') module.exports = Highlight;
