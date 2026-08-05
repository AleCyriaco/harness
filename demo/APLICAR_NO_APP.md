# Como aplicar o redesign no harness (eframe/egui)

Ordem sugerida: **fontes → tokens de tema → navegação → tool calls → composer/settings**. Os três primeiros passos já mudam 80% da percepção e não tocam em lógica.

---

## 1. Fontes empacotadas (substitui `install_fonts` em `theme.rs`)

Baixe os `.ttf` (OFL) e coloque em `assets/fonts/`:

- `IBMPlexSans-Regular.ttf`, `IBMPlexSans-Medium.ttf`, `IBMPlexSans-SemiBold.ttf`
- `IBMPlexMono-Regular.ttf`, `IBMPlexMono-Medium.ttf`

Hoje o app lê fonte do disco do sistema (`/System/Library/Fonts/SFNS.ttf`…), o que muda a cara em cada SO. Troque por `include_bytes!`:

```rust
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();

    fonts.font_data.insert(
        "ui".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "ui_medium".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")).into(),
    );
    fonts.font_data.insert(
        "mono".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")).into(),
    );

    fonts.families.insert(FontFamily::Proportional, vec!["ui".into(), "mono".into()]);
    fonts.families.insert(FontFamily::Monospace, vec!["mono".into(), "ui".into()]);
    // família extra para pesos: use FontFamily::Name("ui_medium".into()) onde precisar
    fonts.families.insert(
        FontFamily::Name("ui_medium".into()),
        vec!["ui_medium".into(), "ui".into()],
    );

    ctx.set_fonts(fonts);
}
```

`FontDefinitions::empty()` em vez de `default()` evita carregar as fontes embutidas do egui (economiza RAM, que é objetivo do projeto). Se precisar de emoji, mantenha `default()` e só insira as suas no início dos vetores.

### Escala de texto (substitui o bloco `style.text_styles`)

```rust
use egui::FontFamily::{Monospace, Proportional};

style.text_styles = [
    (TextStyle::Small,     FontId::new(10.5, Monospace)),   // meta: RAM, daemon, tempos
    (TextStyle::Body,      FontId::new(14.5, Proportional)),// chat
    (TextStyle::Button,    FontId::new(12.5, Proportional)),
    (TextStyle::Heading,   FontId::new(20.0, Proportional)),
    (TextStyle::Monospace, FontId::new(12.5, Monospace)),   // código e caminhos
].into();

style.spacing.item_spacing   = egui::vec2(8.0, 8.0);
style.spacing.button_padding = egui::vec2(11.0, 6.0);
```

Para os rótulos `RODANDO / HOJE` do rail e dos grupos, use um helper em vez de um TextStyle novo:

```rust
pub fn micro(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .monospace()
        .size(9.5)
        .color(pal().muted)
}
```

---

## 2. Tokens de tema + claro/escuro

Hoje as cores são `pub const` em `theme.rs`, o que impede alternância. Troque por uma struct + um estado global leve:

