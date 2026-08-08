//! Design tokens: fontes IBM Plex embutidas, paletas Paper/Ember e frames comuns.
//!
//! As cores não são mais `const` soltas: `pal()` devolve a paleta ativa, então
//! trocar de tema (⇧⌘D / ⌘K) é só `set_mode` + repaint.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke,
    Style, TextStyle, Visuals,
};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Paper,
    Ember,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Paper => "light (Paper)",
            ThemeMode::Ember => "dark (Ember)",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            ThemeMode::Paper => ThemeMode::Ember,
            ThemeMode::Ember => ThemeMode::Paper,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub bg_rail: Color32,
    pub bg_side: Color32,
    pub card: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub border_soft: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub ok: Color32,
    pub error: Color32,
    /// Bolha da mensagem do usuário.
    pub user_bg: Color32,
    /// Fundo de bloco de código / trecho mono inline.
    pub code_bg: Color32,
    pub diff_add_bg: Color32,
    pub diff_add_fg: Color32,
    pub diff_del_bg: Color32,
    pub diff_del_fg: Color32,
    /// Realce de sintaxe nos blocos de código do chat.
    pub syn_kw: Color32,
    pub syn_str: Color32,
    pub syn_com: Color32,
    pub syn_num: Color32,
}

pub const PAPER: Palette = Palette {
    bg: Color32::from_rgb(0xf6, 0xf4, 0xef),
    bg_rail: Color32::from_rgb(0xed, 0xea, 0xe2),
    bg_side: Color32::from_rgb(0xf1, 0xee, 0xe7),
    card: Color32::from_rgb(0xff, 0xfd, 0xf8),
    raised: Color32::from_rgb(0xf4, 0xf0, 0xe6),
    border: Color32::from_rgb(0xe1, 0xdc, 0xd1),
    border_soft: Color32::from_rgb(0xef, 0xe9, 0xdd),
    text: Color32::from_rgb(0x22, 0x1f, 0x1a),
    text_dim: Color32::from_rgb(0x4e, 0x49, 0x41),
    muted: Color32::from_rgb(0xa3, 0x9b, 0x8d),
    accent: Color32::from_rgb(0xd9, 0x77, 0x57),
    ok: Color32::from_rgb(0x7f, 0xa0, 0x7a),
    error: Color32::from_rgb(0xb4, 0x59, 0x3c),
    user_bg: Color32::from_rgb(0xee, 0xea, 0xe0),
    code_bg: Color32::from_rgb(0xef, 0xec, 0xe3),
    diff_add_bg: Color32::from_rgb(0xee, 0xf3, 0xec),
    diff_add_fg: Color32::from_rgb(0x4a, 0x77, 0x42),
    diff_del_bg: Color32::from_rgb(0xf7, 0xec, 0xe7),
    diff_del_fg: Color32::from_rgb(0xb4, 0x59, 0x3c),
    syn_kw: Color32::from_rgb(0x8f, 0x53, 0x8c),
    syn_str: Color32::from_rgb(0x4a, 0x77, 0x42),
    syn_com: Color32::from_rgb(0xa3, 0x9b, 0x8d),
    syn_num: Color32::from_rgb(0xa2, 0x60, 0x4b),
};

pub const EMBER: Palette = Palette {
    bg: Color32::from_rgb(0x17, 0x16, 0x13),
    bg_rail: Color32::from_rgb(0x13, 0x12, 0x10),
    bg_side: Color32::from_rgb(0x1b, 0x19, 0x17),
    card: Color32::from_rgb(0x1e, 0x1c, 0x19),
    raised: Color32::from_rgb(0x22, 0x1f, 0x1c),
    border: Color32::from_rgb(0x2e, 0x2b, 0x26),
    border_soft: Color32::from_rgb(0x23, 0x21, 0x20),
    text: Color32::from_rgb(0xee, 0xea, 0xe1),
    text_dim: Color32::from_rgb(0xc9, 0xc3, 0xb7),
    muted: Color32::from_rgb(0x6d, 0x66, 0x5b),
    accent: Color32::from_rgb(0xd9, 0x77, 0x57),
    ok: Color32::from_rgb(0x8f, 0xae, 0x87),
    error: Color32::from_rgb(0xe0, 0x8a, 0x68),
    user_bg: Color32::from_rgb(0x24, 0x21, 0x1e),
    code_bg: Color32::from_rgb(0x22, 0x1f, 0x1c),
    diff_add_bg: Color32::from_rgb(0x1c, 0x26, 0x1b),
    diff_add_fg: Color32::from_rgb(0x8f, 0xae, 0x87),
    diff_del_bg: Color32::from_rgb(0x2a, 0x1c, 0x18),
    diff_del_fg: Color32::from_rgb(0xe0, 0x8a, 0x68),
    syn_kw: Color32::from_rgb(0xc9, 0x8b, 0xc6),
    syn_str: Color32::from_rgb(0x8f, 0xae, 0x87),
    syn_com: Color32::from_rgb(0x6d, 0x66, 0x5b),
    syn_num: Color32::from_rgb(0xe0, 0xa0, 0x80),
};

/// 0 = Paper, 1 = Ember. Global leve para `pal()` ser chamável de qualquer widget.
static MODE: AtomicU8 = AtomicU8::new(0);

