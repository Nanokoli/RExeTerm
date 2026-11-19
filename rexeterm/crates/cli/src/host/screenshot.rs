//! Rasterize the daemon's current viewport, including cell styles.
use super::screen::Viewport;
use anyhow::{anyhow, bail, Result};
use fontdue::{Font, FontSettings};
use std::{path::Path, sync::OnceLock};

/// Monospace font, in preference order.
const MONO_FONTS: [&str; 6] = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "C:/Windows/Fonts/consola.ttf",
    "C:/Windows/Fonts/cour.ttf",
];
/// Noto Sans Mono CJK SC subset: GB2312 + ASCII + TUI symbols (OFL 1.1, assets/LICENSE-NotoSansSC.txt).
static EMBEDDED_CJK: &[u8] = include_bytes!("../../assets/NotoMonoSC-Subset.otf");

/// Fallbacks for characters the monospace face does not cover (CJK, symbols).
const FALLBACK_FONTS: [&str; 8] = [
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/simsun.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/arphic/uming.ttc",
];

const BACKGROUND: u8 = 0x10;
const FOREGROUND: u8 = 0xe6;
const FONT_SIZE: f32 = 16.0;
const PADDING: usize = 6;
/// Coverage above this becomes ink (nearest neighbour, no blending).
const COVERAGE: u8 = 128;

type Rgb = [u8; 3];

fn indexed_color(index: u8) -> Rgb {
    const BASE: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00],
        [0xcd, 0x31, 0x31],
        [0x0d, 0xbc, 0x79],
        [0xe5, 0xe5, 0x10],
        [0x24, 0x72, 0xc8],
        [0xbc, 0x3f, 0xbc],
        [0x11, 0xa8, 0xcd],
        [0xe5, 0xe5, 0xe5],
        [0x66, 0x66, 0x66],
        [0xf1, 0x4c, 0x4c],
        [0x23, 0xd1, 0x8b],
        [0xf5, 0xf5, 0x43],
        [0x3b, 0x8e, 0xfa],
        [0xd6, 0x70, 0xd6],
        [0x29, 0xb8, 0xdb],
        [0xff, 0xff, 0xff],
    ];
    if index < 16 {
        return BASE[index as usize];
    }
    if index < 232 {
        let index = index - 16;
        let steps = [0u8, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
        return [
            steps[(index / 36) as usize],
            steps[((index % 36) / 6) as usize],
            steps[(index % 6) as usize],
        ];
    }
    let level = 8 + (index - 232) * 10;
    [level, level, level]
}

fn vt_color(color: Option<avt::Color>) -> Option<Rgb> {
    match color? {
        avt::Color::Indexed(index) => Some(indexed_color(index)),
        avt::Color::RGB(rgb) => Some([rgb.r, rgb.g, rgb.b]),
    }
}

/// Which edges a box-drawing character connects: up, down, left, right.
fn box_edges(ch: char) -> Option<[bool; 4]> {
    Some(match ch {
        '\u{2500}' => [false, false, true, true], // ─
        '\u{2502}' => [true, true, false, false], // │
        '\u{250c}' => [false, true, false, true], // ┌
        '\u{2510}' => [false, true, true, false], // ┐
        '\u{2514}' => [true, false, false, true], // └
        '\u{2518}' => [true, false, true, false], // ┘
        '\u{251c}' => [true, true, false, true],  // ├
        '\u{2524}' => [true, true, true, false],  // ┤
        '\u{252c}' => [false, true, true, true],  // ┬
        '\u{2534}' => [true, false, true, true],  // ┴
        '\u{253c}' => [true, true, true, true],   // ┼
        _ => return None,
    })
}

fn fill(canvas: &mut [u8], width: usize, height: usize, x: i32, y: i32, color: [u8; 3]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let index = (y as usize * width + x as usize) * 4;
    canvas[index..index + 3].copy_from_slice(&color);
}