```rust
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode { #[default] Paper, Ember }

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32, pub bg_rail: Color32, pub bg_side: Color32,
    pub card: Color32, pub raised: Color32, pub border: Color32, pub border_soft: Color32,
    pub text: Color32, pub text_dim: Color32, pub muted: Color32,
    pub accent: Color32, pub ok: Color32, pub error: Color32,
}

pub const PAPER: Palette = Palette {
    bg: Color32::from_rgb(0xf6,0xf4,0xef), bg_rail: Color32::from_rgb(0xed,0xea,0xe2),
    bg_side: Color32::from_rgb(0xf1,0xee,0xe7), card: Color32::from_rgb(0xff,0xfd,0xf8),
    raised: Color32::from_rgb(0xf4,0xf0,0xe6), border: Color32::from_rgb(0xe1,0xdc,0xd1),
    border_soft: Color32::from_rgb(0xef,0xe9,0xdd), text: Color32::from_rgb(0x22,0x1f,0x1a),
    text_dim: Color32::from_rgb(0x4e,0x49,0x41), muted: Color32::from_rgb(0xa3,0x9b,0x8d),
    accent: Color32::from_rgb(0xd9,0x77,0x57), ok: Color32::from_rgb(0x7f,0xa0,0x7a),
    error: Color32::from_rgb(0xb4,0x59,0x3c),
};

pub const EMBER: Palette = Palette {
    bg: Color32::from_rgb(0x17,0x16,0x13), bg_rail: Color32::from_rgb(0x13,0x12,0x10),
    bg_side: Color32::from_rgb(0x1b,0x19,0x17), card: Color32::from_rgb(0x1e,0x1c,0x19),
    raised: Color32::from_rgb(0x22,0x1f,0x1c), border: Color32::from_rgb(0x2e,0x2b,0x26),
    border_soft: Color32::from_rgb(0x23,0x21,0x20), text: Color32::from_rgb(0xee,0xea,0xe1),
    text_dim: Color32::from_rgb(0xc9,0xc3,0xb7), muted: Color32::from_rgb(0x6d,0x66,0x5b),
    accent: Color32::from_rgb(0xd9,0x77,0x57), ok: Color32::from_rgb(0x8f,0xae,0x87),
    error: Color32::from_rgb(0xe0,0x8a,0x68),
};

static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn pal() -> Palette {
    if MODE.load(std::sync::atomic::Ordering::Relaxed) == 0 { PAPER } else { EMBER }
}

pub fn set_mode(ctx: &egui::Context, mode: ThemeMode) {
    MODE.store(if mode == ThemeMode::Paper { 0 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
    apply(ctx); // reaplica Visuals; egui repinta no próximo frame
}
```

Depois é find-and-replace mecânico: `crate::theme::TEXT` → `crate::theme::pal().text`, `BG_SIDE` → `pal().bg_side`, etc. Em `apply()`, monte os `Visuals` a partir de `pal()` (mantenha a estrutura que você já tem — só troque as constantes) e escolha `Visuals::light()`/`Visuals::dark()` conforme o modo.

Persistência: some `theme: ThemeMode` em `Config` (`config.rs`) e chame `set_mode` no `HarnessApp::new`. Atalho `⇧⌘D` junto do handler de `Ctrl+Enter` que já existe.

Raios/sombras do redesign: cards `12–14`, controles `8–9`, rail `9`. A `card_frame()` atual (radius 16, sombra 18/12%) fica em 14 e sombra `blur 22, alpha 18`.

---

## 3. Navegação: rail + painel único + ⌘K

### 3.1 Tirar a barra de abas de sessão
Remova o `TopBottomPanel::top("session_tabs")` inteiro. Os dados (`open_tabs`, `active_tab`, `switch_tab`, `close_tab`) **continuam** — só a lista da esquerda passa a ser a única superfície: item ativo destacado, ponto terracota quando `busy`, subtítulo mono com `chat_folder_name · última tool · tempo` (você já tem tudo em `daemon_live: Vec<SessionSummary>`).

Ordene em três grupos, filtrando a lista que já existe:

```rust
let (rodando, resto): (Vec<_>, Vec<_>) = self.daemon_live.iter().partition(|s| s.busy);
```

