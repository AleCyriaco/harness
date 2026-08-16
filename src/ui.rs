//! Primitivas visuais do redesign: rail, chips, chevrons, pontos de estado.
//!
//! Tudo desenhado com o painter (nada de glifos exóticos que faltam na fonte).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Vec2,
};

use crate::theme::pal;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Square,
    Circle,
    Diamond,
    Bar,
    Globe,
    /// três nós ligados — grafo estrutural
    Nodes,
    /// pulso — painel de uso
    Pulse,
    /// seta circular — repetir até terminar (loop)
    Loop,
    /// cadeado aberto — o portão de aprovação foi liberado (trust)
    Unlock,
}

fn paint_glyph(painter: &egui::Painter, center: Pos2, g: Glyph, filled: bool, color: Color32) {
    let s = 15.0_f32;
    let half = s * 0.5;
    let rect = Rect::from_center_size(center, Vec2::splat(s));
    let stroke = Stroke::new(1.7, color);
    match g {
        Glyph::Square => {
            if filled {
                painter.rect_filled(rect, CornerRadius::same(3), color);
            } else {
                painter.rect_stroke(rect, CornerRadius::same(3), stroke, StrokeKind::Inside);
            }
        }
        Glyph::Circle => {
            if filled {
                painter.circle_filled(center, half, color);
            } else {
                painter.circle_stroke(center, half - 0.75, stroke);
            }
        }
        Glyph::Diamond => {
            let pts = vec![
                Pos2::new(center.x, center.y - half),
                Pos2::new(center.x + half, center.y),
                Pos2::new(center.x, center.y + half),
                Pos2::new(center.x - half, center.y),
            ];
            if filled {
                painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
            } else {
                painter.add(egui::Shape::closed_line(pts, stroke));
            }
        }
        Glyph::Bar => {
            let r = Rect::from_center_size(center, Vec2::new(s, 2.6));
            painter.rect_filled(r, CornerRadius::same(1), color);
        }
        Glyph::Nodes => {
            let r = 2.7;
            let a = Pos2::new(center.x - half + r, center.y + half - r);
            let b = Pos2::new(center.x + half - r, center.y + half - r);
            let top = Pos2::new(center.x, center.y - half + r);
            let thin = Stroke::new(1.4, color);
            painter.line_segment([a, top], thin);
            painter.line_segment([b, top], thin);
            painter.line_segment([a, b], thin);
            for p in [a, b, top] {
                if filled {
                    painter.circle_filled(p, r, color);
                } else {
                    painter.circle_stroke(p, r - 0.4, thin);
                }
            }
        }
        Glyph::Pulse => {
            let thin = Stroke::new(1.6, color);
            let y = center.y;
            let pts = vec![
                Pos2::new(center.x - half, y),
                Pos2::new(center.x - half * 0.45, y),
                Pos2::new(center.x - half * 0.2, y - half * 0.85),
                Pos2::new(center.x + half * 0.1, y + half * 0.7),
                Pos2::new(center.x + half * 0.4, y),
                Pos2::new(center.x + half, y),
            ];
            painter.add(egui::Shape::line(pts, thin));
        }
        Glyph::Loop => {
            // arco de ~300° + ponta de seta: "repete"
            let rr = half - 0.6;
            let mut pts = Vec::new();
            let start = -0.35f32;
            let sweep = std::f32::consts::TAU * 0.82;
            let steps = 22;
            for i in 0..=steps {
                let a = start + sweep * (i as f32 / steps as f32);
                pts.push(Pos2::new(center.x + a.cos() * rr, center.y + a.sin() * rr));
            }
            painter.add(egui::Shape::line(pts.clone(), stroke));
            if let Some(tip) = pts.last() {
                let a = start + sweep;
                // seta tangente ao arco
                let tx = -a.sin();
                let ty = a.cos();
                let back = Pos2::new(tip.x - tx * 3.4, tip.y - ty * 3.4);
                let nx = a.cos();
                let ny = a.sin();
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        *tip,
                        Pos2::new(back.x + nx * 2.4, back.y + ny * 2.4),
                        Pos2::new(back.x - nx * 2.4, back.y - ny * 2.4),
                    ],
                    color,
                    Stroke::NONE,
                ));
            }
        }
        Glyph::Unlock => {
            // corpo do cadeado
            let body = Rect::from_center_size(
                Pos2::new(center.x, center.y + half * 0.35),
                Vec2::new(s * 0.78, s * 0.55),
            );
            if filled {
                painter.rect_filled(body, CornerRadius::same(2), color);
            } else {
                painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            }
            // haste aberta: arco que não fecha, deslocado para a direita
            let hr = s * 0.28;
            let hc = Pos2::new(center.x + hr * 0.55, body.top() - hr * 0.55);
            let mut pts = Vec::new();
            for i in 0..=14 {
                let a = std::f32::consts::PI * (0.05 + 0.95 * (i as f32 / 14.0));
                pts.push(Pos2::new(hc.x - a.cos() * hr, hc.y - a.sin() * hr));
            }
            painter.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
        }
        Glyph::Globe => {
            painter.circle_stroke(center, half - 0.75, stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x - half + 0.75, center.y),
                    Pos2::new(center.x + half - 0.75, center.y),
                ],
                stroke,
            );
            painter.add(egui::Shape::closed_line(
                vec![
                    Pos2::new(center.x, center.y - half + 0.75),
                    Pos2::new(center.x + half * 0.55, center.y),
                    Pos2::new(center.x, center.y + half - 0.75),
                    Pos2::new(center.x - half * 0.55, center.y),
                ],
                stroke,
            ));
        }
    }
}

