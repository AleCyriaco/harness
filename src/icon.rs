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

/// Amostras por eixo (antialias).
const SS: u32 = 4;

fn sd_round_box(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - hw + r;
    let qy = py.abs() - hh + r;
    let ax = qx.max(0.0);
    let ay = qy.max(0.0);
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

/// Traço do "h" cursivo, relativo ao lado do quadrado.
const STROKE_W: f32 = 0.088;
/// Raios do fundo: só aparecem a partir deste tamanho, senão viram borrão.
const RAYS_MIN_PX: u32 = 64;
const RAY_COUNT: usize = 44;

/// Pontos-guia do "h" manuscrito, em coordenadas -0.5..0.5 (y cresce para baixo).
/// Dois traços: entrada+laço+haste, e o ombro+perna com o gancho final.
const H_GUIDES: [&[[f32; 2]]; 2] = [
    // Tique curto de entrada subindo, laço alto e aberto, e o lado descendente
    // do laço virando a haste — é a haste contínua que faz ler "h" e não "R".
    &[
        [-0.26, -0.05],
        [-0.19, -0.17],
        [-0.11, -0.33],
        [-0.01, -0.44],
        [0.09, -0.36],
        [0.10, -0.19],
        [0.02, -0.05],
        [-0.02, 0.16],
        [-0.04, 0.43],
    ],
    // ombro saindo da haste na altura-x, perna direita e gancho final
    &[
        [-0.03, 0.11],
        [0.05, -0.02],
        [0.17, -0.03],
        [0.24, 0.10],
        [0.25, 0.29],
        [0.31, 0.42],
        [0.40, 0.34],
    ],
];

/// Catmull-Rom: passa pelos pontos-guia, então ajustar a curva é mexer neles.
fn smooth(guides: &[[f32; 2]], per_seg: usize) -> Vec<[f32; 2]> {
    let n = guides.len();
    let at = |i: isize| guides[i.clamp(0, n as isize - 1) as usize];
    let mut out = Vec::with_capacity(n * per_seg);
    for i in 0..n.saturating_sub(1) {
        let (p0, p1, p2, p3) = (
            at(i as isize - 1),
            at(i as isize),
            at(i as isize + 1),
            at(i as isize + 2),
        );
        for s in 0..per_seg {
            let t = s as f32 / per_seg as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let mut p = [0.0f32; 2];
            for k in 0..2 {
                p[k] = 0.5
                    * ((2.0 * p1[k])
                        + (-p0[k] + p2[k]) * t
                        + (2.0 * p0[k] - 5.0 * p1[k] + 4.0 * p2[k] - p3[k]) * t2
                        + (-p0[k] + 3.0 * p1[k] - 3.0 * p2[k] + p3[k]) * t3);
            }
            out.push(p);
        }
    }
    out.push(guides[n - 1]);
    out
}

fn strokes() -> Vec<Vec<[f32; 2]>> {
    H_GUIDES.iter().map(|g| smooth(g, 12)).collect()
}

/// Distância ao segmento — o traço é "tudo a menos de meia largura da curva".
fn dist_seg(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
    let (wx, wy) = (px - a[0], py - a[1]);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= 1e-9 {
        0.0
    } else {
        ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0)
    };
    let (dx, dy) = (wx - vx * t, wy - vy * t);
    (dx * dx + dy * dy).sqrt()
}

fn dist_to_h(px: f32, py: f32, paths: &[Vec<[f32; 2]>]) -> f32 {
    let mut best = f32::MAX;
    for path in paths {
        for pair in path.windows(2) {
            best = best.min(dist_seg(px, py, pair[0], pair[1]));
        }
    }
    best
}