#[allow(clippy::too_many_arguments)]
fn draw_box(
    canvas: &mut [u8],
    width: usize,
    height: usize,
    x0: i32,
    y0: i32,
    cell_width: usize,
    cell_height: usize,
    edges: [bool; 4],
    color: [u8; 3],
    thickness: i32,
) {
    let mid_x = x0 + cell_width as i32 / 2;
    let mid_y = y0 + cell_height as i32 / 2;
    let [up, down, left, right] = edges;
    if left || right {
        let from = if left { x0 } else { mid_x };
        let to = if right {
            x0 + cell_width as i32 - 1
        } else {
            mid_x
        };
        for x in from..=to {
            for offset in 0..thickness {
                fill(canvas, width, height, x, mid_y + offset, color);
            }
        }
    }
    if up || down {
        let from = if up { y0 } else { mid_y };
        let to = if down {
            y0 + cell_height as i32 - 1
        } else {
            mid_y
        };
        for y in from..=to {
            for offset in 0..thickness {
                fill(canvas, width, height, mid_x + offset, y, color);
            }
        }
    }
}

fn load(path: &str) -> Option<Font> {
    if !Path::new(path).exists() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Font::from_bytes(bytes, FontSettings::default()).ok()
}

fn is_monospace(font: &Font) -> bool {
    let advance = |ch: char| font.metrics(ch, FONT_SIZE).advance_width;
    (advance('i') - advance('M')).abs() < 0.01 && (advance('M') - advance('W')).abs() < 0.01
}

/// Primary face plus fallbacks: `REXETERM_SCREEN_FONT` wins, then the built-in lists.
/// The first entry must be monospace, otherwise the grid is wrong.
fn load_fonts() -> Result<Vec<Font>> {
    let mut mono_paths: Vec<String> = Vec::new();
    if let Ok(extra) = std::env::var("REXETERM_SCREEN_FONT") {
        mono_paths.push(extra);
    }
    mono_paths.extend(MONO_FONTS.iter().map(|path| path.to_string()));
    let mut fallback_paths: Vec<String> = Vec::new();
    if let Ok(extra) = std::env::var("REXETERM_SCREEN_FONT_FALLBACK") {
        fallback_paths.extend(extra.split([';', ':']).map(str::to_string));
    }
    fallback_paths.extend(FALLBACK_FONTS.iter().map(|path| path.to_string()));

    let mut fonts: Vec<Font> = Vec::new();
    for path in mono_paths {
        if let Some(font) = load(&path) {
            fonts.push(font);
            if fonts.iter().any(is_monospace) {
                break;
            }
        }
    }
    // Embedded CJK subset: same screenshot everywhere, and it also covers CJK.
    if let Ok(font) = Font::from_bytes(EMBEDDED_CJK, FontSettings::default()) {
        fonts.push(font);
    }
    for path in fallback_paths {
        if fonts.len() >= 5 {
            break;
        }
        if let Some(font) = load(&path) {
            fonts.push(font);
        }
    }
    if fonts.is_empty() {
        bail!("no font found; set REXETERM_SCREEN_FONT to a .ttf/.ttc path");
    }
    if let Some(index) = fonts.iter().position(is_monospace) {
        fonts.swap(0, index);
    }
    Ok(fonts)
}

fn glyph(fonts: &[Font], ch: char) -> Option<&Font> {
    fonts
        .iter()
        .find(|font| font.lookup_glyph_index(ch) != 0)
        .or_else(|| fonts.first())
}

