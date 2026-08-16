//! Grafo estrutural do workspace — a ideia do graphify, em Rust e sem Python.
//!
//! O graphify faz três passadas: AST determinística (tree-sitter), transcrição
//! local de áudio/vídeo (Whisper) e extração semântica por LLM. Aqui está só a
//! **primeira** — que é de onde vem a economia de tokens — feita com parsing
//! por linha em vez de tree-sitter, para não arrastar dependência nem inflar o
//! binário. Sem Whisper e sem passada de LLM: construir o grafo custa zero token.
//!
//! O que ele resolve: hoje o agente descobre código com `search` + `read_file`
//! repetidos, e cada retorno é cortado em `tool_result_cap`. Uma consulta ao
//! grafo devolve um subgrafo pequeno e denso no lugar de vários arquivos.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Extensões que valem indexar (código + texto estruturado).
const INDEXED: &[&str] = &[
    "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "kt", "swift", "c", "h", "cc", "cpp",
    "hpp", "cs", "rb", "php", "sh", "sql", "toml", "md",
];

const SKIP_DIRS: &[&str] = &[
    "target", "node_modules", ".git", "dist", "build", "venv", ".venv", "__pycache__",
    ".next", "vendor", "Pods", ".cargo",
];

/// A papelada do próprio harness (`.harness_checkpoints`, `.harness_spill.jsonl`,
/// `.harness/`) não é código do usuário. Uma pasta de chat pode virar projeto
/// depois, e aí o agente encontrava cópias de cada arquivo que ele mesmo salvou
/// — e ficava tentando reconciliá-las.
pub fn is_harness_junk(name: &str) -> bool {
    name.starts_with(".harness")
}