/// RGBA não pré-multiplicado, `size`×`size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let n = size as f32;
    let side = n * (1.0 - 2.0 * MARGIN);
    let half = side * 0.5;
    let radius = side * CORNER;
    let paths = strokes();
    // Abaixo de 32px o traço fino some no downsample: engrossa para o ícone
    // continuar legível no Finder e na barra de menus.
    let weight = if size < 32 { 1.35 } else if size < 64 { 1.15 } else { 1.0 };
    let half_stroke = side * STROKE_W * weight * 0.5;
    let rays = size >= RAYS_MIN_PX;

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let step = 1.0 / SS as f32;
    let samples = (SS * SS) as f32;

    for y in 0..size {
        let t = y as f32 / (n - 1.0).max(1.0);
        let bg = [
            TOP[0] + (BOTTOM[0] - TOP[0]) * t,
            TOP[1] + (BOTTOM[1] - TOP[1]) * t,
            TOP[2] + (BOTTOM[2] - TOP[2]) * t,
        ];
        for x in 0..size {
            let mut cover = 0.0_f32;
            let mut white = 0.0_f32;
            let mut ray = 0.0_f32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) * step - n * 0.5;
                    let py = y as f32 + (sy as f32 + 0.5) * step - n * 0.5;
                    if sd_round_box(px, py, half, half, radius) > 0.0 {
                        continue;
                    }
                    cover += 1.0;
                    // traço do h em coordenadas normalizadas
                    if dist_to_h(px / side, py / side, &paths) * side <= half_stroke {
                        white += 1.0;
                    } else if rays && in_ray(px, py, side) {
                        ray += 1.0;
                    }
                }
            }
            if cover == 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let w = white / cover;
            let r = ray / cover;
            // raios: clareiam o fundo de leve, atrás da letra
            let mix = |c: f32| {
                let base = c + (255.0 - c) * 0.22 * r;
                (base + (255.0 - base) * w).round().clamp(0.0, 255.0) as u8
            };
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

/// Raios irradiando do centro, com comprimentos alternados como na referência.
fn in_ray(px: f32, py: f32, side: f32) -> bool {
    let r = (px * px + py * py).sqrt() / side;
    if !(0.20..0.52).contains(&r) {
        return false;
    }
    let ang = py.atan2(px);
    let slot = (ang + std::f32::consts::PI) / (std::f32::consts::TAU) * RAY_COUNT as f32;
    let idx = slot.floor() as usize % RAY_COUNT;
    let frac = slot.fract();
    // metade da fatia é raio, metade é vão
    if !(0.30..0.70).contains(&frac) {
        return false;
    }
    // comprimentos variados: uns começam mais longe, outros terminam antes
    let start = 0.20 + 0.10 * (((idx * 7) % 5) as f32 / 4.0);
    let end = 0.40 + 0.12 * (((idx * 3) % 4) as f32 / 3.0);
    (start..end).contains(&r)
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
    // Sem raios aqui: na UI a marca vive entre 20 e 34px, onde eles borrariam.
    let stroke = egui::Stroke::new(side * STROKE_W, egui::Color32::WHITE);
    for path in strokes() {
        let pts: Vec<egui::Pos2> = path
            .iter()
            .map(|p| egui::pos2(c.x + p[0] * side, c.y + p[1] * side))
            .collect();
        painter.add(egui::Shape::line(pts, stroke));
    }
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

    /// Afirma a intenção, não coordenadas: mexer nos pontos-guia da letra não
    /// deve quebrar o teste, mas perder a letra ou o fundo deve.
    #[test]
    fn shape_is_sane() {
        let size = 128;
        let px = rgba(size);
        assert_eq!(px.len(), (size * size * 4) as usize);
        let at = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            (px[i], px[i + 1], px[i + 2], px[i + 3])
        };
        // canto: fora do quadrado arredondado
        assert_eq!(at(0, 0).3, 0, "canto deve ser transparente");

        let (mut opaque, mut white, mut terracota) = (0u32, 0u32, 0u32);
        for y in 0..size {
            for x in 0..size {
                let (r, g, b, a) = at(x, y);
                if a < 250 {
                    continue;
                }
                opaque += 1;
                if r > 245 && g > 245 && b > 245 {
                    white += 1;
                } else if r > g && g > b {
                    terracota += 1;
                }
            }
        }
        assert!(opaque > size * size / 2, "o quadrado deve cobrir a maior parte");
        assert!(terracota > opaque / 2, "o fundo deve ser terracota");
        let letter = white as f32 / opaque as f32;
        assert!(
            (0.05..0.35).contains(&letter),
            "a letra deve ocupar entre 5% e 35% do quadrado, veio {:.0}%",
            letter * 100.0
        );
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