pub fn mode() -> ThemeMode {
    if MODE.load(Ordering::Relaxed) == 0 {
        ThemeMode::Paper
    } else {
        ThemeMode::Ember
    }
}

pub fn pal() -> Palette {
    if MODE.load(Ordering::Relaxed) == 0 {
        PAPER
    } else {
        EMBER
    }
}

pub fn set_mode(ctx: &egui::Context, m: ThemeMode) {
    MODE.store(
        if m == ThemeMode::Paper { 0 } else { 1 },
        Ordering::Relaxed,
    );
    apply(ctx);
    ctx.request_repaint();
}

/// Rótulo micro (RODANDO / HOJE / CHAT) — mono 9.5 caixa alta.
pub fn micro(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .monospace()
        .size(9.5)
        .color(pal().muted)
}

/// Meta em mono 10.5 (RAM, daemon, tempos, subtítulo de sessão).
pub fn meta(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .monospace()
        .size(10.5)
        .color(pal().muted)
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut style = Style {
        visuals: visuals(),
        ..(*ctx.style()).clone()
    };

    use FontFamily::{Monospace, Proportional};
    style.text_styles = [
        // meta: RAM, daemon, tempos
        (TextStyle::Small, FontId::new(10.5, Monospace)),
        // chat
        (TextStyle::Body, FontId::new(14.5, Proportional)),
        (TextStyle::Button, FontId::new(12.5, Proportional)),
        (TextStyle::Heading, FontId::new(20.0, Proportional)),
        // código e caminhos
        (TextStyle::Monospace, FontId::new(12.5, Monospace)),
    ]
    .into();

    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 6.0);
    style.spacing.indent = 14.0;
    // Selecionar texto com o mouse + ⌘C para copiar qualquer trecho do chat.
    style.interaction.selectable_labels = true;

    ctx.set_style(style);
}

fn visuals() -> Visuals {
    let p = pal();
    let dark = mode() == ThemeMode::Ember;
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.bg_side;
    v.faint_bg_color = p.bg_side;
    v.code_bg_color = p.code_bg;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = Color32::from_rgb(180, 120, 40);
    v.error_fg_color = p.error;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 18 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 80 } else { 14 }),
    };

    // Controles: raio 8–9 (cards ficam em 12–14 via card_frame)
    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
        w.fg_stroke = Stroke::new(1.0, p.text);
    }

    v.widgets.noninteractive.bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.noninteractive.bg_stroke = Stroke::new(0.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.muted);

    v.widgets.inactive.bg_fill = p.bg_side;
    v.widgets.inactive.weak_bg_fill = p.bg_side;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);

    v.widgets.hovered.bg_fill = p.raised;
    v.widgets.hovered.weak_bg_fill = p.raised;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border);

    v.widgets.active.bg_fill = p.raised;
    v.widgets.active.weak_bg_fill = p.raised;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);

    v.widgets.open.bg_fill = p.raised;
    v.widgets.open.weak_bg_fill = p.raised;

    v.selection.bg_fill = p.accent.gamma_multiply(if dark { 0.45 } else { 0.3 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v
}

/// IBM Plex Sans/Mono embutidos no binário — mesma renderização em Win/Mac/Linux.
///
/// Mantemos as famílias padrão do egui **depois** das nossas: elas cobrem os
/// símbolos/emoji que o Plex não tem (setas, ✓, emoji vindo do LLM).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "ui".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "ui_medium".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")).into(),
    );
    fonts.font_data.insert(
        "ui_semibold".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")).into(),
    );
    fonts.font_data.insert(
        "mono".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "mono_medium".into(),
        FontData::from_static(include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf")).into(),
    );

    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "ui".into());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "mono".into());

    // Famílias extras para peso (rótulos do rail, títulos de sessão).
    // Cada uma herda a cadeia de fallback da família base, senão símbolos
    // fora do Plex (setas, ✓, emoji) viram tofu.
    let prop_fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mono_fallback = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let with = |first: &[&str], rest: &[String]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().map(|s| s.to_string()).collect();
        for f in rest {
            if !v.contains(f) {
                v.push(f.clone());
            }
        }
        v
    };
    fonts.families.insert(
        FontFamily::Name("ui_medium".into()),
        with(&["ui_medium"], &prop_fallback),
    );
    fonts.families.insert(
        FontFamily::Name("ui_semibold".into()),
        with(&["ui_semibold", "ui_medium"], &prop_fallback),
    );
    fonts.families.insert(
        FontFamily::Name("mono_medium".into()),
        with(&["mono_medium"], &mono_fallback),
    );

    ctx.set_fonts(fonts);
}

pub fn ui_medium(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text.into())
        .family(FontFamily::Name("ui_medium".into()))
        .size(size)
}

pub fn mono_medium(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text.into())
        .family(FontFamily::Name("mono_medium".into()))
        .size(size)
}

/// Card do redesign: raio 14, sombra blur 22 / alpha 18.
pub fn card_frame() -> egui::Frame {
    let p = pal();
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(14))
        .shadow(Shadow {
            offset: [0, 6],
            blur: 22,
            spread: 0,
            color: Color32::from_black_alpha(if mode() == ThemeMode::Ember { 90 } else { 18 }),
        })
}

/// Bloco de tool calls: raio 10, borda suave, sem sombra.
pub fn tool_frame() -> egui::Frame {
    let p = pal();
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border_soft))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::ZERO)
}
