//! Exported screens: draws a screen into an off-screen buffer and writes the buffer as an
//! HTML page, one fixed-size box per terminal cell with the cell's own colours, so the
//! page looks like the terminal and can be turned into a picture.

use super::*;
use draw::Theme;
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Cell,
    style::{Color, Modifier},
};

/// The terminal's own colours, used where a theme keeps them (`Color::Reset`).
const DEFAULT_FG: (u8, u8, u8) = (204, 204, 204);
const DEFAULT_BG: (u8, u8, u8) = (12, 12, 12);
/// One cell, in CSS pixels.
const CELL_W: u16 = 9;
const CELL_H: u16 = 18;

/// The page size in CSS pixels for a screen of `w` x `h` cells.
pub fn page_size(w: u16, h: u16) -> (u32, u32) {
    (
        u32::from(w) * u32::from(CELL_W),
        u32::from(h) * u32::from(CELL_H),
    )
}

/// Draws one screen (bodies hidden) and returns it as a self-contained HTML page.
pub fn page(root: &Path, s: &Snapshot, screen: Screen, t: &Theme, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("an off-screen terminal");
    term.draw(|f| draw(f, root, s, false, screen, t))
        .expect("drawing to memory");
    let buf = term.backend().buffer();

    // One CSS class per distinct cell style keeps the page small.
    let mut classes: Vec<String> = Vec::new();
    let mut grid = String::new();
    for y in 0..h {
        grid.push_str("<div>");
        for x in 0..w {
            let cell = &buf[(x, y)];
            let (css, sym) = cell_css(cell);
            let class = match classes.iter().position(|c| *c == css) {
                Some(i) => i,
                None => {
                    classes.push(css);
                    classes.len() - 1
                }
            };
            grid.push_str(&format!("<span class=s{class}>{}</span>", html::esc(sym)));
        }
        grid.push_str("</div>\n");
    }

    let styles: String = classes
        .iter()
        .enumerate()
        .map(|(i, c)| format!(".s{i}{{{c}}}\n"))
        .collect();
    let (pw, ph) = page_size(w, h);
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width={pw}\">\
         <title>cq-top {name}</title>\n<style>\n\
         html,body{{margin:0;background:{bg};}}\n\
         .g{{width:{pw}px;height:{ph}px;overflow:hidden;\
         font:15px/{CELL_H}px \"Cascadia Mono\",Consolas,\"Courier New\",monospace;}}\n\
         .g div{{display:flex;height:{CELL_H}px;}}\n\
         .g span{{display:inline-block;flex:none;width:{CELL_W}px;height:{CELL_H}px;\
         text-align:center;white-space:pre;overflow:hidden;}}\n\
         {styles}</style></head>\n<body><div class=\"g\">\n{grid}</div></body></html>\n",
        name = screen.name(),
        bg = hex(t.bg.map_or(DEFAULT_BG, |c| rgb(c, DEFAULT_BG))),
    )
}

/// The share of a cell, in eighths, that a bar glyph fills, and from which side: lower
/// eighths grow up, left eighths grow right. Such glyphs are drawn as a background instead,
/// so a bar made of several rows or cells has no seams between them.
fn block_fill(sym: &str) -> Option<(&'static str, u32)> {
    let mut chars = sym.chars();
    let c = u32::from(chars.next()?);
    if chars.next().is_some() {
        return None;
    }
    match c {
        0x2581..=0x2588 => Some(("to top", c - 0x2580)),
        0x2589..=0x258F => Some(("to right", 0x2590 - c)),
        _ => None,
    }
}

/// The CSS for a cell and the text to put in it.
fn cell_css(cell: &Cell) -> (String, &str) {
    let m = cell.modifier;
    let (mut fg, mut bg) = (rgb(cell.fg, DEFAULT_FG), rgb(cell.bg, DEFAULT_BG));
    if m.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if m.contains(Modifier::DIM) {
        fg = mix(fg, bg);
    }
    let mut sym = match cell.symbol() {
        "" => " ",
        sym => sym,
    };
    let mut css = match block_fill(sym) {
        Some((side, eighths)) => {
            sym = " ";
            let pct = eighths * 100 / 8;
            format!(
                "color:{fg};background:linear-gradient({side},{fg} {pct}%,{bg} {pct}%);",
                fg = hex(fg),
                bg = hex(bg)
            )
        }
        None => format!("color:{};background:{};", hex(fg), hex(bg)),
    };
    if m.contains(Modifier::BOLD) {
        css.push_str("font-weight:bold;");
    }
    if m.contains(Modifier::ITALIC) {
        css.push_str("font-style:italic;");
    }
    if m.contains(Modifier::UNDERLINED) {
        css.push_str("text-decoration:underline;");
    }
    (css, sym)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8)) -> (u8, u8, u8) {
    let m = |x: u8, y: u8| ((u16::from(x) + u16::from(y)) / 2) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The 16 named colours, as Windows Terminal's default scheme (Campbell) shows them.
const ANSI: [(u8, u8, u8); 16] = [
    (12, 12, 12),
    (197, 15, 31),
    (19, 161, 14),
    (193, 156, 0),
    (0, 55, 218),
    (136, 23, 152),
    (58, 150, 221),
    (204, 204, 204),
    (118, 118, 118),
    (231, 72, 86),
    (22, 198, 12),
    (249, 241, 165),
    (59, 120, 255),
    (180, 0, 158),
    (97, 214, 214),
    (242, 242, 242),
];

fn rgb(c: Color, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Reset => default,
        Color::Black => ANSI[0],
        Color::Red => ANSI[1],
        Color::Green => ANSI[2],
        Color::Yellow => ANSI[3],
        Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5],
        Color::Cyan => ANSI[6],
        Color::Gray => ANSI[7],
        Color::DarkGray => ANSI[8],
        Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10],
        Color::LightYellow => ANSI[11],
        Color::LightBlue => ANSI[12],
        Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14],
        Color::White => ANSI[15],
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(i) => match i {
            0..=15 => ANSI[usize::from(i)],
            16..=231 => {
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                let i = i - 16;
                (step(i / 36), step(i / 6 % 6), step(i % 6))
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                (v, v, v)
            }
        },
    }
}
