//! HTML → markdown limpo, para o agente ler página sem pagar token de menu.
//!
//! Três decisões que explicam o resto do arquivo:
//!
//! 1. **Varredura linear.** A versão antiga (`browser::html_to_text`) chamava
//!    `html[i..].to_ascii_lowercase()` a cada byte — cópia do resto do
//!    documento inteiro por caractere. Aqui o scanner anda uma vez só.
//! 2. **Readability por âncora, não por pontuação.** Se existe `<article>` ou
//!    `<main>`, o conteúdo é aquilo; senão, o corpo menos a lista de ruído.
//!    Pontuar densidade de texto acerta mais em teoria e é bem mais código.
//! 3. **Markdown, não texto plano.** Título, link e bloco de código sobrevivem;
//!    é o que o modelo precisa pra citar a fonte e copiar snippet.

/// Tags cujo conteúdo nunca interessa.
const DROP_TAGS: &[&str] = &[
    "script", "style", "noscript", "svg", "canvas", "iframe", "nav", "footer",
    "aside", "form", "button", "select", "template",
];

/// Só estes aceitam descarte por classe/id. Um `<a class="header">` dentro de
/// um `<h2>` é o link âncora do título, não o cabeçalho do site — checar
/// classe em elemento inline apagava o texto do título.
const NOISE_CONTAINERS: &[&str] = &[
    "div", "section", "aside", "header", "footer", "p", "table", "ul", "ol",
];

/// Marcas de classe/id que quase sempre são cromo de site.
const NOISE_HINTS: &[&str] = &[
    "nav", "menu", "sidebar", "footer", "header", "cookie", "consent", "banner",
    "advert", "-ads", "ads-", "social", "share", "newsletter", "subscribe",
    "breadcrumb", "pagination", "related", "promo",
];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub title: String,
    pub markdown: String,
    /// Links absolutos encontrados no conteúdo (deduplicados, na ordem).
    pub links: Vec<String>,
}

// ---------------------------------------------------------------------------
// Scanner: uma passada, sem alocar o resto do documento
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Open { name: String, attrs: &'a str },
    Close { name: String },
    Text(&'a str),
}

struct Scanner<'a> {
    src: &'a str,
    i: usize,
}

impl<'a> Iterator for Scanner<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let b = self.src.as_bytes();
        if self.i >= b.len() {
            return None;
        }
        if b[self.i] == b'<' {
            // comentário / doctype: pula até o fechamento
            if self.src[self.i..].starts_with("<!--") {
                let end = self.src[self.i..].find("-->").map(|p| self.i + p + 3);
                self.i = end.unwrap_or(b.len());
                return self.next();
            }
            let close = match self.src[self.i..].find('>') {
                Some(p) => self.i + p,
                None => {
                    self.i = b.len();
                    return None;
                }
            };
            let inner = &self.src[self.i + 1..close];
            self.i = close + 1;
            if inner.starts_with('!') || inner.starts_with('?') {
                return self.next();
            }
            let is_close = inner.starts_with('/');
            let body = if is_close { &inner[1..] } else { inner };
            let split = body
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(body.len());
            let name = body[..split].to_ascii_lowercase();
            if name.is_empty() {
                return self.next();
            }
            return Some(if is_close {
                Token::Close { name }
            } else {
                Token::Open {
                    name,
                    attrs: &body[split..],
                }
            });
        }
        let end = self.src[self.i..].find('<').map(|p| self.i + p).unwrap_or(b.len());
        let text = &self.src[self.i..end];
        self.i = end;
        Some(Token::Text(text))
    }
}

fn scan(src: &str) -> Scanner<'_> {
    Scanner { src, i: 0 }
}