/// Item do rail: 54×56, forma 15px + rótulo mono 10.
pub fn rail_item(
    ui: &mut egui::Ui,
    glyph: Glyph,
    label: &str,
    active: bool,
    dot: bool,
) -> Response {
    let p = pal();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(54.0, 56.0), Sense::click());
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, CornerRadius::same(9), p.card);
        painter.rect_stroke(
            rect,
            CornerRadius::same(9),
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(9), p.raised);
    }

    let fg = if active { p.text } else { p.muted };
    let shape_color = if active { p.accent } else { p.muted };
    paint_glyph(
        painter,
        Pos2::new(rect.center().x, rect.top() + 18.0),
        glyph,
        active,
        shape_color,
    );
    painter.text(
        Pos2::new(rect.center().x, rect.bottom() - 13.0),
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(10.0),
        fg,
    );
    if dot {
        painter.circle_filled(
            Pos2::new(rect.right() - 8.0, rect.top() + 7.0),
            3.4,
            p.accent,
        );
    }
    response
}

/// Ponto de estado 5–6px alinhado ao texto.
pub fn dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(radius * 2.0 + 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

/// Chevron ▸ / ▾ desenhado (sem depender de glifo).
pub fn chevron(ui: &mut egui::Ui, open: bool, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 12.0), Sense::hover());
    let c = rect.center();
    let pts = if open {
        vec![
            Pos2::new(c.x - 4.0, c.y - 2.0),
            Pos2::new(c.x + 4.0, c.y - 2.0),
            Pos2::new(c.x, c.y + 3.0),
        ]
    } else {
        vec![
            Pos2::new(c.x - 2.0, c.y - 4.0),
            Pos2::new(c.x + 3.0, c.y),
            Pos2::new(c.x - 2.0, c.y + 4.0),
        ]
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// Chip com borda (composer, filtros).
pub fn chip(ui: &mut egui::Ui, text: &str) -> Response {
    let p = pal();
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .monospace()
                .size(11.5)
                .color(p.text_dim),
        )
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, p.border_soft))
        .corner_radius(CornerRadius::same(7))
        .min_size(Vec2::new(0.0, 24.0)),
    )
}

/// `chip` que sabe estar ligado: acende em terracota quando `on`.
/// Usada pelos toggles do composer (Token Less Cost, Gauntlet Loop).
pub fn pill_toggle(ui: &mut egui::Ui, text: &str, on: bool) -> Response {
    pill_toggle_icon(ui, text, on, None)
}

/// Pill com glifo desenhado à esquerda do texto. Desenhado, não fonte: a
/// IBM Plex embutida não traz ↻ nem cadeado, e símbolo ausente vira tofu.
pub fn pill_toggle_icon(
    ui: &mut egui::Ui,
    text: &str,
    on: bool,
    glyph: Option<Glyph>,
) -> Response {
    let p = pal();
    let color = if on { p.accent } else { p.muted };
    let stroke = Stroke::new(1.0, if on { p.accent } else { p.border_soft });
    let mut btn = egui::Button::new(
        egui::RichText::new(if glyph.is_some() {
            format!("   {text}")
        } else {
            text.to_string()
        })
        .monospace()
        .size(11.5)
        .color(color),
    )
    .fill(Color32::TRANSPARENT)
    .stroke(stroke)
    .corner_radius(CornerRadius::same(7))
    .min_size(Vec2::new(0.0, 24.0));
    if glyph.is_some() {
        btn = btn.min_size(Vec2::new(0.0, 24.0));
    }
    let resp = ui.add(btn);
    if let Some(g) = glyph {
        let c = Pos2::new(resp.rect.left() + 12.0, resp.rect.center().y);
        paint_glyph(ui.painter(), c, g, false, color);
    }
    resp
}

