//! What a drawer's body looks like: one image of the filter field, the
//! scope, the rows and what else is on it, drawn on the main thread (the
//! column shows it on the open sheet). Drawing it again is only for when
//! something changed, so the lines of text are kept (`Lines`) between
//! drawings: hovering across the rows doesn't shape them again.

use std::collections::HashMap;
use std::rc::Rc;

use wl_shell_render::Image;
use wl_shell_render::text::{self, Field, Pill, Text};

use crate::drawer::{self, Layout};

/// The color tab's width and its outline.
const TAB_W: f64 = 5.0;
/// Between a row's start and its text.
const TEXT_X: f64 = 16.0;
/// The most lines kept.
const KEPT: usize = 256;
/// The icon in a row.
pub const ICON: f64 = 16.0;

pub fn title_size(size: f64) -> f64 {
    size * 0.95
}

pub fn small_size(size: f64) -> f64 {
    (size * 0.8).max(9.0)
}

/// A row's height: its two lines, set a little closer than they are
/// spaced.
pub fn row_height(size: f64) -> f64 {
    (text::line_height(title_size(size)) + text::line_height(small_size(size)) - 3) as f64
}

/// The footer's height.
pub fn foot_height(size: f64) -> f64 {
    text::line_height(small_size(size)) as f64
}

/// A row to draw.
pub struct Row {
    pub title: String,
    pub sub: String,
    /// Its note's paper, 0xRRGGBBAA.
    pub color: u32,
    pub overdue: bool,
    pub selected: bool,
    pub hovered: bool,
}

/// Everything a drawing depends on.
pub struct View<'a> {
    pub scale: i32,
    pub w: f64,
    pub h: f64,
    pub layout: &'a Layout,
    pub font: &'a str,
    /// The font size of the sheets.
    pub size: f64,
    /// The text's color, 0xRRGGBBAA.
    pub ink: u32,
    pub overdue: u32,
    /// The filter as typed, its width the room for it.
    pub filter: Field,
    pub placeholder: &'a str,
    pub scope: String,
    pub scope_lit: bool,
    pub rows: Vec<Row>,
    /// The trash icon (in its color), and the row it is on.
    pub trash: Option<(usize, Image)>,
    /// What an empty list says.
    pub empty: Option<String>,
    /// How many rows are above the first one, and below the last.
    pub above: usize,
    pub more: usize,
}

type LineKey = (String, u32, u64, i32, i32);

/// The lines of text drawn, to draw again without shaping them.
#[derive(Default)]
pub struct Lines(HashMap<LineKey, Rc<Pill>>);

impl Lines {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    fn get(
        &mut self,
        fonts: &mut Text,
        scale: i32,
        text: &str,
        style: &text::Style,
        width: i32,
    ) -> Rc<Pill> {
        let key = (
            text.to_owned(),
            style.color,
            style.size.to_bits(),
            width,
            scale,
        );
        if let Some(line) = self.0.get(&key) {
            return line.clone();
        }
        if self.0.len() >= KEPT {
            self.0.clear();
        }
        let line = Rc::new(fonts.line(text, style, width, scale));
        self.0.insert(key, line.clone());
        line
    }
}

/// `rgba` with its alpha set to `alpha`.
fn faint(rgba: u32, alpha: u32) -> u32 {
    (rgba & !0xff) | alpha
}

