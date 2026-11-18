//! A sheet's file bar: the note's `file://` links as small icons along its
//! bottom, in their order in the text. Pure, unit tested.
//!
//! Icons that don't fit leave the last slot to a `+n` for the rest.

use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

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