pub(super) fn screen_png(screen: &Viewport) -> Result<Vec<u8>> {
    static FONTS: OnceLock<Result<Vec<Font>, String>> = OnceLock::new();
    let fonts = FONTS
        .get_or_init(|| load_fonts().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| anyhow!("{message}"))?;
    let primary = &fonts[0];
    let line = primary
        .horizontal_line_metrics(FONT_SIZE)
        .map(|metrics| metrics.new_line_size)
        .unwrap_or(FONT_SIZE * 1.25);
    let cell_height = line.ceil().max(1.0) as usize;
    let cell_width = primary
        .metrics('M', FONT_SIZE)
        .advance_width
        .ceil()
        .max(1.0) as usize;
    // Text sits on the cell's optical middle, so box drawing and glyphs line up.
    let (ascent, descent) = primary
        .horizontal_line_metrics(FONT_SIZE)
        .map(|metrics| (metrics.ascent, metrics.descent))
        .unwrap_or((FONT_SIZE * 0.8, FONT_SIZE * 0.2));
    let width = screen.cols * cell_width + PADDING * 2;
    let height = screen.rows * cell_height + PADDING * 2;
    let pixels = width
        .checked_mul(height)
        .filter(|pixels| *pixels <= 12_000_000)
        .ok_or_else(|| anyhow!("PTY screenshot exceeds the pixel budget"))?;
    let mut canvas = vec![0u8; pixels * 4];
    let default_bg = [BACKGROUND; 3];
    for pixel in canvas.as_chunks_mut::<4>().0 {
        *pixel = [BACKGROUND, BACKGROUND, BACKGROUND, 0xff];
    }
    // Paint backgrounds first, including the tail cell of wide characters.
    for (row, line) in screen.cells.iter().enumerate() {
        for (col, cell) in line.iter().enumerate() {
            let (_, bg) = colors(cell.pen());
            if bg == default_bg {
                continue;
            }
            let x0 = PADDING + col * cell_width;
            let y0 = PADDING + row * cell_height;
            for y in y0..y0 + cell_height {
                for x in x0..x0 + cell_width {
                    fill(&mut canvas, width, height, x as i32, y as i32, bg);
                }
            }
        }
    }
    for (row, line) in screen.cells.iter().enumerate() {
        for (col, cell) in line
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.width() != 0)
        {
            let pen = cell.pen();
            let (fg, _) = colors(pen);
            let x0 = (PADDING + col * cell_width) as i32;
            let y0 = (PADDING + row * cell_height) as i32;
            let ch = cell.char();
            if let Some(edges) = box_edges(ch) {
                let thickness = if pen.is_bold() { 2 } else { 1 };
                draw_box(
                    &mut canvas,
                    width,
                    height,
                    x0,
                    y0,
                    cell_width,
                    cell_height,
                    edges,
                    fg,
                    thickness,
                );
            } else if ch != ' ' {
                if let Some(font) = glyph(fonts, ch) {
                    let (metrics, bitmap) = font.rasterize(ch, FONT_SIZE);
                    let left = x0 + metrics.xmin;
                    let baseline =
                        y0 + ((cell_height as f32 + ascent + descent) / 2.0).round() as i32;
                    let top = baseline - metrics.height as i32 - metrics.ymin;
                    for (gy, chunk) in bitmap.chunks(metrics.width.max(1)).enumerate() {
                        for (gx, alpha) in chunk.iter().enumerate() {
                            if *alpha >= COVERAGE {
                                let x = left + gx as i32;
                                let y = top + gy as i32;
                                fill(&mut canvas, width, height, x, y, fg);
                                if pen.is_bold() {
                                    fill(&mut canvas, width, height, x + 1, y, fg);
                                }
                            }
                        }
                    }
                }
            }
            for (enabled, offset) in [
                (pen.is_underline(), cell_height.saturating_sub(2)),
                (pen.is_strikethrough(), cell_height / 2),
            ] {
                if enabled {
                    for col in 0..cell_width * usize::from(cell.width()) {
                        fill(
                            &mut canvas,
                            width,
                            height,
                            x0 + col as i32,
                            y0 + offset as i32,
                            fg,
                        );
                    }
                }
            }
        }
    }
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| anyhow!(error))?;
        writer
            .write_image_data(&canvas)
            .map_err(|error| anyhow!(error))?;
    }
    Ok(png_bytes)
}

fn colors(pen: &avt::Pen) -> (Rgb, Rgb) {
    let fg = vt_color(pen.foreground()).unwrap_or([FOREGROUND; 3]);
    let bg = vt_color(pen.background()).unwrap_or([BACKGROUND; 3]);
    if pen.is_inverse() {
        (bg, fg)
    } else {
        (fg, bg)
    }
}