/// Draw `view`: the image, and how far the filter is scrolled sideways.
pub fn compose(fonts: &mut Text, lines: &mut Lines, view: &View) -> (Image, f64) {
    let s = view.scale.max(1);
    let px = |v: f64| (v * s as f64).round() as i32;
    let (bw, bh) = (px(view.w).max(1), px(view.h).max(1));
    let mut canvas = Image {
        width: bw as u32,
        height: bh as u32,
        data: vec![0; (bw * bh * 4) as usize],
    };
    let small = text::Style {
        font: view.font.to_owned(),
        size: small_size(view.size),
        color: faint(view.ink, 0x88),
        background: 0,
    };
    let title = text::Style {
        font: view.font.to_owned(),
        size: title_size(view.size),
        color: view.ink,
        background: 0,
    };
    let fill = |canvas: &mut Image, r: wl_shell_ui::Rect, radius: f64, color: u32| {
        let image = text::rounded_rect(px(r.w), px(r.h), radius * s as f64, color);
        blit(canvas, &image, px(r.x), px(r.y));
    };
    let put = |canvas: &mut Image, line: &Pill, x: f64, y: f64| {
        blit(canvas, &line.image, px(x), px(y));
    };
    let l = view.layout;

    // The filter, and the scope.
    fill(&mut canvas, l.filter, 4.0, faint(view.ink, 0x16));
    let inset = drawer::INSET;
    let field_w = (l.filter.w - 2.0 * inset).max(1.0);
    let field = Field {
        width: field_w.round() as i32,
        ..view.filter.clone()
    };
    let drawn = fonts.field(&field);
    let fy = l.filter.y + (l.filter.h - drawn.height as f64) / 2.0;
    if field.text.is_empty() {
        let hint = lines.get(fonts, s, view.placeholder, &small, field_w as i32);
        put(&mut canvas, &hint, l.filter.x + inset, fy + 1.0);
    }
    blit(&mut canvas, &drawn.image, px(l.filter.x + inset), px(fy));
    if view.scope_lit {
        fill(&mut canvas, l.scope, 4.0, faint(view.ink, 0x16));
    }
    let scope_style = text::Style {
        color: faint(view.ink, 0xc0),
        ..small.clone()
    };
    let scope = lines.get(
        fonts,
        s,
        &format!("{} ▾", view.scope),
        &scope_style,
        l.scope.w as i32,
    );
    put(
        &mut canvas,
        &scope,
        l.scope.x + (l.scope.w - scope.width as f64) / 2.0,
        l.scope.y + (l.scope.h - scope.height as f64) / 2.0,
    );

    // The rows.
    let title_h = text::line_height(title.size) as f64;
    for (n, row) in view.rows.iter().enumerate() {
        let Some(&r) = l.rows.get(n) else { break };
        if row.selected {
            fill(&mut canvas, r, 5.0, faint(view.ink, 0x22));
        } else if row.hovered {
            fill(&mut canvas, r, 5.0, faint(view.ink, 0x12));
        }
        let tab = wl_shell_ui::Rect::new(r.x + 4.0, r.y + 4.0, TAB_W, (r.h - 8.0).max(2.0));
        let outline = wl_shell_ui::Rect::new(tab.x - 1.0, tab.y - 1.0, tab.w + 2.0, tab.h + 2.0);
        fill(&mut canvas, outline, 3.0, faint(view.ink, 0x50));
        fill(&mut canvas, tab, 2.5, row.color);
        let on_trash = view.trash.as_ref().is_some_and(|(i, _)| *i == n);
        let end = match on_trash {
            true => drawer::trash_rect(r, ICON).x - 4.0,
            false => r.right() - 4.0,
        };
        let room = (end - (r.x + TEXT_X)).max(8.0) as i32;
        let line = lines.get(fonts, s, &row.title, &title, room);
        put(&mut canvas, &line, r.x + TEXT_X, r.y);
        if !row.sub.is_empty() {
            let line = lines.get(fonts, s, &row.sub, &small, room);
            put(&mut canvas, &line, r.x + TEXT_X, r.y + title_h - 3.0);
        }
        if row.overdue && !on_trash {
            let d = 6.0;
            let dot = wl_shell_ui::Rect::new(r.right() - 4.0 - d, r.y + (r.h - d) / 2.0, d, d);
            fill(&mut canvas, dot, d / 2.0, view.overdue);
        }
        if let Some((i, icon)) = &view.trash
            && *i == n
        {
            let at = drawer::trash_rect(r, ICON);
            blit(&mut canvas, icon, px(at.x), px(at.y));
        }
    }
    if let Some(message) = &view.empty {
        let line = lines.get(
            fonts,
            s,
            message,
            &small,
            l.filter.w.max(l.scope.right()) as i32,
        );
        let top = l.rows.first().map_or(l.filter.bottom(), |r| r.y);
        put(&mut canvas, &line, l.filter.x + 2.0, top + 6.0);
    }
    if view.above > 0 {
        let line = lines.get(
            fonts,
            s,
            &format!("▴ {} above", view.above),
            &small,
            (l.more.w / 2.0) as i32,
        );
        put(&mut canvas, &line, l.more.x, l.more.y);
    }
    if view.more > 0 {
        let line = lines.get(
            fonts,
            s,
            &format!("▾ {} more", view.more),
            &small,
            l.more.w as i32,
        );
        put(
            &mut canvas,
            &line,
            l.more.right() - line.width as f64,
            l.more.y,
        );
    }
    (canvas, drawn.scroll)
}