### 3.2 Rail de 60px
Um `SidePanel::left("rail").exact_width(60.0).resizable(false)` **antes** do painel de sessões, com um enum novo:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest { Chat, Files, Memory, Swarm, Diag, WebServer }
```

`RightTab` deixa de existir com 8 variantes: `Preview` e `Side` viram conteúdo que abre **dentro** do painel do chat (você já tem `self.preview` e `side_panel::get()`), e `Browser`+`Server` viram um destino só (`WebServer`) no pé do rail.

Cada item do rail é um botão de 48×~44 com quadrado/círculo/losango 11px + rótulo mono 8.5px, `frame(false)` e fundo `pal().card` + borda quando ativo — desenhe com `ui.allocate_response` + `painter().rect_filled` se quiser exatamente como no mock, ou aproxime com `SelectableLabel` e um `Frame`.

### 3.3 Status bar em vez da linha de status no topo
Troque a `ui.label(status_short)` do `TopBottomPanel::top("top")` por um `TopBottomPanel::bottom("statusbar").exact_height(26.0)`, mono 10.5, com: workspace · `● daemon 2/8` · swarm workers · RAM (o `self.mem_line` que você já monta) · `⌘K comandos`. O topo do chat fica só com título da sessão + toggle Code/Office + botão do painel.

### 3.4 ⌘K
Um `egui::Window` sem título, `anchor(Align2::CENTER_TOP, [0., 120.])`, `frame` com `card_frame()`, um `TextEdit` com foco forçado (`response.request_focus()`) e uma lista filtrada. Fonte das entradas: os `SlashAction` de `slash.rs` + comandos de Server/Web/tema/modelo + sessões + arquivos de `scan_artifacts`. Isso é o que permite aposentar as abas Server/Web/Diag sem perder função.

---

## 4. Tool calls expansíveis

Hoje `role == "tool"` renderiza uma linha mono truncada em 160 chars. Para expandir, o `UiMessage` precisa de estrutura:

```rust
struct ToolCall {
    name: String,        // "write_file"
    target: String,      // "src/lexer/token.rs"
    result: String,      // corpo completo / diff
    metric: String,      // "412 linhas", "+6 −4", "48 passed"
    state: ToolState,    // Running | Ok | Err | NeedsApproval
    open: bool,
}
```

Renderize com `egui::CollapsingHeader::new(...).id_salt(idx).show_unindented(...)` dentro de um `Frame` (`fill: pal().card`, borda `border_soft`, radius 10) — ou controle `open` você mesmo, que dá o cabeçalho exato do mock. Chamadas consecutivas do mesmo bloco entram no **mesmo** Frame com `Separator` de 1px entre linhas.

Estado `Running`: barra de 2px no rodapé da linha, largura oscilando com `ui.input(|i| i.time)` — você já pede `request_repaint_after(400ms)` quando `busy`.

Aprovação inline (`NeedsApproval`) substitui a `egui::Window::new("Approve tool")`: mesmos três botões chamando o `decide_approval` que já existe.

---

## 5. Composer e Settings

**Composer** (`TopBottomPanel::bottom("composer")`): largura do card 700 (hoje 640), radius 14, e a barra inferior vira chips com borda em vez de texto solto — `grok-4.5 ▾` (abre o pool do `Config::llm_pool`), `＋ arquivo`, `⌘⏎` em mono e o botão `Enviar` terracota. Altura sobe de 128 para ~140 para caber o chip row sem aperto.

**Settings**: a `Window` de hoje empilha tudo. Vire duas colunas dentro da mesma janela: `SidePanel::left` interno de 150px com as sete seções (Modelos e pool · Workspace · Aprovações · Memória · Swarm · Aparência · Atualizações) e um `enum SettingsSection` guardando a ativa. É recorte de UI, não mudança de lógica — cada bloco atual vira o corpo de um `match`.

**Setup**: um passo só (pasta + provedor + key), com os presets de provedor como três cartões clicáveis que preenchem `draft_api_base`. O botão testa a key antes de fechar (`provider_doctor.rs` já faz isso).

---

## Ordem de commits sugerida

1. `theme.rs`: fontes embutidas + `Palette`/`pal()` + escala (nada visual quebra)
2. find-and-replace das constantes → `pal()`, atalho de tema, persistir em `Config`
3. remover `session_tabs`, enriquecer a lista de sessões
4. rail + `Dest` + fundir Web/Server, mover Preview/Side para o painel do chat
5. status bar inferior + limpar o top bar
6. ⌘K
7. tool calls estruturados + aprovação inline
8. composer e Settings em seções

Os passos 1–5 dão o visual dos mocks `1a`/`1b`; 6–8 são o que muda de verdade a sensação de uso.
