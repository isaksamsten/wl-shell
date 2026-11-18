//! A sheet's file bar: the note's `file://` links as small icons along its
//! bottom, in their order in the text. Pure, unit tested.
//!
//! Icons that don't fit leave the last slot to a `+n` for the rest.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use wl_shell_notes::{Color, PRESETS};
use wl_shell_ui::Rect;

use wl_shell_render::files;

/// The local files `links` (a note's link destinations) point at, each
/// once, in order.
pub fn files(links: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for link in links {
        if let Some(path) = files::uri_path(link.trim())
            && !out.contains(&path)
        {
            out.push(path);
        }
    }
    out
}

/// What a bar `width` wide shows of `n` files, icons `icon` wide and
/// `gap` apart: how many icons, and the count behind a `+n` in the slot
/// after them, if any.
pub fn fit(n: usize, width: f64, icon: f64, gap: f64) -> (usize, Option<usize>) {
    let slots = ((width + gap) / (icon + gap)).floor().max(0.0) as usize;
    match n <= slots {
        true => (n, None),
        false => {
            let shown = slots.saturating_sub(1);
            (shown, Some(n - shown))
        }
    }
}

/// The name a file shows with: its file name.
pub fn name(path: &std::path::Path) -> String {
    (path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub const TRASH_TIMEOUT: Duration = Duration::from_secs(3);

/// The deadline applies only while the same trash button is hovered.
#[derive(Default)]
pub struct Trash(Option<Instant>);

impl Trash {
    pub fn armed(&self, now: Instant) -> bool {
        self.0.is_some_and(|until| now < until)
    }

    pub fn click(&mut self, now: Instant) -> bool {
        let confirmed = self.armed(now);
        self.0 = (!confirmed).then_some(now + TRASH_TIMEOUT);
        confirmed
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}

pub fn colors(current: Color) -> impl Iterator<Item = Color> {
    matches!(current, Color::Rgb(_))
        .then_some(current)
        .into_iter()
        .chain(PRESETS.into_iter().map(Color::Preset))
}

pub fn color_name(color: Color) -> String {
    match color {
        Color::Preset(name) => name.into(),
        Color::Rgb(rgb) => format!("#{rgb:06x}"),
    }
}

/// The bar reads left to right on either edge. Swatches slide out of
/// the color control, compressing to fit narrow sheets.
pub fn palette(left: f64, dot: Rect, count: usize, progress: f64) -> Vec<Rect> {
    let pitch = ((dot.x - left) / count.max(1) as f64).clamp(0.0, 24.0);
    let size = (pitch * 0.85).min(dot.w);
    (0..count)
        .map(|i| {
            let (cx, cy) = dot.center();
            let target = dot.x - (count - i) as f64 * pitch + pitch / 2.0;
            let x = cx + (target - cx) * progress;
            Rect::new(x - size / 2.0, cy - size / 2.0, size, size)
        })
        .collect()
}

/// Fixed-height cells; scrolling by rows makes the last file reachable
/// without drawing or requesting labels for offscreen cells.
pub struct Grid {
    pub cells: Vec<(usize, Rect)>,
    pub last_row: usize,
}

pub fn grid(area: Rect, count: usize, row: usize) -> Grid {
    let cols = ((area.w / 80.0) as usize).clamp(1, 8);
    let rows = ((area.h / 43.0) as usize).clamp(1, 32 / cols);
    let last_row = count.div_ceil(cols).saturating_sub(rows);
    let first = row.min(last_row) * cols;
    let w = area.w / cols as f64;
    let h = (area.h / rows as f64).min(54.0);
    let cells = (first..count.min(first + rows * cols))
        .map(|i| {
            let j = i - first;
            (
                i,
                Rect::new(
                    area.x + (j % cols) as f64 * w,
                    area.y + (j / cols) as f64 * h,
                    w,
                    h,
                ),
            )
        })
        .collect();
    Grid { cells, last_row }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_confirmation_expires_and_can_be_canceled() {
        let mut trash = Trash::default();
        let now = Instant::now();
        assert!(!trash.click(now));
        assert!(trash.armed(now));
        assert!(!trash.click(now + TRASH_TIMEOUT));
        assert!(trash.click(now + TRASH_TIMEOUT + Duration::from_millis(1)));
        assert!(!trash.click(now));
        trash.clear();
        assert!(!trash.click(now));
    }

    #[test]
    fn palette_fits_on_both_edges() {
        for (left, width) in [(0.0, 240.0), (28.0, 80.0)] {
            let dot = Rect::new(left + width, 40.0, 16.0, 16.0);
            let colors: Vec<_> = colors(Color::Rgb(0x123456)).collect();
            assert_eq!(colors[0], Color::Rgb(0x123456));
            let start = palette(left, dot, colors.len(), 0.0);
            assert!(start.iter().all(|r| r.center() == dot.center()));
            let end = palette(left, dot, colors.len(), 1.0);
            assert!(end[0].x >= left && end.last().unwrap().right() <= dot.x);
            assert!(end.windows(2).all(|rs| rs[0].right() <= rs[1].x + 1e-9));
        }
    }

    #[test]
    fn grid_can_scroll_to_every_file() {
        let area = Rect::new(12.0, 25.0, 245.0, 115.0);
        assert_eq!(grid(area, 17, 0).cells[0].0, 0);
        let end = grid(area, 17, usize::MAX);
        assert_eq!(end.last_row, 4);
        assert_eq!(end.cells.last().unwrap().0, 16);
        assert!(
            end.cells
                .iter()
                .all(|(_, r)| r.right() <= area.right() && r.bottom() <= area.bottom())
        );
    }

    #[test]
    fn local_files_once() {
        let links = [
            "file:///home/a/report.pdf".to_string(),
            "https://example.com".into(),
            "file:///home/a/My%20Budget.ods".into(),
            "file:///home/a/report.pdf".into(),
            "notes/other.md".into(),
        ];
        assert_eq!(
            files(&links),
            [
                PathBuf::from("/home/a/report.pdf"),
                PathBuf::from("/home/a/My Budget.ods")
            ]
        );
        assert_eq!(name(&files(&links)[1]), "My Budget.ods");
    }

    #[test]
    fn fitting() {
        // 16 px icons 8 apart: 100 px hold 4.
        assert_eq!(fit(3, 100.0, 16.0, 8.0), (3, None));
        assert_eq!(fit(4, 100.0, 16.0, 8.0), (4, None));
        assert_eq!(fit(5, 100.0, 16.0, 8.0), (3, Some(2)));
        assert_eq!(fit(0, 100.0, 16.0, 8.0), (0, None));
        assert_eq!(fit(2, 10.0, 16.0, 8.0), (0, Some(2)));
    }
}