/// `src` over `dst` at (`x`, `y`), both premultiplied BGRA; what is
/// outside `dst` is left out.
pub fn blit(dst: &mut Image, src: &Image, x: i32, y: i32) {
    let (dw, dh) = (dst.width as i32, dst.height as i32);
    let sw = src.width as i32;
    for sy in 0..src.height as i32 {
        let ty = y + sy;
        if ty < 0 || ty >= dh {
            continue;
        }
        for sx in 0..sw {
            let tx = x + sx;
            if tx < 0 || tx >= dw {
                continue;
            }
            let si = ((sy * sw + sx) * 4) as usize;
            let alpha = src.data[si + 3] as u32;
            if alpha == 0 {
                continue;
            }
            let di = ((ty * dw + tx) * 4) as usize;
            for k in 0..4 {
                let over = dst.data[di + k] as u32 * (255 - alpha) + 127;
                dst.data[di + k] = (src.data[si + k] as u32 + over / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, bgra: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            data: bgra.repeat((w * h) as usize),
        }
    }

    fn pixel(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.data[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn blitting_blends_and_clips() {
        let mut dst = solid(4, 4, [0, 0, 0, 0]);
        // Opaque replaces, transparent leaves, half blends.
        blit(&mut dst, &solid(2, 2, [10, 20, 30, 255]), 1, 1);
        assert_eq!(pixel(&dst, 1, 1), [10, 20, 30, 255]);
        assert_eq!(pixel(&dst, 2, 2), [10, 20, 30, 255]);
        assert_eq!(pixel(&dst, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&dst, 3, 3), [0, 0, 0, 0]);
        blit(&mut dst, &solid(1, 1, [0, 0, 0, 0]), 1, 1);
        assert_eq!(pixel(&dst, 1, 1), [10, 20, 30, 255]);
        // Premultiplied: half of white over black.
        blit(&mut dst, &solid(1, 1, [128, 128, 128, 128]), 1, 1);
        let p = pixel(&dst, 1, 1);
        assert_eq!(p[3], 255);
        assert!(p[0] > 128 && p[0] < 140, "{p:?}");
        // Partly or wholly outside: no panic, no wrapping.
        blit(&mut dst, &solid(3, 3, [1, 1, 1, 255]), -1, -1);
        assert_eq!(pixel(&dst, 1, 1), [1, 1, 1, 255]);
        blit(&mut dst, &solid(3, 3, [2, 2, 2, 255]), 3, 3);
        assert_eq!(pixel(&dst, 3, 3), [2, 2, 2, 255]);
        blit(&mut dst, &solid(2, 2, [9, 9, 9, 255]), 50, -50);
        assert_eq!(pixel(&dst, 3, 0), [0, 0, 0, 0]);
    }

    fn row(title: &str, selected: bool, hovered: bool) -> Row {
        Row {
            title: title.into(),
            sub: "firefox · a first line that is much too long to fit in the row at all".into(),
            color: 0xfff2a8ff,
            overdue: false,
            selected,
            hovered,
        }
    }

    fn view<'a>(layout: &'a Layout, rows: Vec<Row>, scale: i32) -> View<'a> {
        let filter = Field {
            text: "rel".into(),
            caret: Some(3),
            selection: None,
            style: text::Style {
                font: "sans-serif".into(),
                size: 12.0,
                color: 0x2b2b2bff,
                background: 0,
            },
            bold: false,
            width: drawer::field_width(248.0) as i32,
            scroll: 0.0,
            scale,
        };
        let trash = Some((
            1,
            solid(16 * scale as u32, 16 * scale as u32, [30, 30, 30, 255]),
        ));
        View {
            scale,
            w: 248.0,
            h: 200.0,
            layout,
            font: "sans-serif",
            size: 13.0,
            ink: 0x2b2b2bff,
            overdue: 0xd64541ff,
            filter,
            placeholder: "Filter",
            scope: "here".into(),
            scope_lit: false,
            rows,
            trash,
            empty: None,
            above: 2,
            more: 3,
        }
    }

    fn painted(img: &Image, x: f64, y: f64, w: f64, h: f64, scale: i32) -> usize {
        let s = scale as f64;
        let mut n = 0;
        for py in (y * s) as u32..((y + h) * s) as u32 {
            for px in (x * s) as u32..((x + w) * s) as u32 {
                n += (pixel(img, px, py)[3] > 0) as usize;
            }
        }
        n
    }

    #[test]
    fn drawing_a_drawer() {
        let mut fonts = Text::new();
        let mut lines = Lines::default();
        for scale in [1, 2] {
            let (row_h, foot) = (row_height(13.0), foot_height(13.0));
            let layout = drawer::layout(248.0, 200.0, row_h, foot, 12);
            // More rows than the layout has room for: the rest are left out.
            let rows: Vec<Row> = (0..9)
                .map(|i| row(&format!("Note {i}"), i == 0, i == 1))
                .collect();
            let (image, scroll) = compose(&mut fonts, &mut lines, &view(&layout, rows, scale));
            assert_eq!(
                (image.width, image.height),
                (248 * scale as u32, 200 * scale as u32)
            );
            assert_eq!(image.data.len(), (image.width * image.height * 4) as usize);
            assert_eq!(scroll, 0.0);
            let s = scale;
            // The filter, the selected row, its tab, and the footer are drawn.
            let f = layout.filter;
            assert!(painted(&image, f.x, f.y, f.w, f.h, s) > 100);
            let r0 = layout.rows[0];
            assert!(painted(&image, r0.x, r0.y, r0.w, r0.h, s) > 500);
            let tab = painted(&image, r0.x + 4.0, r0.y + 4.0, 5.0, r0.h - 8.0, s);
            assert!(tab > 20, "{tab}");
            let m = layout.more;
            assert!(painted(&image, m.x, m.y, m.w, m.h, s) > 20);
            // Nothing past the last row that fits, but the footer.
            let last = layout.rows.last().unwrap();
            let below = painted(
                &image,
                0.0,
                last.bottom() + 1.0,
                248.0,
                m.y - last.bottom() - 1.0,
                s,
            );
            assert_eq!(below, 0);
            // Drawing it again, from the kept lines, changes nothing.
            let rows: Vec<Row> = (0..9)
                .map(|i| row(&format!("Note {i}"), i == 0, i == 1))
                .collect();
            let (again, _) = compose(&mut fonts, &mut lines, &view(&layout, rows, scale));
            assert_eq!(again, image);
        }
    }

    #[test]
    fn drawing_an_empty_drawer() {
        let mut fonts = Text::new();
        let mut lines = Lines::default();
        let layout = drawer::layout(248.0, 200.0, row_height(13.0), foot_height(13.0), 0);
        let mut v = view(&layout, Vec::new(), 1);
        v.trash = None;
        v.above = 0;
        v.more = 0;
        v.empty = Some("No hidden notes here (Tab: all)".into());
        let (image, _) = compose(&mut fonts, &mut lines, &v);
        let top = layout.rows[0].y;
        assert!(painted(&image, 0.0, top, 248.0, 30.0, 1) > 50);
        // A narrow drawer and a long message don't overflow the image.
        let narrow = drawer::layout(60.0, 200.0, row_height(13.0), foot_height(13.0), 0);
        let mut v = view(&narrow, Vec::new(), 1);
        v.w = 60.0;
        v.empty = Some("No hidden notes here (Tab: all)".into());
        let (image, _) = compose(&mut fonts, &mut lines, &v);
        assert_eq!(image.width, 60);
    }

    #[test]
    fn rows_fit_five_to_a_sheet() {
        // The default: a sheet of 220 px, 13 px text.
        let (row, foot) = (row_height(13.0), foot_height(13.0));
        let body = 220.0 - 20.0;
        let l = drawer::layout(248.0, body, row, foot, 5);
        assert_eq!(l.capacity(), 5, "{row}");
        let l = drawer::layout(248.0, body, row, foot, 30);
        assert!(l.capacity() >= 4, "{row} {foot}");
        // Bigger text, bigger rows.
        assert!(row_height(20.0) > row_height(13.0));
        assert!(small_size(5.0) >= 9.0);
    }
}
