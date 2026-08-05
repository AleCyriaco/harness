//! Ícone do app desenhado em código: quadrado terracota arredondado + H branco.
//!
//! O tom é o `#d97757` do Claude — o mesmo `accent` das paletas em `theme.rs`,
//! então ícone e UI usam a mesma cor de marca.
//!
//! Sem asset e sem decoder de PNG — a mesma função gera o ícone da janela em
//! runtime e os arquivos do bundle (.icns/.ico), então os dois nunca divergem.

/// Gradiente do fundo, de cima para baixo. A média dá exatamente #d97757.
const TOP: [f32; 3] = [0xe7 as f32, 0x89 as f32, 0x69 as f32];
const BOTTOM: [f32; 3] = [0xcb as f32, 0x65 as f32, 0x45 as f32];

/// Margem em volta do quadrado (padrão macOS: conteúdo ~80% da tela).
const MARGIN: f32 = 0.10;
/// Raio do canto, relativo ao lado do quadrado.
const CORNER: f32 = 0.225;

/// Geometria do H, relativa ao lado do quadrado.
const H_HEIGHT: f32 = 0.54;
const H_WIDTH: f32 = 0.46;
const STEM: f32 = 0.125;
const BAR: f32 = 0.115;

/// Amostras por eixo (antialias).
const SS: u32 = 4;

fn sd_round_box(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - hw + r;
    let qy = py.abs() - hh + r;
    let ax = qx.max(0.0);
    let ay = qy.max(0.0);
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

fn in_box(px: f32, py: f32, hw: f32, hh: f32) -> bool {
    px.abs() <= hw && py.abs() <= hh
}

/// RGBA não pré-multiplicado, `size`×`size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let n = size as f32;
    let side = n * (1.0 - 2.0 * MARGIN);
    let half = side * 0.5;
    let radius = side * CORNER;

    // H: duas hastes + travessa, centrados no quadrado.
    let h_half_h = side * H_HEIGHT * 0.5;
    let h_half_w = side * H_WIDTH * 0.5;
    let stem_half = side * STEM * 0.5;
    let bar_half = side * BAR * 0.5;
    let stem_cx = h_half_w - stem_half;

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let step = 1.0 / SS as f32;
    let samples = (SS * SS) as f32;

    for y in 0..size {
        // gradiente vertical, constante na linha
        let t = y as f32 / (n - 1.0).max(1.0);
        let bg = [
            TOP[0] + (BOTTOM[0] - TOP[0]) * t,
            TOP[1] + (BOTTOM[1] - TOP[1]) * t,
            TOP[2] + (BOTTOM[2] - TOP[2]) * t,
        ];
        for x in 0..size {
            let mut cover = 0.0_f32;
            let mut white = 0.0_f32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) * step - n * 0.5;
                    let py = y as f32 + (sy as f32 + 0.5) * step - n * 0.5;
                    if sd_round_box(px, py, half, half, radius) > 0.0 {
                        continue;
                    }
                    cover += 1.0;
                    let stems = in_box(px.abs() - stem_cx, py, stem_half, h_half_h);
                    let bar = in_box(px, py, h_half_w, bar_half);
                    if stems || bar {
                        white += 1.0;
                    }
                }
            }
            if cover == 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // dentro da área coberta, quanto é branco
            let w = white / cover;
            let mix = |c: f32| (c + (255.0 - c) * w).round().clamp(0.0, 255.0) as u8;
            out.extend_from_slice(&[
                mix(bg[0]),
                mix(bg[1]),
                mix(bg[2]),
                (cover / samples * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// Terracota chapado da marca (média do gradiente = #d97757) — para desenhar
/// em tamanhos pequenos na UI, onde gradiente não acrescenta nada.
pub fn mark_color() -> egui::Color32 {
    egui::Color32::from_rgb(
        ((TOP[0] + BOTTOM[0]) * 0.5) as u8,
        ((TOP[1] + BOTTOM[1]) * 0.5) as u8,
        ((TOP[2] + BOTTOM[2]) * 0.5) as u8,
    )
}

/// Desenha a marca do app (quadrado terracota + H branco) preenchendo `rect`.
/// Mesma geometria do ícone — uma marca só, em toda a UI.
pub fn paint_mark(painter: &egui::Painter, rect: egui::Rect) {
    let side = rect.width().min(rect.height());
    painter.rect_filled(
        rect,
        egui::CornerRadius::same((side * CORNER).round() as u8),
        mark_color(),
    );
    let c = rect.center();
    let hh = side * H_HEIGHT;
    let hw = side * H_WIDTH;
    let stem = side * STEM;
    let bar = side * BAR;
    let off = (hw - stem) * 0.5;
    let white = egui::Color32::WHITE;
    for dx in [-off, off] {
        painter.rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(c.x + dx, c.y),
                egui::vec2(stem, hh),
            ),
            0,
            white,
        );
    }
    painter.rect_filled(
        egui::Rect::from_center_size(c, egui::vec2(hw, bar)),
        0,
        white,
    );
}

/// Ícone da janela (dock no Linux/Windows; no macOS quem manda é o bundle).
pub fn window_icon() -> egui::IconData {
    const SIZE: u32 = 128;
    egui::IconData {
        rgba: rgba(SIZE),
        width: SIZE,
        height: SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marca tem que bater com o `accent` do tema (#d97757).
    #[test]
    fn mark_matches_theme_accent() {
        assert_eq!(mark_color(), crate::theme::PAPER.accent);
        assert_eq!(mark_color(), crate::theme::EMBER.accent);
    }

    #[test]
    fn shape_is_sane() {
        let size = 64;
        let px = rgba(size);
        assert_eq!(px.len(), (size * size * 4) as usize);
        let at = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            (px[i], px[i + 1], px[i + 2], px[i + 3])
        };
        // canto: fora do quadrado arredondado → transparente
        assert_eq!(at(0, 0).3, 0);
        // centro: travessa do H → branco opaco
        let c = at(size / 2, size / 2);
        assert_eq!((c.0, c.1, c.2, c.3), (255, 255, 255, 255));
        // entre a haste e a borda: terracota opaco
        let b = at(size / 2, size / 6);
        assert_eq!(b.3, 255);
        assert!(b.0 > b.1 && b.1 > b.2, "fundo deve ser terracota, veio {b:?}");
    }

    /// Gera os RGBA crus que viram .icns/.ico. Rode com:
    /// `cargo test -- --ignored dump_icon`
    #[test]
    #[ignore]
    fn dump_icon() {
        let dir = std::path::Path::new("target/icon-raw");
        std::fs::create_dir_all(dir).unwrap();
        for size in [16u32, 32, 64, 128, 256, 512, 1024] {
            std::fs::write(dir.join(format!("{size}.rgba")), rgba(size)).unwrap();
        }
        eprintln!("wrote {}", dir.display());
    }
}