/// Valor de um atributo, aspas simples ou duplas. `attrs` é o resto da tag.
fn attr(attrs: &str, key: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(key) {
        let at = from + p;
        let before_ok = at == 0
            || lower.as_bytes()[at - 1].is_ascii_whitespace()
            || lower.as_bytes()[at - 1] == b'"';
        let rest = &attrs[at + key.len()..];
        let rest_t = rest.trim_start();
        if before_ok && rest_t.starts_with('=') {
            let v = rest_t[1..].trim_start();
            let quote = v.chars().next()?;
            return Some(if quote == '"' || quote == '\'' {
                v[1..].split(quote).next().unwrap_or("").to_string()
            } else {
                v.split_whitespace().next().unwrap_or("").to_string()
            });
        }
        from = at + key.len();
    }
    None
}

fn looks_like_noise(attrs: &str) -> bool {
    let hay = format!(
        "{} {}",
        attr(attrs, "class").unwrap_or_default(),
        attr(attrs, "id").unwrap_or_default()
    )
    .to_ascii_lowercase();
    if hay.trim().is_empty() {
        return false;
    }
    NOISE_HINTS.iter().any(|h| hay.contains(h))
}

// ---------------------------------------------------------------------------
// URL: junta href relativo com a base (sem crate de url)
// ---------------------------------------------------------------------------

pub fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return String::new();
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let scheme_end = base.find("://").map(|p| p + 3).unwrap_or(0);
    let scheme = &base[..scheme_end];
    if href.starts_with("//") {
        let s = if scheme.is_empty() { "https:" } else { scheme.trim_end_matches("//") };
        return format!("{s}{href}");
    }
    let after = &base[scheme_end..];
    let host_end = after.find('/').map(|p| scheme_end + p).unwrap_or(base.len());
    let origin = &base[..host_end];
    if href.starts_with('/') {
        return format!("{origin}{href}");
    }
    // relativo ao diretório atual
    let path = &base[host_end..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let dir = match path.rfind('/') {
        Some(p) => &path[..=p],
        None => "/",
    };
    format!("{origin}{dir}{href}")
}

pub fn origin_of(url: &str) -> String {
    let scheme_end = url.find("://").map(|p| p + 3).unwrap_or(0);
    let after = &url[scheme_end..];
    let host_end = after.find('/').map(|p| scheme_end + p).unwrap_or(url.len());
    url[..host_end].to_string()
}

// ---------------------------------------------------------------------------
// Entidades
// ---------------------------------------------------------------------------