/// Nó de um grafo desenhável (Graph, Mem, Live usam o mesmo).
pub struct GNode {
    pub id: String,
    pub label: String,
    pub r: f32,
    pub color: Color32,
    pub dim: bool,
}

/// Desenha um grafo em círculo, arrastável e clicável.
/// Devolve o id clicado, se houve clique.
pub fn graph_canvas(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    nodes: &[GNode],
    edges: &[(usize, usize)],
    pos: &mut std::collections::HashMap<String, Pos2>,
    selected: &Option<String>,
) -> Option<String> {
    let p = pal();
    // layout padrão: anel, com os maiores primeiro (já vêm ordenados)
    let c = rect.center();
    let radius = (rect.width().min(rect.height()) * 0.40).max(60.0);
    let n = nodes.len().max(1) as f32;
    for (i, node) in nodes.iter().enumerate() {
        let ang = (i as f32) * std::f32::consts::TAU / n - std::f32::consts::FRAC_PI_2;
        // dois anéis quando há muitos, para não virar colar apertado
        let rr = if nodes.len() > 18 && i % 2 == 1 { radius * 0.62 } else { radius };
        let d = c + egui::vec2(ang.cos(), ang.sin()) * rr;
        pos.entry(node.id.clone()).or_insert(d);
    }
    let painter = ui.painter_at(rect);
    for (a, b) in edges {
        let (Some(na), Some(nb)) = (nodes.get(*a), nodes.get(*b)) else {
            continue;
        };
        let (Some(pa), Some(pb)) = (pos.get(&na.id), pos.get(&nb.id)) else {
            continue;
        };
        let lit = selected.as_deref() == Some(na.id.as_str())
            || selected.as_deref() == Some(nb.id.as_str());
        painter.line_segment(
            [*pa, *pb],
            Stroke::new(if lit { 1.8 } else { 0.8 }, if lit { p.accent } else { p.border_soft }),
        );
    }
    let mut clicked = None;
    for (i, node) in nodes.iter().enumerate() {
        let mut at = pos[&node.id];
        let hit = egui::Rect::from_center_size(at, Vec2::splat(node.r * 2.0 + 8.0));
        let resp = ui.interact(hit, egui::Id::new(("gnode", i, &node.id)), egui::Sense::click_and_drag());
        if resp.dragged() {
            at += resp.drag_delta();
            pos.insert(node.id.clone(), at);
        }
        if resp.clicked() {
            clicked = Some(node.id.clone());
        }
        let sel = selected.as_deref() == Some(node.id.as_str());
        if sel {
            painter.circle_stroke(at, node.r + 5.0, Stroke::new(2.0, p.accent));
        }
        let col = if node.dim { p.muted } else { node.color };
        painter.circle(at, node.r, col.gamma_multiply(0.16), Stroke::new(1.8, col));
        painter.text(
            Pos2::new(at.x, at.y + node.r + 9.0),
            Align2::CENTER_CENTER,
            &node.label,
            FontId::monospace(if node.r < 11.0 { 9.5 } else { 11.0 }),
            if sel { p.text } else { p.text_dim },
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    clicked
}

/// Estado vazio que ensina: o que este painel é, e o que fazer agora.
/// Devolve `true` quando o usuário clica na ação.
pub fn empty_state(ui: &mut egui::Ui, what: &str, why: &str, action: Option<&str>) -> bool {
    let p = pal();
    let mut clicked = false;
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        ui.label(crate::theme::ui_medium(what, 13.5).color(p.text_dim));
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(why)
                .size(12.0)
                .color(p.muted),
        );
        if let Some(a) = action {
            ui.add_space(10.0);
            clicked = primary_button(ui, a, true).clicked();
        }
    });
    clicked
}

/// Botão terracota principal (Enviar, Salvar).
pub fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> Response {
    let p = pal();
    ui.add_enabled(
        enabled,
        egui::Button::new(
            crate::theme::ui_medium(text, 12.5).color(if p.bg.r() > 128 {
                Color32::WHITE
            } else {
                p.bg
            }),
        )
        .fill(p.accent)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(76.0, 30.0)),
    )
}

