//! A sheet's file bar: the shelf button, with how many files the note
//! links (`file://` links) on it, which opens the shelf (shelf.rs) of all
//! of them; and the controls. Pure, unit tested.

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

/// The count on the shelf button: how many files the note has, `9+`
/// past nine; None without any.
pub fn count(n: usize) -> Option<String> {
    match n {
        0 => None,
        1..=9 => Some(n.to_string()),
        _ => Some("9+".into()),
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

/// How wide the cell of a choice labeled `label` in the due row is:
/// its letters and some room around them.
pub fn cell_width(label: &str) -> f64 {
    label.chars().count() as f64 * 6.0 + 10.0
}

/// The row of due choices: cells (`widths` wide, in order) slide out of
/// the clock icon `anchor` and settle side by side, `gap` apart, with
/// the last one's right edge at `right` and the row within `left`. A row
/// too wide for that room is scaled down to fit. `progress` is 0 with
/// every cell on the clock, 1 with each in its place.
pub fn row(
    left: f64,
    right: f64,
    anchor: Rect,
    widths: &[f64],
    gap: f64,
    progress: f64,
) -> Vec<Rect> {
    let room = (right - left).max(0.0);
    let total = widths.iter().sum::<f64>() + gap * widths.len().saturating_sub(1) as f64;
    let fit = if total > room && total > 0.0 {
        room / total
    } else {
        1.0
    };
    let mut edge = right;
    let mut cells: Vec<Rect> = widths
        .iter()
        .rev()
        .map(|w| {
            let w = w * fit;
            let target = edge - w;
            edge = target - gap * fit;
            let x = anchor.x + (target - anchor.x) * progress;
            Rect::new(x, anchor.y, w, anchor.h)
        })
        .collect();
    cells.reverse();
    cells
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
    fn the_count_on_the_button() {
        assert_eq!(count(0), None);
        assert_eq!(count(3).as_deref(), Some("3"));
        assert_eq!(count(9).as_deref(), Some("9"));
        assert_eq!(count(10).as_deref(), Some("9+"));
    }

    #[test]
    fn the_due_row_takes_the_whole_bar() {
        let widths: Vec<f64> = ["1h", "Tonight", "Tomorrow", "Mon", "×"]
            .iter()
            .map(|l| cell_width(l))
            .collect();
        // The sheet's bar is 248 wide, the clock four icons from its end.
        let (left, right) = (12.0, 260.0);
        let clock = Rect::new(right - 16.0 - 4.0 * 24.0, 40.0, 16.0, 16.0);
        let start = row(left, right, clock, &widths, 4.0, 0.0);
        assert!(start.iter().all(|r| (r.x - clock.x).abs() < 1e-9));
        let end = row(left, right, clock, &widths, 4.0, 1.0);
        assert_eq!(end.len(), widths.len());
        // Not scaled: every label has the room it asked for.
        assert!(end.iter().zip(&widths).all(|(r, w)| (r.w - w).abs() < 1e-9));
        // In order, apart, inside the bar, ending at its end.
        assert!(end[0].x >= left && (end.last().unwrap().right() - right).abs() < 1e-9);
        assert!(end.windows(2).all(|rs| rs[0].right() <= rs[1].x + 1e-9));
        assert!(end.iter().all(|r| r.y == clock.y && r.h == clock.h));
        // A narrower bar scales the row down to fit instead.
        let narrow = row(left, 120.0, clock, &widths, 4.0, 1.0);
        assert!(narrow[0].x >= left - 1e-9 && narrow.last().unwrap().right() <= 120.0 + 1e-9);
        assert!(narrow.windows(2).all(|rs| rs[0].right() <= rs[1].x + 1e-9));
        // No choices, no cells; no room, no panic.
        assert!(row(left, right, clock, &[], 4.0, 1.0).is_empty());
        assert_eq!(row(100.0, 90.0, clock, &widths, 4.0, 1.0).len(), 5);
    }
}