const MAX_FILE_BYTES: u64 = 1_500_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStats {
    pub root: String,
    pub files: usize,
    pub symbols: usize,
    pub edges: usize,
    pub clusters: usize,
    pub built_at: String,
    /// Arquivos que mudaram desde a última build (0 = grafo em dia).
    pub stale_files: usize,
    /// Bytes de código cobertos pelo índice.
    pub indexed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolHit {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: usize,
    pub cluster: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryResult {
    pub query: String,
    pub symbols: Vec<SymbolHit>,
    /// Arquivos que referenciam os símbolos achados.
    pub referenced_by: Vec<String>,
    /// Vizinhos no mesmo cluster.
    pub neighbors: Vec<String>,
    /// Bytes dos arquivos que uma busca por leitura teria trazido.
    pub would_read_bytes: u64,
    /// Bytes desta resposta.
    pub answer_bytes: u64,
}

impl QueryResult {
    /// Estimativa grosseira: ~4 chars por token.
    pub fn saved_tokens(&self) -> i64 {
        (self.would_read_bytes as i64 - self.answer_bytes as i64) / 4
    }

    pub fn render(&self) -> String {
        if self.symbols.is_empty() {
            return format!(
                "graph: nothing matched \"{}\" — try a symbol name, a file, or a word from the path",
                self.query
            );
        }
        let mut out = vec![format!("query: {}", self.query)];
        out.push("symbols:".into());
        for s in &self.symbols {
            out.push(format!(
                "  {} ({}) — {}:{}  [cluster {}]",
                s.name, s.kind, s.path, s.line, s.cluster
            ));
        }
        if !self.referenced_by.is_empty() {
            out.push("referenced in:".into());
            for p in &self.referenced_by {
                out.push(format!("  {p}"));
            }
        }
        if !self.neighbors.is_empty() {
            out.push("cluster neighbours:".into());
            out.push(format!("  {}", self.neighbors.join(", ")));
        }
        out.push(format!(
            "— {} KB of file reads avoided (~{} tokens)",
            self.would_read_bytes / 1024,
            self.saved_tokens().max(0)
        ));
        out.join("\n")
    }
}

/// Raio de impacto: quem quebra se `symbol` mudar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImpactResult {
    pub symbol: String,
    /// Onde o símbolo é definido (nível 0).
    pub defined_in: Vec<String>,
    /// Um degrau por salto de referência: nível 1 chama direto, 2 chama quem chama…
    pub levels: Vec<Vec<String>>,
    pub total_files: usize,
    /// Parou por causa do teto, não porque acabou.
    pub truncated: bool,
    pub max_depth: usize,
    /// Arquivos no grafo todo — serve para saber se o raio saturou.
    pub total_in_graph: usize,
}

impl ImpactResult {
    pub fn render(&self) -> String {
        if self.defined_in.is_empty() {
            return format!(
                "impact: '{}' not found in the graph — run graph_build, or check the name",
                self.symbol
            );
        }
        let mut out = vec![format!("impact of {}", self.symbol)];
        out.push(format!("defined in: {}", self.defined_in.join(", ")));
        if self.levels.is_empty() {
            out.push("nothing references it — changing it is contained".into());
            return out.join("\n");
        }
        for (i, files) in self.levels.iter().enumerate() {
            let share = if self.total_in_graph > 0 {
                files.len() as f32 / self.total_in_graph as f32
            } else {
                0.0
            };
            out.push(format!("{} hop(s) away ({} files):", i + 1, files.len()));
            // Um nível que pega quase tudo não é informação: é o grafo dizendo
            // que o símbolo é central demais para o raio ser seletivo.
            if share >= 0.4 {
                out.push(format!(
                    "  ({:.0}% of the codebase — too broad to be useful; trust hop 1)",
                    share * 100.0
                ));
                continue;
            }
            for f in files {
                out.push(format!("  {f}"));
            }
        }
        out.push(format!(
            "{} file(s) in the blast radius{}",
            self.total_files,
            if self.truncated {
                format!(" — stopped at depth {} (there may be more)", self.max_depth)
            } else {
                String::new()
            }
        ));
        out.join("\n")
    }
}

/// Fecho transitivo pela tabela `refs`: começa nos arquivos que definem o
/// símbolo e, a cada salto, pega quem referencia algo definido no nível anterior.
///
/// `max_depth` existe porque codebase acoplada vira bola de pelo: sem teto, o
/// terceiro salto costuma devolver o projeto inteiro e a resposta perde o valor.
pub fn impact(root: &Path, symbol: &str, max_depth: usize) -> Result<ImpactResult> {
    let conn = open(root)?;
    let max_depth = max_depth.clamp(1, 6);

    let defining: Vec<(i64, String)> = {
        let mut st = conn.prepare(
            "SELECT DISTINCT f.id, f.path FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1",
        )?;
        let rows = st.query_map(params![symbol], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.flatten().collect()
    };
    if defining.is_empty() {
        return Ok(ImpactResult {
            symbol: symbol.into(),
            max_depth,
            ..Default::default()
        });
    }

    let total_in_graph: usize = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as usize;

    let mut visited: HashSet<i64> = defining.iter().map(|(id, _)| *id).collect();
    // o primeiro salto olha o símbolo pedido; os seguintes, o que a fronteira define
    let mut names: HashSet<String> = HashSet::from([symbol.to_string()]);
    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;

    let mut q_refs = conn.prepare(
        "SELECT DISTINCT f.id, f.path FROM refs r JOIN files f ON f.id = r.file_id
         WHERE r.name = ?1",
    )?;
    let mut q_syms = conn.prepare("SELECT DISTINCT name FROM symbols WHERE file_id = ?1")?;

    for depth in 1..=max_depth {
        let mut next: Vec<(i64, String)> = Vec::new();
        for n in &names {
            let rows = q_refs.query_map(params![n], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?)))?;
            for (id, path) in rows.flatten() {
                if visited.insert(id) {
                    next.push((id, path));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        next.sort_by(|a, b| a.1.cmp(&b.1));
        levels.push(next.iter().map(|(_, p)| p.clone()).collect());
        let frontier: Vec<i64> = next.iter().map(|(id, _)| *id).collect();

        if depth == max_depth {
            // ainda havia fronteira quando o teto chegou
            truncated = true;
            break;
        }
        names.clear();
        for id in &frontier {
            let rows = q_syms.query_map(params![id], |r| r.get::<_, String>(0))?;
            for n in rows.flatten() {
                names.insert(n);
            }
        }
        if names.is_empty() {
            break;
        }
    }

    Ok(ImpactResult {
        symbol: symbol.into(),
        defined_in: defining.into_iter().map(|(_, p)| p).collect(),
        total_files: visited.len(),
        levels,
        truncated,
        max_depth,
        total_in_graph,
    })
}

fn db_path(root: &Path) -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("sh", "harness", "harness")
        .context("dirs")?;
    let dir = dirs.data_dir().join("graph");
    std::fs::create_dir_all(&dir)?;
    // um banco por raiz indexada
    let key = fnv(&root.display().to_string());
    Ok(dir.join(format!("{key:016x}.sqlite3")))
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Suba isto ao mudar o schema: o grafo é cache derivado, então a migração é
/// simplesmente jogar fora e reconstruir.
const SCHEMA: i64 = 2;

fn open(root: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path(root)?)?;
    let have: i64 = conn
        .query_row(
            "SELECT v FROM meta WHERE k='schema'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if have != SCHEMA {
        conn.execute_batch(
            "DROP TABLE IF EXISTS files;
             DROP TABLE IF EXISTS symbols;
             DROP TABLE IF EXISTS refs;
             DROP TABLE IF EXISTS meta;",
        )?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            lang TEXT NOT NULL,
            size INTEGER NOT NULL,
            mtime INTEGER NOT NULL,
            cluster INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS symbols (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            line INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);
         CREATE TABLE IF NOT EXISTS refs (
            file_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL DEFAULT 'call'
         );
         CREATE INDEX IF NOT EXISTS refs_name ON refs(name);
         CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
    )?;
    conn.execute(
        "INSERT INTO meta(k,v) VALUES('schema',?1) ON CONFLICT(k) DO UPDATE SET v=?1",
        params![SCHEMA.to_string()],
    )?;
    Ok(conn)
}

fn lang_of(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "sh" => "shell",
        "sql" => "sql",
        "toml" => "toml",
        "md" => "markdown",
        _ => "text",
    }
}

/// Nome que vem logo depois de `kw ` na linha (`fn foo(`, `class Bar:`…).
fn name_after(line: &str, kw: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix(kw)?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Aceita também os prefixos de visibilidade (`pub`, `export`, `async`…).
fn strip_modifiers(line: &str) -> &str {
    let mut t = line.trim_start();
    loop {
        let mut moved = false;
        for m in [
            "pub(crate) ", "pub(super) ", "pub ", "export default ", "export ", "async ",
            "static ", "const ", "final ", "private ", "public ", "protected ", "unsafe ",
            "extern ", "declare ", "abstract ", "open ", "internal ", "override ",
        ] {
            if let Some(r) = t.strip_prefix(m) {
                t = r.trim_start();
                moved = true;
            }
        }
        if !moved {
            return t;
        }
    }
}

/// Palavras-chave de definição por linguagem: (keyword, kind).
fn keywords(lang: &str) -> &'static [(&'static str, &'static str)] {
    match lang {
        "rust" => &[
            ("fn", "fn"),
            ("struct", "struct"),
            ("enum", "enum"),
            ("trait", "trait"),
            ("type", "type"),
            ("mod", "mod"),
            ("macro_rules!", "macro"),
        ],
        "python" => &[("def", "fn"), ("class", "class")],
        "javascript" | "typescript" => &[
            ("function", "fn"),
            ("class", "class"),
            ("interface", "interface"),
            ("type", "type"),
            ("enum", "enum"),
        ],
        "go" => &[("func", "fn"), ("type", "type")],
        "java" | "kotlin" | "csharp" | "swift" => &[
            ("class", "class"),
            ("interface", "interface"),
            ("enum", "enum"),
            ("struct", "struct"),
            ("fun", "fn"),
            ("func", "fn"),
            ("void", "fn"),
        ],
        "c" | "cpp" => &[("struct", "struct"), ("class", "class"), ("enum", "enum")],
        "ruby" => &[("def", "fn"), ("class", "class"), ("module", "mod")],
        "php" => &[("function", "fn"), ("class", "class"), ("trait", "trait")],
        "shell" => &[("function", "fn")],
        "sql" => &[],
        _ => &[],
    }
}

/// Import/`use`/`require` → nome do módulo alvo (vira aresta arquivo→arquivo).
fn import_target(lang: &str, line: &str) -> Option<String> {
    let t = line.trim_start();
    let raw = match lang {
        "rust" => t.strip_prefix("use ")?.trim_end_matches(';'),
        "python" => {
            if let Some(r) = t.strip_prefix("from ") {
                r.split_whitespace().next()?
            } else {
                t.strip_prefix("import ")?.split(&[',', ' '][..]).next()?
            }
        }
        "javascript" | "typescript" => {
            let q = t.find(|c| c == '\'' || c == '"')?;
            if !t.starts_with("import") && !t.contains("require(") {
                return None;
            }
            let rest = &t[q + 1..];
            let end = rest.find(|c| c == '\'' || c == '"')?;
            &rest[..end]
        }
        "go" | "java" | "kotlin" => t
            .strip_prefix("import ")?
            .trim_matches(|c: char| c == '"' || c == ';' || c.is_whitespace()),
        "c" | "cpp" => t
            .strip_prefix("#include ")?
            .trim_matches(|c: char| c == '"' || c == '<' || c == '>' || c.is_whitespace()),
        _ => return None,
    };
    // último segmento identificável (`crate::app::foo` → `foo`; `./ui/theme` → `theme`)
    let seg = raw
        .rsplit(|c| c == ':' || c == '/' || c == '.')
        .find(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'))?;
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

struct Extracted {
    symbols: Vec<(String, String, usize)>,
    /// chamadas: `foo(` — muitas, e quase todo arquivo chama quase tudo
    refs: HashSet<String>,
    /// imports/use/require — poucos e estruturais, é o que define comunidade
    imports: HashSet<String>,
}

fn extract(lang: &str, body: &str) -> Extracted {
    let kws = keywords(lang);
    let mut symbols = Vec::new();
    let mut refs = HashSet::new();
    let mut imports = HashSet::new();
    let mut defined: HashSet<String> = HashSet::new();

    for (i, raw) in body.lines().enumerate() {
        if raw.len() > 400 {
            continue;
        }
        let line = strip_modifiers(raw);

        if lang == "markdown" {
            if let Some(rest) = line.strip_prefix("# ").or_else(|| line.strip_prefix("## ")) {
                symbols.push((rest.trim().to_string(), "heading".into(), i + 1));
            }
            continue;
        }

        for (kw, kind) in kws {
            if let Some(name) = name_after(line, kw) {
                defined.insert(name.clone());
                symbols.push((name, (*kind).to_string(), i + 1));
                break;
            }
        }
        // `const NAME =` / `let NAME =` em JS/TS viram símbolo também
        if matches!(lang, "javascript" | "typescript") {
            for kw in ["const", "let", "var"] {
                if let Some(name) = name_after(line, kw) {
                    if line.contains("=>") || line.contains("function") {
                        defined.insert(name.clone());
                        symbols.push((name, "fn".into(), i + 1));
                    }
                    break;
                }
            }
        }

        if let Some(t) = import_target(lang, raw) {
            imports.insert(t);
        }
        // chamadas: identificador seguido de `(`
        let bytes = line.as_bytes();
        let mut start: Option<usize> = None;
        for (j, c) in line.char_indices() {
            let ident = c.is_alphanumeric() || c == '_';
            if ident && start.is_none() {
                start = Some(j);
            } else if !ident {
                if let Some(s) = start.take() {
                    if c == '(' && j - s >= 3 {
                        let name = &line[s..j];
                        if !name.chars().next().unwrap_or('0').is_numeric()
                            && !is_noise(name)
                        {
                            refs.insert(name.to_string());
                        }
                    }
                }
            }
        }
        let _ = bytes;
    }
    // um símbolo não "referencia" a si mesmo
    for d in &defined {
        refs.remove(d);
    }
    Extracted {
        symbols,
        refs,
        imports,
    }
}

fn is_noise(name: &str) -> bool {
    matches!(
        name,
        "if" | "for" | "while" | "switch" | "match" | "return" | "fn" | "def" | "print"
            | "println" | "format" | "assert" | "catch" | "func" | "and" | "not" | "int"
            | "str" | "let" | "var" | "new" | "self" | "this" | "super" | "try" | "with"
            | "vec" | "some" | "ok" | "err" | "none" | "String" | "Vec" | "Some" | "Ok"
            | "Err" | "None"
    )
}

fn walk(root: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 12 || out.len() > 20_000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != "." {
            continue;
        }
        if p.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || is_harness_junk(&name) {
                continue;
            }
            walk(&p, out, depth + 1);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if INDEXED.contains(&ext) {
                out.push(p);
            }
        }
    }
}

fn mtime_of(p: &Path) -> i64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Constrói (ou atualiza) o grafo. `incremental` pula arquivos com mtime igual.
pub fn build(root: &Path, incremental: bool) -> Result<GraphStats> {
    let conn = open(root)?;
    let mut paths = Vec::new();
    walk(root, &mut paths, 0);

    let known: HashMap<String, (i64, i64)> = {
        let mut m = HashMap::new();
        let mut st = conn.prepare("SELECT path, id, mtime FROM files")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        })?;
        for r in rows.flatten() {
            m.insert(r.0, (r.1, r.2));
        }
        m
    };

    let seen: HashSet<String> = paths
        .iter()
        .map(|p| rel(root, p))
        .collect();
    // arquivos sumidos saem do grafo
    for (path, (id, _)) in &known {
        if !seen.contains(path) {
            conn.execute("DELETE FROM symbols WHERE file_id=?1", params![id])?;
            conn.execute("DELETE FROM refs WHERE file_id=?1", params![id])?;
            conn.execute("DELETE FROM files WHERE id=?1", params![id])?;
        }
    }

    let mut indexed_bytes = 0u64;
    for p in &paths {
        let Ok(meta) = std::fs::metadata(p) else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        indexed_bytes += meta.len();
        let path = rel(root, p);
        let mt = mtime_of(p);
        if incremental {
            if let Some((_, old)) = known.get(&path) {
                if *old == mt {
                    continue;
                }
            }
        }
        let Ok(body) = std::fs::read_to_string(p) else {
            continue;
        };
        let lang = lang_of(p.extension().and_then(|e| e.to_str()).unwrap_or(""));
        let ext = extract(lang, &body);

        conn.execute(
            "INSERT INTO files(path, lang, size, mtime) VALUES(?1,?2,?3,?4)
             ON CONFLICT(path) DO UPDATE SET lang=?2, size=?3, mtime=?4",
            params![path, lang, meta.len() as i64, mt],
        )?;
        let fid: i64 = conn.query_row(
            "SELECT id FROM files WHERE path=?1",
            params![path],
            |r| r.get(0),
        )?;
        conn.execute("DELETE FROM symbols WHERE file_id=?1", params![fid])?;
        conn.execute("DELETE FROM refs WHERE file_id=?1", params![fid])?;
        for (name, kind, line) in ext.symbols {
            conn.execute(
                "INSERT INTO symbols(file_id,name,kind,line) VALUES(?1,?2,?3,?4)",
                params![fid, name, kind, line as i64],
            )?;
        }
        for name in ext.refs {
            conn.execute(
                "INSERT INTO refs(file_id,name,kind) VALUES(?1,?2,'call')",
                params![fid, name],
            )?;
        }
        for name in ext.imports {
            conn.execute(
                "INSERT INTO refs(file_id,name,kind) VALUES(?1,?2,'import')",
                params![fid, name],
            )?;
        }
    }

    let clusters = cluster(&conn)?;
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO meta(k,v) VALUES('built_at',?1)
         ON CONFLICT(k) DO UPDATE SET v=?1",
        params![now],
    )?;
    conn.execute(
        "INSERT INTO meta(k,v) VALUES('root',?1) ON CONFLICT(k) DO UPDATE SET v=?1",
        params![root.display().to_string()],
    )?;

    let mut st = stats(root, false)?;
    st.clusters = clusters;
    st.indexed_bytes = indexed_bytes;
    Ok(st)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Comunidades por propagação de rótulo sobre o grafo arquivo↔arquivo