/// Botão de ação destrutiva (apagar).
pub fn danger_button(ui: &mut egui::Ui, text: &str) -> Response {
    let p = pal();
    ui.add(
        egui::Button::new(crate::theme::ui_medium(text, 12.5).color(Color32::WHITE))
            .fill(p.error)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(76.0, 30.0)),
    )
}

/// Toggle segmentado (Code / Office, Preview / Side).
pub fn segmented(ui: &mut egui::Ui, options: &[&str], selected: usize) -> Option<usize> {
    let p = pal();
    let mut clicked = None;
    egui::Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // `horizontal` herda a direção do pai; num layout right_to_left as
            // opções sairiam invertidas, então percorremos ao contrário.
            let rtl = ui.layout().main_dir == egui::Direction::RightToLeft;
            ui.horizontal(|ui| {
                let order: Vec<usize> = if rtl {
                    (0..options.len()).rev().collect()
                } else {
                    (0..options.len()).collect()
                };
                for i in order {
                    let opt = &options[i];
                    let on = i == selected;
                    let btn = egui::Button::new(if on {
                        crate::theme::ui_medium(*opt, 11.5).color(p.text)
                    } else {
                        egui::RichText::new(*opt).size(11.5).color(p.muted)
                    })
                    .fill(if on { p.card } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(6))
                    .min_size(Vec2::new(0.0, 22.0));
                    if ui.add(btn).clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
    clicked
}

/// Gráfico de linha de uma série, com área discreta e escala automática.
/// Uma série por gráfico: escalas separadas impedem que a curva menor
/// vire uma linha reta colada no chão.
pub fn spark_chart(ui: &mut egui::Ui, height: f32, series: &[f32], color: Color32) {
    let p = pal();
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(8), p.raised);
    if series.len() < 2 {
        return;
    }

    let peak = series.iter().copied().fold(0.0_f32, f32::max).max(1.0);
    let pad = 4.0;
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + pad, rect.top() + pad),
        Pos2::new(rect.right() - pad, rect.bottom() - pad),
    );
    let base = inner.bottom();
    let step = inner.width() / (series.len() - 1) as f32;
    let pts: Vec<Pos2> = series
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = inner.left() + i as f32 * step;
            let y = base - (v / peak).clamp(0.0, 1.0) * inner.height();
            Pos2::new(x, y)
        })
        .collect();

    // Área por trapézios: um polígono só seria côncavo e o `convex_polygon`
    // do egui devolveria cunhas atravessando o gráfico.
    let soft = color.gamma_multiply(0.16);
    for pair in pts.windows(2) {
        painter.add(egui::Shape::convex_polygon(
            vec![
                pair[0],
                pair[1],
                Pos2::new(pair[1].x, base),
                Pos2::new(pair[0].x, base),
            ],
            soft,
            Stroke::NONE,
        ));
    }
    let tip = pts.last().copied();
    painter.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
    // ponta viva
    if let Some(last) = tip {
        painter.circle_filled(last, 2.0, color);
    }
}

/// Barra proporcional de duas partes (entrada/saída, hit/miss).
pub fn split_bar(ui: &mut egui::Ui, frac_a: f32, color_a: Color32, color_b: Color32) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 6.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), color_b);
    let cut = rect.width() * frac_a.clamp(0.0, 1.0);
    let a = Rect::from_min_size(rect.min, Vec2::new(cut, rect.height()));
    painter.rect_filled(a, CornerRadius::same(3), color_a);
}

/// Linha de 1px na cor `border_soft` (separador dentro de cards).
pub fn hairline(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, pal().border_soft);
}

/// Barra indeterminada de 2px (tool call rodando).
pub fn indeterminate_bar(ui: &mut egui::Ui, time: f64) {
    let p = pal();
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 2.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, p.border_soft);
    let frac = 0.32_f32;
    let travel = (1.0 - frac) * rect.width();
    // vai-e-volta suave sem depender de easing externo
    let t = ((time * 0.6).sin() * 0.5 + 0.5) as f32;
    let x = rect.left() + travel * t;
    let bar = Rect::from_min_size(
        Pos2::new(x, rect.top()),
        Vec2::new(rect.width() * frac, 2.0),
    );
    ui.painter().rect_filled(bar, 0, p.accent);
}
