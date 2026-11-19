//! Writing notes: atomically, so a reader (a sync tool, an editor, the
//! index) never sees half a file.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replace the file at `path` with `text`: written to a temporary file in
/// the same directory, synced, and renamed over it.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    // A dot file, which the index leaves out.
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Write a new note named `stem` (`.md` is added), or `stem-2`, `stem-3`
/// and so on if taken, in `dir`. Never replaces a file. Returns its path.
pub fn create_new(dir: &Path, stem: &str, text: &str) -> io::Result<PathBuf> {
    for n in 1.. {
        let name = match n {
            1 => format!("{stem}.md"),
            n => format!("{stem}-{n}.md"),
        };
        let path = dir.join(name);
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 1000 => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = file
            .write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        return Ok(path);
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes() {
        let dir = std::env::temp_dir().join(format!("wl-shell-notes-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = create_new(&dir, "2026-11-17-2059", "one").unwrap();
        let b = create_new(&dir, "2026-11-17-2059", "two").unwrap();
        assert_eq!(a.file_name().unwrap(), "2026-11-17-2059.md");
        assert_eq!(b.file_name().unwrap(), "2026-11-17-2059-2.md");
        write_atomic(&a, "three").unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "three");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two");
        // No temporary files left.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