fn entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        let end = tail.find(';').filter(|e| *e <= 8);
        match end {
            Some(e) => {
                let name = &tail[1..e];
                let repl = match name {
                    "nbsp" => " ".to_string(),
                    "lt" => "<".into(),
                    "gt" => ">".into(),
                    "amp" => "&".into(),
                    "quot" => "\"".into(),
                    "apos" | "#39" => "'".into(),
                    "mdash" => "—".into(),
                    "ndash" => "–".into(),
                    "hellip" => "…".into(),
                    n if n.starts_with("#x") || n.starts_with("#X") => {
                        u32::from_str_radix(&n[2..], 16)
                            .ok()
                            .and_then(char::from_u32)
                            .map(String::from)
                            .unwrap_or_else(|| tail[..=e].to_string())
                    }
                    n if n.starts_with('#') => n[1..]
                        .parse::<u32>()
                        .ok()
                        .and_then(char::from_u32)
                        .map(String::from)
                        .unwrap_or_else(|| tail[..=e].to_string()),
                    _ => tail[..=e].to_string(),
                };
                out.push_str(&repl);
                rest = &tail[e + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Converte HTML em markdown. `base_url` resolve links relativos (pode ser "").
pub fn extract(html: &str, base_url: &str) -> Page {
    let title = title_of(html);
    let body = main_content(html);
    let (markdown, links) = render(body, base_url);
    Page {
        title,
        markdown: tidy(&markdown),
        links,
    }
}

fn title_of(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let start = match lower.find("<title") {
        Some(p) => p,
        None => return String::new(),
    };
    let open_end = match lower[start..].find('>') {
        Some(p) => start + p + 1,
        None => return String::new(),
    };
    let end = lower[open_end..]
        .find("</title")
        .map(|p| open_end + p)
        .unwrap_or(html.len());
    entities(html[open_end..end].trim()).trim().to_string()
}

/// Recorta o miolo: `<article>` > `<main>` > `<body>` > documento inteiro.
fn main_content(html: &str) -> &str {
    for tag in ["article", "main", "body"] {
        if let Some(slice) = slice_of(html, tag) {
            // âncora vazia não vale — cai pro próximo candidato
            if slice.len() > 200 {
                return slice;
            }
        }
    }
    html
}

fn slice_of<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let start = lower.find(&open)?;
    let content_start = start + lower[start..].find('>')? + 1;
    let close = format!("</{tag}");
    let end = lower[content_start..]
        .find(&close)
        .map(|p| content_start + p)
        .unwrap_or(html.len());
    Some(&html[content_start..end])
}

fn render(html: &str, base: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(html.len() / 4);
    let mut links: Vec<String> = Vec::new();
    // pilha de skip: quantas tags de ruído estão abertas
    let mut skip_depth = 0usize;
    let mut skip_tag: Option<String> = None;
    let mut in_pre = false;
    let mut list_ordered: Vec<bool> = Vec::new();
    let mut item_no: Vec<usize> = Vec::new();
    let mut pending_link: Option<(String, usize)> = None;

    for tok in scan(html) {
        match tok {
            Token::Open { name, attrs } => {
                if skip_tag.is_some() {
                    if Some(&name) == skip_tag.as_ref() {
                        skip_depth += 1;
                    }
                    continue;
                }
                if DROP_TAGS.contains(&name.as_str())
                    || (NOISE_CONTAINERS.contains(&name.as_str()) && looks_like_noise(attrs))
                {
                    skip_tag = Some(name);
                    skip_depth = 1;
                    continue;
                }
                match name.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        let n = name[1..].parse::<usize>().unwrap_or(1);
                        push_block(&mut out);
                        out.push_str(&"#".repeat(n));
                        out.push(' ');
                    }
                    "p" | "div" | "section" | "tr" | "table" | "blockquote" => push_block(&mut out),
                    "br" => out.push('\n'),
                    "hr" => {
                        push_block(&mut out);
                        out.push_str("---");
                        push_block(&mut out);
                    }
                    "ul" | "ol" => {
                        push_block(&mut out);
                        list_ordered.push(name == "ol");
                        item_no.push(0);
                    }
                    "li" => {
                        out.push('\n');
                        let depth = list_ordered.len().saturating_sub(1);
                        out.push_str(&"  ".repeat(depth));
                        if *list_ordered.last().unwrap_or(&false) {
                            let n = item_no.last_mut().map(|n| { *n += 1; *n }).unwrap_or(1);
                            out.push_str(&format!("{n}. "));
                        } else {
                            out.push_str("- ");
                        }
                    }
                    "pre" => {
                        push_block(&mut out);
                        out.push_str("```\n");
                        in_pre = true;
                    }
                    "code" if !in_pre => out.push('`'),
                    "strong" | "b" => out.push_str("**"),
                    "em" | "i" => out.push('*'),
                    "a" => {
                        if let Some(href) = attr(attrs, "href") {
                            let abs = resolve(base, &href);
                            if !abs.is_empty() {
                                pending_link = Some((abs, out.len()));
                                out.push('[');
                            }
                        }
                    }
                    _ => {}
                }
            }
            Token::Close { name } => {
                if let Some(t) = skip_tag.clone() {
                    if name == t {
                        skip_depth -= 1;
                        if skip_depth == 0 {
                            skip_tag = None;
                        }
                    }
                    continue;
                }
                match name.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "blockquote" => {
                        push_block(&mut out)
                    }
                    "ul" | "ol" => {
                        list_ordered.pop();
                        item_no.pop();
                        push_block(&mut out);
                    }
                    "pre" => {
                        in_pre = false;
                        out.push_str("\n```");
                        push_block(&mut out);
                    }
                    "code" if !in_pre => out.push('`'),
                    "strong" | "b" => out.push_str("**"),
                    "em" | "i" => out.push('*'),
                    "a" => {
                        if let Some((href, at)) = pending_link.take() {
                            // link sem texto vira nada em vez de "[]()"
                            if out[at..].trim() == "[" {
                                out.truncate(at);
                            } else {
                                out.push_str(&format!("]({href})"));
                                if !links.contains(&href) {
                                    links.push(href);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Token::Text(t) => {
                if skip_tag.is_some() || t.is_empty() {
                    continue;
                }
                let decoded = entities(t);
                if in_pre {
                    out.push_str(&decoded);
                } else {
                    let flat = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
                    if flat.is_empty() {
                        if decoded.contains(char::is_whitespace)
                            && !out.ends_with([' ', '\n', '['])
                        {
                            out.push(' ');
                        }
                    } else {
                        if decoded.starts_with(char::is_whitespace)
                            && !out.ends_with([' ', '\n', '['])
                        {
                            out.push(' ');
                        }
                        out.push_str(&flat);
                        if decoded.ends_with(char::is_whitespace) {
                            out.push(' ');
                        }
                    }
                }
            }
        }
    }
    (out, links)
}

fn push_block(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        if out.ends_with('\n') {
            out.push('\n');
        } else {
            out.push_str("\n\n");
        }
    }
}

/// Colapsa linhas em branco em excesso e espaços de sobra.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blanks = 0;
    for line in s.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
            out.push('\n');
        } else {
            blanks = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// Todos os hrefs do documento, inclusive os do menu.
///
/// `Page.links` traz só os links do **conteúdo** — é o que citar numa resposta.
/// O crawler precisa do oposto: navegação de site é justamente onde estão os
/// links para as outras páginas. Duas necessidades, duas funções.
pub fn all_links(html: &str, base: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in scan(html) {
        if let Token::Open { name, attrs } = tok {
            if name != "a" {
                continue;
            }
            if let Some(href) = attr(attrs, "href") {
                let abs = resolve(base, &href);
                if !abs.is_empty() && !out.contains(&abs) {
                    out.push(abs);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// robots.txt — o mínimo para não bater onde pediram para não bater
// ---------------------------------------------------------------------------

/// Prefixos proibidos para `User-agent: *`.
pub fn robots_disallow(robots_txt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut applies = false;
    for raw in robots_txt.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = match line.split_once(':') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim()),
            None => continue,
        };
        match k.as_str() {
            "user-agent" => applies = v == "*",
            "disallow" if applies && !v.is_empty() => out.push(v.to_string()),
            _ => {}
        }
    }
    out
}

pub fn robots_allows(disallow: &[String], url: &str) -> bool {
    let origin = origin_of(url);
    let path = url.strip_prefix(&origin).unwrap_or("/");
    let path = if path.is_empty() { "/" } else { path };
    !disallow.iter().any(|d| path.starts_with(d.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
    <html><head><title>Guia &amp; Notas</title></head>
    <body>
      <nav class="site-nav"><a href="/home">Home</a><a href="/about">About</a></nav>
      <div class="cookie-banner">We use cookies <a href="/privacy">Privacy</a></div>
      <article>
        <h1>Instalar</h1>
        <p>Rode o comando e leia a <a href="/docs/ref">referência</a>.</p>
        <h2>Passos</h2>
        <ul><li>Baixar</li><li>Extrair</li></ul>
        <pre><code>cargo build --release</code></pre>
        <p>Fim &mdash; <strong>pronto</strong>.</p>
      </article>
      <footer><a href="/legal">Legal</a></footer>
      <script>var x = "<h1>não sou conteúdo</h1>";</script>
    </body></html>"#;

    #[test]
    fn titulo_cabecalho_link_lista_e_codigo_sobrevivem() {
        let p = extract(PAGE, "https://ex.com/guia/");
        assert_eq!(p.title, "Guia & Notas");
        assert!(p.markdown.contains("# Instalar"), "{}", p.markdown);
        assert!(p.markdown.contains("## Passos"));
        assert!(p.markdown.contains("[referência](https://ex.com/docs/ref)"));
        assert!(p.markdown.contains("- Baixar"));
        assert!(p.markdown.contains("```"));
        assert!(p.markdown.contains("cargo build --release"));
        assert!(p.markdown.contains("**pronto**"));
    }

    #[test]
    fn cromo_de_site_nao_entra() {
        let p = extract(PAGE, "https://ex.com/guia/");
        for ruido in ["Home", "About", "cookies", "Legal", "não sou conteúdo"] {
            assert!(!p.markdown.contains(ruido), "vazou {ruido}: {}", p.markdown);
        }
        // e o link de menu não polui a lista de links do crawler
        assert_eq!(p.links, vec!["https://ex.com/docs/ref".to_string()]);
    }

    /// Os dois defeitos que só a página real mostrou.
    #[test]
    fn titulo_com_ancora_e_espaco_depois_de_codigo() {
        let html = r##"<article>
            <h2 id="inst"><a class="header" href="#inst">Installation</a></h2>
            <p>use <code>rustup</code> for this, and <strong>read</strong> the docs</p>
            </article>"##;
        let p = extract(html, "https://ex.com/");
        // classe "header" no <a> é âncora de título, não cromo de site
        assert!(p.markdown.contains("## Installation"), "{}", p.markdown);
        // e o espaço depois do marcador de fechamento não some
        assert!(p.markdown.contains("`rustup` for this"), "{}", p.markdown);
        assert!(p.markdown.contains("**read** the docs"), "{}", p.markdown);
    }

    #[test]
    fn crawler_ve_o_menu_que_o_leitor_nao_ve() {
        let p = extract(PAGE, "https://ex.com/guia/");
        let todos = all_links(PAGE, "https://ex.com/guia/");
        // leitura: só o link do texto
        assert_eq!(p.links, vec!["https://ex.com/docs/ref".to_string()]);
        // travessia: menu e rodapé também contam
        assert!(todos.contains(&"https://ex.com/home".to_string()));
        assert!(todos.contains(&"https://ex.com/legal".to_string()));
        assert!(todos.contains(&"https://ex.com/docs/ref".to_string()));
    }

    #[test]
    fn href_relativo_absoluto_e_de_raiz() {
        let b = "https://ex.com/a/b/page.html?q=1";
        assert_eq!(resolve(b, "https://o.com/x"), "https://o.com/x");
        assert_eq!(resolve(b, "/x"), "https://ex.com/x");
        assert_eq!(resolve(b, "c.html"), "https://ex.com/a/b/c.html");
        assert_eq!(resolve(b, "//cdn.com/x"), "https://cdn.com/x");
        assert_eq!(resolve(b, "#topo"), "");
        assert_eq!(origin_of(b), "https://ex.com");
    }

    #[test]
    fn robots_respeita_apenas_o_bloco_curinga() {
        let txt = "User-agent: badbot\nDisallow: /\n\nUser-agent: *\nDisallow: /admin\nDisallow: /tmp # nota\n";
        let d = robots_disallow(txt);
        assert_eq!(d, vec!["/admin".to_string(), "/tmp".to_string()]);
        assert!(robots_allows(&d, "https://ex.com/docs"));
        assert!(!robots_allows(&d, "https://ex.com/admin/x"));
    }

    /// A versão antiga era O(n²) (lowercase do resto do doc por byte). Este
    /// teste falha por timeout prático se alguém reintroduzir isso.
    #[test]
    fn pagina_grande_e_linear() {
        let unit = "<p>lorem ipsum dolor sit amet <a href=\"/x\">link</a></p>";
        let big = unit.repeat(12_000); // ~600 KB
        let t = std::time::Instant::now();
        let p = extract(&big, "https://ex.com/");
        let ms = t.elapsed().as_millis();
        assert!(!p.markdown.is_empty());
        assert!(ms < 1500, "extração de 600 KB levou {ms}ms — virou quadrática?");
    }

    #[test]
    fn sem_article_usa_o_body_sem_ruido() {
        let html = "<body><div class=\"sidebar\">menu</div><p>corpo real do texto que precisa ter tamanho suficiente para virar o conteudo escolhido pelo extrator sem depender de article ou main.</p></body>";
        let p = extract(html, "");
        assert!(p.markdown.contains("corpo real"));
        assert!(!p.markdown.contains("menu"));
    }
}