/// (arestas: um arquivo referencia um símbolo definido no outro).
fn cluster(conn: &Connection) -> Result<usize> {
    let mut owner: HashMap<String, i64> = HashMap::new();
    {
        let mut st = conn.prepare("SELECT name, file_id FROM symbols")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for (name, fid) in rows.flatten() {
            owner.entry(name).or_insert(fid);
        }
    }
    // Arestas ponderadas. Se toda chamada virasse aresta, o grafo fica quase
    // completo e a propagação colapsa tudo num cluster só — foi o que aconteceu
    // na primeira versão. Import é estrutura (peso 4); chamada é indício (1).
    let mut adj: HashMap<i64, HashMap<i64, u32>> = HashMap::new();
    {
        let mut st = conn.prepare("SELECT file_id, name, kind FROM refs")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for (fid, name, kind) in rows.flatten() {
            if let Some(dst) = owner.get(&name) {
                if *dst != fid {
                    let w = if kind == "import" { 4 } else { 1 };
                    *adj.entry(fid).or_default().entry(*dst).or_insert(0) += w;
                    *adj.entry(*dst).or_default().entry(fid).or_insert(0) += w;
                }
            }
        }
    }
    // laços fracos fora: sem isto tudo vira um cluster só
    const MIN_W: u32 = 4;
    for m in adj.values_mut() {
        m.retain(|_, w| *w >= MIN_W);
    }
    let ids: Vec<i64> = {
        let mut st = conn.prepare("SELECT id FROM files ORDER BY id")?;
        let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
        rows.flatten().collect()
    };
    // rótulo inicial = o próprio id; converge em poucas rodadas
    let mut label: HashMap<i64, i64> = ids.iter().map(|i| (*i, *i)).collect();
    for _ in 0..8 {
        let mut changed = false;
        for id in &ids {
            let Some(ns) = adj.get(id) else { continue };
            if ns.is_empty() {
                continue;
            }
            let mut count: HashMap<i64, u32> = HashMap::new();
            for (n, w) in ns {
                *count.entry(label[n]).or_insert(0) += *w;
            }
            // desempate pelo menor rótulo, para ser determinístico
            if let Some((best, _)) = count
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
            {
                if label[id] != best {
                    label.insert(*id, best);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // renumera para 1..n
    let mut order: Vec<i64> = label.values().copied().collect::<HashSet<_>>().into_iter().collect();
    order.sort_unstable();
    let idx: HashMap<i64, i64> = order
        .iter()
        .enumerate()
        .map(|(i, l)| (*l, i as i64 + 1))
        .collect();
    for (fid, l) in &label {
        conn.execute(
            "UPDATE files SET cluster=?1 WHERE id=?2",
            params![idx[l], fid],
        )?;
    }
    Ok(order.len())
}

/// Topologia para desenho: arquivos como nós, referências como arestas.
/// Limita a `max` arquivos mais conectados — desenhar 3 mil nós não ajuda ninguém.
pub fn topology(root: &Path, max: usize) -> Result<(Vec<(String, usize)>, Vec<(usize, usize)>)> {
    let conn = open(root)?;
    let mut stmt = conn.prepare(
        "SELECT f.path, COUNT(s.id) AS n FROM files f
         LEFT JOIN symbols s ON s.file_id = f.id
         GROUP BY f.id ORDER BY n DESC LIMIT ?1",
    )?;
    let files: Vec<(String, usize)> = stmt
        .query_map(params![max as i64], |r| {
            let path: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            Ok((path, n as usize))
        })?
        .filter_map(|x| x.ok())
        .collect();
    if files.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let names: Vec<(String, usize)> = files;
    let index: std::collections::HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.as_str(), i))
        .collect();
    // arestas: referência de um arquivo a um símbolo definido em outro
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT rf.path, df.path FROM refs r
         JOIN files rf ON rf.id = r.file_id
         JOIN symbols s ON s.name = r.name
         JOIN files df ON df.id = s.file_id
         WHERE rf.id != df.id LIMIT 4000",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows.flatten() {
        if let (Some(&a), Some(&b)) = (index.get(row.0.as_str()), index.get(row.1.as_str())) {
            if a != b && !edges.contains(&(a, b)) {
                edges.push((a, b));
            }
        }
        if edges.len() >= 300 {
            break;
        }
    }
    Ok((names, edges))
}

pub fn stats(root: &Path, with_stale: bool) -> Result<GraphStats> {
    let conn = open(root)?;
    let files: usize = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
    let symbols: usize = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
    let edges: usize = conn.query_row("SELECT COUNT(*) FROM refs", [], |r| r.get(0))?;
    let clusters: usize = conn
        .query_row("SELECT COUNT(DISTINCT cluster) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let built_at: String = conn
        .query_row("SELECT v FROM meta WHERE k='built_at'", [], |r| r.get(0))
        .unwrap_or_default();
    let indexed_bytes: i64 = conn
        .query_row("SELECT COALESCE(SUM(size),0) FROM files", [], |r| r.get(0))
        .unwrap_or(0);

    // Quantos arquivos no disco estão mais novos que o índice. Custa uma
    // varredura do disco, então só quando pedido (o polling da GUI passa false).
    let mut stale = 0usize;
    if with_stale {
        let mut paths = Vec::new();
        walk(root, &mut paths, 0);
        let mut st = conn.prepare("SELECT mtime FROM files WHERE path=?1")?;
        for p in &paths {
            let path = rel(root, p);
            let known: Option<i64> = st.query_row(params![path], |r| r.get(0)).ok();
            match known {
                Some(m) if m == mtime_of(p) => {}
                _ => stale += 1,
            }
        }
    }

    Ok(GraphStats {
        root: root.display().to_string(),
        files,
        symbols,
        edges,
        clusters,
        built_at,
        stale_files: stale,
        indexed_bytes: indexed_bytes as u64,
    })
}

/// Consulta: casa símbolos e caminhos, expande por referências e cluster.
/// `read_cap` = `Config::tool_result_cap`: é o teto do que um `read_file`
/// devolveria, então a economia é medida contra o que o agente realmente leria.
pub fn query(root: &Path, q: &str, limit: usize, read_cap: u64) -> Result<QueryResult> {
    let conn = open(root)?;
    let terms: Vec<String> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_lowercase())
        .collect();
    if terms.is_empty() {
        return Ok(QueryResult {
            query: q.into(),
            ..Default::default()
        });
    }

    let mut hits: Vec<SymbolHit> = Vec::new();
    let mut seen = HashSet::new();
    {
        let mut st = conn.prepare(
            "SELECT s.name, s.kind, f.path, s.line, f.cluster
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE LOWER(s.name) LIKE ?1 OR LOWER(f.path) LIKE ?1
             ORDER BY LENGTH(s.name) ASC LIMIT ?2",
        )?;
        for t in &terms {
            let pat = format!("%{t}%");
            let rows = st.query_map(params![pat, limit as i64], |r| {
                Ok(SymbolHit {
                    name: r.get(0)?,
                    kind: r.get(1)?,
                    path: r.get(2)?,
                    line: r.get::<_, i64>(3)? as usize,
                    cluster: r.get(4)?,
                })
            })?;
            for h in rows.flatten() {
                if seen.insert((h.path.clone(), h.line)) {
                    hits.push(h);
                }
            }
        }
    }
    hits.truncate(limit);

    let mut referenced_by: Vec<String> = Vec::new();
    let mut neighbors: Vec<String> = Vec::new();
    let mut touched: HashSet<String> = hits.iter().map(|h| h.path.clone()).collect();

    if !hits.is_empty() {
        let mut st = conn.prepare(
            "SELECT DISTINCT f.path FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.name = ?1 LIMIT 12",
        )?;
        for h in &hits {
            let rows = st.query_map(params![h.name], |r| r.get::<_, String>(0))?;
            for p in rows.flatten() {
                if p != h.path && !referenced_by.contains(&p) {
                    touched.insert(p.clone());
                    referenced_by.push(p);
                }
            }
        }
        let cl = hits[0].cluster;
        let mut st = conn.prepare(
            "SELECT path FROM files WHERE cluster=?1 ORDER BY size DESC LIMIT 8",
        )?;
        let rows = st.query_map(params![cl], |r| r.get::<_, String>(0))?;
        for p in rows.flatten() {
            if !touched.contains(&p) {
                neighbors.push(p);
            }
        }
    }
    referenced_by.truncate(12);

    // custo evitado: tamanho dos arquivos que a resposta aponta
    let mut would = 0u64;
    {
        let mut st = conn.prepare("SELECT size FROM files WHERE path=?1")?;
        for p in touched.iter() {
            let s: i64 = st.query_row(params![p], |r| r.get(0)).unwrap_or(0);
            would += (s as u64).min(read_cap);
        }
    }

    let mut res = QueryResult {
        query: q.into(),
        symbols: hits,
        referenced_by,
        neighbors,
        would_read_bytes: would,
        answer_bytes: 0,
    };
    res.answer_bytes = res.render().len() as u64;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_symbols_and_refs() {
        let body = "use crate::theme::pal;\npub fn send_user_message(&mut self) {\n    let p = pal();\n    helper_thing(p);\n}\nstruct Foo;\n";
        let e = extract("rust", body);
        let names: Vec<&str> = e.symbols.iter().map(|s| s.0.as_str()).collect();
        assert!(names.contains(&"send_user_message"), "{names:?}");
        assert!(names.contains(&"Foo"), "{names:?}");
        assert!(e.refs.contains("helper_thing"), "{:?}", e.refs);
        // `pal` entra como chamada (`pal()`) e como import (`use ...::pal`)
        assert!(e.refs.contains("pal"));
        assert!(e.imports.contains("pal"), "{:?}", e.imports);
    }

    #[test]
    fn extracts_python_and_js() {
        let py = extract("python", "from a.b import c\ndef run(x):\n    helper_call(x)\nclass Thing:\n    pass\n");
        assert!(py.imports.contains("b"), "{:?}", py.imports);
        let names: Vec<&str> = py.symbols.iter().map(|s| s.0.as_str()).collect();
        assert!(names.contains(&"run") && names.contains(&"Thing"), "{names:?}");
        assert!(py.refs.contains("helper_call"));

        let js = extract("javascript", "import x from './ui/theme';\nexport function draw(){ paintThing(); }\n");
        let names: Vec<&str> = js.symbols.iter().map(|s| s.0.as_str()).collect();
        assert!(names.contains(&"draw"), "{names:?}");
        // import agora vive separado de chamada — é ele que forma comunidade
        assert!(js.imports.contains("theme"), "{:?}", js.imports);
        assert!(js.refs.contains("paintThing"), "{:?}", js.refs);
    }

    #[test]
    fn impact_para_quando_ninguem_referencia() {
        // símbolo inexistente: sem grafo montado, o resultado é vazio e honesto
        let r = ImpactResult {
            symbol: "nada".into(),
            ..Default::default()
        };
        assert!(r.render().contains("not found"));
    }

    /// Roda de verdade sobre o src/ deste repo:
    #[test]
    fn papelada_do_harness_nao_e_projeto() {
        // uma pasta de chat pode virar projeto depois; o que o harness gravou
        // lá dentro não pode voltar como se fosse código do usuário
        assert!(is_harness_junk(".harness"));
        assert!(is_harness_junk(".harness_checkpoints"));
        assert!(is_harness_junk(".harness_spill.jsonl"));
        assert!(is_harness_junk(".harness_chat.txt"));
        // e não pode pegar nome legítimo parecido
        assert!(!is_harness_junk("harness"));
        assert!(!is_harness_junk("src"));
    }

    /// `cargo test -- --ignored graph_end_to_end --nocapture`
    #[test]
    #[ignore]
    fn graph_end_to_end() {
        let root = std::path::Path::new("src");
        let st = build(root, false).unwrap();
        eprintln!("{st:?}");
        assert!(st.files > 10 && st.symbols > 100);
        let q = query(root, "send_user_message", 6, 12_000).unwrap();
        eprintln!("{}", q.render());
        assert!(!q.symbols.is_empty());
        assert!(q.saved_tokens() > 0);

        // raio de impacto de um símbolo bem conectado
        let i = impact(root, "pal", 3).unwrap();
        eprintln!("{}", i.render());
        assert!(!i.defined_in.is_empty(), "pal deve estar no grafo");
        assert!(i.total_files > 1, "pal é usado em vários arquivos");

        // e de um que ninguém chama
        let none = impact(root, "simbolo_que_nao_existe_xyz", 3).unwrap();
        assert!(none.defined_in.is_empty());
        assert!(none.render().contains("not found"));
    }
}
