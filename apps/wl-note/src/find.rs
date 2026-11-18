//! Which note a `<note>` on the command line names. Pure, unit tested.
//!
//! In this order: a path relative to the notes directory (`.md` may be
//! left out), or an absolute one inside it; else a file name (with or
//! without `.md`); else a title, ignoring case. A name or title has to be
//! unique, else the error lists the notes it could be.

use std::path::{Path, PathBuf};

/// The note among `notes` (paths relative to `dir`, and their titles)
/// that `query` names.
pub fn find<'a>(
    dir: &Path,
    notes: &[(&'a Path, Option<&str>)],
    query: &str,
) -> Result<&'a Path, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("no note named".into());
    }
    let with_md = |p: &Path| -> PathBuf {
        match p.extension().is_some_and(|e| e == "md") {
            true => p.to_path_buf(),
            false => p.with_extension("md"),
        }
    };
    let as_path = Path::new(q);
    let rel = match as_path.strip_prefix(dir) {
        Ok(rel) => rel,
        Err(_) => as_path,
    };
    for wanted in [rel.to_path_buf(), with_md(rel)] {
        if let Some((p, _)) = notes.iter().find(|(p, _)| *p == wanted) {
            return Ok(p);
        }
    }
    let unique = |found: Vec<&'a Path>| -> Option<Result<&'a Path, String>> {
        match found.as_slice() {
            [] => None,
            [one] => Some(Ok(*one)),
            many => {
                let list: Vec<String> = many.iter().map(|p| p.display().to_string()).collect();
                Some(Err(format!("'{q}' could be any of: {}", list.join(", "))))
            }
        }
    };
    let name = with_md(Path::new(q));
    let by_name = (notes.iter())
        .filter(|(p, _)| p.file_name() == Some(name.as_os_str()))
        .map(|(p, _)| *p)
        .collect();
    if let Some(found) = unique(by_name) {
        return found;
    }
    let folded = q.to_lowercase();
    let by_title = (notes.iter())
        .filter(|(_, t)| t.is_some_and(|t| t.trim().to_lowercase() == folded))
        .map(|(p, _)| *p)
        .collect();
    unique(by_title).unwrap_or_else(|| Err(format!("no note '{q}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds() {
        let (a, b, c, d) = (
            Path::new("work/release.md"),
            Path::new("groceries.md"),
            Path::new("home/release.md"),
            Path::new("todo.md"),
        );
        let notes = [
            (a, Some("Release checklist")),
            (b, Some("Groceries")),
            (c, None),
            (d, Some("groceries")),
        ];
        let dir = Path::new("/n");
        let f = |q: &str| find(dir, &notes, q);
        assert_eq!(f("work/release.md"), Ok(a));
        assert_eq!(f("work/release"), Ok(a));
        assert_eq!(f("/n/home/release.md"), Ok(c));
        assert_eq!(f("todo"), Ok(d));
        assert_eq!(f("release checklist"), Ok(a));
        // A name before a title.
        assert_eq!(f("groceries"), Ok(b));
        // Not unique.
        let e = f("release").unwrap_err();
        assert!(
            e.contains("work/release.md") && e.contains("home/release.md"),
            "{e}"
        );
        let e = f("GROCERIES ").unwrap_err();
        assert!(e.contains("could be any of"), "{e}");
        assert!(f("nope").unwrap_err().contains("no note"));
        assert!(f(" ").is_err());
    }
}
