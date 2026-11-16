//! The launcher's file search: a walk of the disk feeding a matcher, both
//! off the main loop. There is no index: each search walks anew while the
//! launcher is open, and the best matches stream onto the drum.
//!
//! - The walk is the `ignore` crate's parallel walker on a few threads of
//!   its own. `exclude` globs (gitignore syntax, relative to the root)
//!   become overrides, so excluded directories are never entered; `.git`
//!   never is. Hidden entries are skipped unless `hidden`; `.gitignore`,
//!   `.ignore` and git's excludes are honored if `gitignore`. Symlinks
//!   aren't followed, the walk stays on the root's file system, and it
//!   stops after `max` entries. Directories are results too, and are
//!   still descended into. Paths that aren't UTF-8 are left out.
//! - Every entry is pushed into a `nucleo::Nucleo` through its injector.
//!   The one column is the path relative to the root, which is all an
//!   item holds (with whether it is a directory). Matching uses
//!   `match_paths()` and ignores case and accents; the query is nucleo's
//!   pattern syntax (`^`, `$`, `!`, `'`, atoms split by spaces).
//! - Nucleo's notify (new items, or a match pass done) and the walk's end
//!   call `notify`; the caller then `tick`s, which never blocks, and takes
//!   the `top` matches.
//! - Dropping it cancels the walk (its visitors quit on a flag) and frees
//!   the items once the walk's threads let go of them.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ignore::overrides::{Override, OverrideBuilder};
use ignore::{WalkBuilder, WalkState};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Matcher, Nucleo, Utf32Str};

/// Threads for the walk, and for matching, at most.
const MAX_THREADS: usize = 4;
/// Matches past the top looked through for the kept selection, at most.
const KEEP_SCAN: u32 = 20_000;

/// What to walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub root: PathBuf,
    /// Gitignore-style globs, relative to the root.
    pub exclude: Vec<String>,
    pub hidden: bool,
    pub gitignore: bool,
    /// Entries walked at most.
    pub max: usize,
}

/// An entry the walk found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its path relative to the root.
    pub rel: Box<str>,
    pub dir: bool,
}

/// A walk on its own threads.
pub struct Walk {
    cancel: Arc<AtomicBool>,
    seen: Arc<AtomicUsize>,
    done: Arc<AtomicBool>,
    max: usize,
}

/// The overrides for `exclude` (and `.git`) under `root`.
fn overrides(root: &Path, exclude: &[String]) -> Result<Override, String> {
    let mut builder = OverrideBuilder::new(root);
    for glob in exclude.iter().map(|g| g.trim()).filter(|g| !g.is_empty()) {
        // Overrides whitelist; `!` makes a glob ignore instead.
        let glob = format!("!{}", glob.strip_prefix('!').unwrap_or(glob));
        builder
            .add(&glob)
            .map_err(|e| format!("files exclude {glob:?}: {e}"))?;
    }
    builder.add("!.git").map_err(|e| e.to_string())?;
    builder.build().map_err(|e| e.to_string())
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().min(MAX_THREADS))
}

impl Walk {
    /// Walk `settings.root`, giving every entry to `sink`, and calling
    /// `done` once it ends (finished, stopped at `max`, or cancelled).
    pub fn start(
        settings: &Settings,
        sink: impl Fn(Entry) + Send + Sync + 'static,
        done: impl FnOnce() + Send + 'static,
    ) -> Result<Self, String> {
        let gitignore = settings.gitignore;
        let mut builder = WalkBuilder::new(&settings.root);
        builder
            .hidden(!settings.hidden)
            .ignore(gitignore)
            .git_ignore(gitignore)
            .git_global(gitignore)
            .git_exclude(gitignore)
            .parents(gitignore)
            .follow_links(false)
            .same_file_system(true)
            .overrides(overrides(&settings.root, &settings.exclude)?)
            .threads(threads());
        let walker = builder.build_parallel();
        let walk = Self {
            cancel: Arc::default(),
            seen: Arc::default(),
            done: Arc::default(),
            max: settings.max,
        };
        let (cancel, seen, finished) = (walk.cancel.clone(), walk.seen.clone(), walk.done.clone());
        let (root, max) = (settings.root.clone(), settings.max);
        std::thread::Builder::new()
            .name("wl-dock-files".into())
            .spawn(move || {
                let sink = &sink;
                walker.run(|| {
                    let (cancel, seen, root) = (cancel.clone(), seen.clone(), root.clone());
                    Box::new(move |result| {
                        if cancel.load(Ordering::Relaxed) {
                            return WalkState::Quit;
                        }
                        let Ok(entry) = result else {
                            return WalkState::Continue;
                        };
                        if entry.depth() == 0 {
                            return WalkState::Continue;
                        }
                        if seen.fetch_add(1, Ordering::Relaxed) >= max {
                            return WalkState::Quit;
                        }
                        let rel = entry.path().strip_prefix(&root).ok().and_then(Path::to_str);
                        if let Some(rel) = rel.filter(|r| !r.is_empty()) {
                            let dir = entry.file_type().is_some_and(|t| t.is_dir());
                            sink(Entry {
                                rel: rel.into(),
                                dir,
                            });
                        }
                        WalkState::Continue
                    })
                });
                finished.store(true, Ordering::Release);
                done();
            })
            .map_err(|e| format!("cannot start the file search: {e}"))?;
        Ok(walk)
    }

    /// Stop it; its threads quit at their next entry.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Entries walked so far.
    pub fn seen(&self) -> usize {
        self.seen.load(Ordering::Relaxed).min(self.max)
    }

    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Its path relative to the root.
    pub rel: String,
    pub dir: bool,
}

/// A search: the walk, and the matcher it feeds.
pub struct FileSearch {
    // The walk goes first, so it is cancelled before the matcher goes.
    walk: Walk,
    nucleo: Nucleo<Entry>,
    root: PathBuf,
    query: String,
    matcher: Matcher,
    buf: Vec<char>,
}

impl FileSearch {
    /// Start walking `settings.root`; `notify` is called (from other
    /// threads) whenever there is something new to `tick` for.
    pub fn start(settings: &Settings, notify: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let config = Config::DEFAULT.match_paths();
        let nucleo = Nucleo::new(config.clone(), notify.clone(), Some(threads()), 1);
        let injector = nucleo.injector();
        let walk = Walk::start(
            settings,
            move |entry| {
                injector.push(entry, |e, cols| cols[0] = e.rel.as_ref().into());
            },
            move || notify(),
        )?;
        Ok(Self {
            walk,
            nucleo,
            root: settings.root.clone(),
            query: String::new(),
            matcher: Matcher::new(config),
            buf: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Match `query` from now on. Appending to the last query only
    /// rematches its matches.
    pub fn set_query(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        let append = query.starts_with(&self.query) && !self.query.ends_with('\\');
        (self.nucleo.pattern).reparse(0, query, CaseMatching::Ignore, Normalization::Smart, append);
        self.query = query.to_string();
    }

    /// Take in what the matcher has; never blocks. Returns whether the
    /// matches changed.
    pub fn tick(&mut self) -> bool {
        self.nucleo.tick(0).changed
    }

    /// The best `n` matches. `keep` (a relative path) stays among them if
    /// it matches at all: it takes the last place if it isn't in the top.
    pub fn top(&self, n: usize, keep: Option<&str>) -> Vec<Found> {
        let snapshot = self.nucleo.snapshot();
        let count = snapshot.matched_item_count();
        let n = (n as u32).min(count);
        let found = |item: nucleo::Item<'_, Entry>| Found {
            rel: item.data.rel.to_string(),
            dir: item.data.dir,
        };
        let mut top: Vec<Found> = snapshot.matched_items(..n).map(found).collect();
        if let Some(keep) = keep
            && n > 0
            && !top.iter().any(|f| f.rel == keep)
        {
            let end = count.min(n.saturating_add(KEEP_SCAN));
            let kept = (snapshot.matched_items(n..end)).find(|i| &*i.data.rel == keep);
            if let (Some(kept), Some(last)) = (kept, top.last_mut()) {
                *last = found(kept);
            }
        }
        top
    }

    /// The parts of `rel` the query matches, as byte ranges, in order.
    pub fn highlight(&mut self, rel: &str) -> Vec<Range<usize>> {
        let chars: Vec<(usize, char)> = rel.char_indices().collect();
        self.buf.clear();
        self.buf.extend(chars.iter().map(|(_, c)| *c));
        let mut indices = Vec::new();
        let pattern = self.nucleo.pattern.column_pattern(0);
        pattern.indices(
            Utf32Str::Unicode(&self.buf),
            &mut self.matcher,
            &mut indices,
        );
        indices.sort_unstable();
        indices.dedup();
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for i in indices {
            let Some(&(start, c)) = chars.get(i as usize) else {
                continue;
            };
            let end = start + c.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => ranges.push(start..end),
            }
        }
        ranges
    }

    /// Entries walked so far.
    pub fn seen(&self) -> usize {
        self.walk.seen()
    }

    /// The walk is over.
    pub fn walked(&self) -> bool {
        self.walk.done()
    }

    /// Everything walked has been matched.
    pub fn settled(&self) -> bool {
        self.walk.done() && self.nucleo.snapshot().item_count() as usize >= self.walk.seen()
    }

    /// The matches are the query's, not an earlier one's (matching a new
    /// query takes a moment).
    pub fn current(&self) -> bool {
        let shown = self.nucleo.snapshot().pattern().column_pattern(0);
        shown.atoms == self.nucleo.pattern.column_pattern(0).atoms
    }

    /// Matches in the last snapshot.
    pub fn matched(&self) -> usize {
        self.nucleo.snapshot().matched_item_count() as usize
    }
}

/// `path` for showing: `~` for the home directory.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        return match rest.as_os_str().is_empty() {
            true => "~".into(),
            false => format!("~/{}", rest.display()),
        };
    }
    path.display().to_string()
}

/// `path` (with `matched`, byte ranges of it) in at most `max` characters
/// if it can be: the directories between its first component and its name
/// that the query didn't match go, from the left, each run of them
/// becoming one `…`. Returns the path and its matched ranges.
pub fn fit_path(path: &str, matched: &[Range<usize>], max: usize) -> (String, Vec<Range<usize>>) {
    let mut comps = Vec::new();
    let mut start = 0;
    for (i, c) in path.char_indices() {
        if c == '/' {
            comps.push(start..i);
            start = i + 1;
        }
    }
    comps.push(start..path.len());
    let hit = |r: &Range<usize>| matched.iter().any(|m| m.start < r.end && r.start < m.end);
    let mut keep = vec![true; comps.len()];
    for i in 1..comps.len().saturating_sub(1) {
        if joined(path, &comps, &keep, matched).0.chars().count() <= max {
            break;
        }
        if !hit(&comps[i]) {
            keep[i] = false;
        }
    }
    joined(path, &comps, &keep, matched)
}

/// The `keep` components of `path` joined, with `…` for each run of the
/// others, and `matched` moved along.
fn joined(
    path: &str,
    comps: &[Range<usize>],
    keep: &[bool],
    matched: &[Range<usize>],
) -> (String, Vec<Range<usize>>) {
    let mut out = String::new();
    let mut ranges = Vec::new();
    let mut elided = false;
    for (i, r) in comps.iter().enumerate() {
        if !keep[i] {
            if !elided {
                out.push_str("/…");
                elided = true;
            }
            continue;
        }
        elided = false;
        if i > 0 {
            out.push('/');
        }
        let at = out.len();
        out.push_str(&path[r.clone()]);
        for m in matched {
            let (s, e) = (m.start.max(r.start), m.end.min(r.end));
            if s < e {
                ranges.push(s - r.start + at..e - r.start + at);
            }
        }
    }
    (out, ranges)
}

/// A match's path relative to the root, in the parts the launcher shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parts<'a> {
    /// The file name, and the parts of it the query matched.
    pub name: &'a str,
    pub name_matched: Vec<Range<usize>>,
    /// The parent directory's own name (empty at the root), and the parts
    /// of it the query matched.
    pub parent: &'a str,
    pub parent_matched: Vec<Range<usize>>,
}

/// `rel` in parts, with `matched` (byte ranges of `rel`) split between
/// its name and its parent's.
pub fn parts<'a>(rel: &'a str, matched: &[Range<usize>]) -> Parts<'a> {
    let (dir, name_at) = match rel.rfind('/') {
        Some(i) => (&rel[..i], i + 1),
        None => ("", 0),
    };
    let parent_at = dir.rfind('/').map_or(0, |i| i + 1);
    let within = |start: usize, end: usize| -> Vec<Range<usize>> {
        (matched.iter())
            .map(|r| r.start.max(start)..r.end.min(end))
            .filter(|r| r.start < r.end)
            .map(|r| r.start - start..r.end - start)
            .collect()
    };
    Parts {
        name: &rel[name_at..],
        name_matched: within(name_at, rel.len()),
        parent: &dir[parent_at..],
        parent_matched: within(parent_at, dir.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("wl-dock-filesearch-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, rel: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, rel).unwrap();
        }

        fn settings(&self) -> Settings {
            Settings {
                root: self.0.clone(),
                exclude: vec!["node_modules".into(), "target".into()],
                hidden: false,
                gitignore: true,
                max: 1_000_000,
            }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Everything a walk finds, sorted.
    fn walk(settings: &Settings) -> Vec<String> {
        let found = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        let sink = found.clone();
        let w = Walk::start(
            settings,
            move |e| {
                let suffix = if e.dir { "/" } else { "" };
                sink.lock().unwrap().push(format!("{}{suffix}", e.rel));
            },
            move || tx.send(()).unwrap(),
        )
        .unwrap();
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(w.done());
        let mut found = found.lock().unwrap().clone();
        found.sort();
        found
    }

    #[test]
    fn filters() {
        let t = Tree::new("filters");
        for f in [
            "docs/plan.md",
            "node_modules/x/index.js",
            "a/node_modules/y.js",
            "target/debug/bin",
            ".hidden/secret",
            ".dotfile",
            "repo/.gitignore",
            "repo/build/out.o",
            "repo/src/main.rs",
            "repo/notes.log",
        ] {
            t.file(f);
        }
        std::fs::write(t.0.join("repo/.gitignore"), "build/\n*.log\n").unwrap();
        std::fs::create_dir_all(t.0.join("repo/.git")).unwrap();
        let mut s = t.settings();
        assert_eq!(
            walk(&s),
            [
                "a/",
                "docs/",
                "docs/plan.md",
                "repo/",
                "repo/src/",
                "repo/src/main.rs"
            ]
        );
        // Hidden entries, but never `.git`.
        s.hidden = true;
        let all = walk(&s);
        assert!(all.contains(&".dotfile".to_string()));
        assert!(all.contains(&".hidden/secret".to_string()));
        assert!(all.contains(&"repo/.gitignore".to_string()));
        assert!(!all.iter().any(|p| p.contains(".git/")));
        // Without the ignore files.
        s.hidden = false;
        s.gitignore = false;
        let all = walk(&s);
        assert!(all.contains(&"repo/build/out.o".to_string()));
        assert!(all.contains(&"repo/notes.log".to_string()));
        // No excludes.
        s.exclude.clear();
        let all = walk(&s);
        assert!(all.contains(&"a/node_modules/y.js".to_string()));
        assert!(all.contains(&"target/debug/bin".to_string()));
        // Globs with slashes are relative to the root.
        s.exclude = vec!["/a".into(), "docs/*.md".into()];
        let all = walk(&s);
        assert!(!all.iter().any(|p| p.starts_with("a/")));
        assert!(all.contains(&"docs/".to_string()));
        assert!(!all.contains(&"docs/plan.md".to_string()));
        assert!(all.contains(&"node_modules/x/index.js".to_string()));
        // A broken glob.
        s.exclude = vec!["a[".into()];
        assert!(Walk::start(&s, |_| {}, || {}).is_err());
    }

    #[test]
    fn symlinks_are_not_followed() {
        let t = Tree::new("links");
        t.file("real/inside.txt");
        std::os::unix::fs::symlink(t.0.join("real"), t.0.join("link")).unwrap();
        // A loop doesn't trap it either.
        std::os::unix::fs::symlink(&t.0, t.0.join("real/up")).unwrap();
        let all = walk(&t.settings());
        assert!(all.contains(&"real/inside.txt".to_string()));
        assert!(all.contains(&"link".to_string()));
        assert!(!all.iter().any(|p| p.starts_with("link/")));
        assert!(!all.iter().any(|p| p.starts_with("real/up/")));
    }

    #[test]
    fn stops_at_max() {
        let t = Tree::new("max");
        for i in 0..50 {
            t.file(&format!("d{}/f{i}", i % 5));
        }
        let mut s = t.settings();
        s.max = 20;
        assert_eq!(walk(&s).len(), 20);
        s.max = 1000;
        assert_eq!(walk(&s).len(), 55);
    }

    #[test]
    fn cancelling_stops_the_walk() {
        let t = Tree::new("cancel");
        for i in 0..2000 {
            t.file(&format!("d{}/f{i}", i % 40));
        }
        let count = Arc::new(AtomicUsize::new(0));
        let walk: Arc<Mutex<Option<Walk>>> = Arc::default();
        let (tx, rx) = mpsc::channel();
        let (sink_count, sink_walk) = (count.clone(), walk.clone());
        let w = Walk::start(
            &t.settings(),
            move |_| {
                if sink_count.fetch_add(1, Ordering::Relaxed) == 10 {
                    // Cancel from inside, once it is under way.
                    while sink_walk.lock().unwrap().is_none() {
                        std::thread::yield_now();
                    }
                    if let Some(w) = &*sink_walk.lock().unwrap() {
                        w.cancel();
                    }
                }
            },
            move || tx.send(()).unwrap(),
        )
        .unwrap();
        *walk.lock().unwrap() = Some(w);
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let n = count.load(Ordering::Relaxed);
        assert!(n < 500, "{n}");
    }

    /// The search's matches for `query`, once everything is matched.
    fn search(s: &mut FileSearch, query: &str) -> Vec<String> {
        s.set_query(query);
        for _ in 0..1000 {
            let changed = s.tick();
            if s.settled() && !changed && !s.nucleo.tick(0).running {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        s.top(5, None).into_iter().map(|f| f.rel).collect()
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn matching() {
        let t = Tree::new("match");
        for f in [
            "projects/wl-shell/apps/wl-dock/PLAN.md",
            "projects/wl-shell/README.md",
            "planets/mars.txt",
            "Documents/Éclair.pdf",
            "Documents/file.pdf",
            "Documents/profile.pdf",
        ] {
            t.file(f);
        }
        let mut s = FileSearch::start(&t.settings(), Arc::new(|| {})).unwrap();
        assert_eq!(s.root(), t.0);
        // Matches at the start of a component; every atom has to match.
        // (Equal scores keep the walk's order.)
        let plan = "projects/wl-shell/apps/wl-dock/PLAN.md";
        let mut found = search(&mut s, "plan");
        found.sort();
        assert_eq!(found, ["planets", "planets/mars.txt", plan]);
        assert_eq!(search(&mut s, "dock plan"), [plan]);
        // Accents and case.
        assert_eq!(search(&mut s, "eclair"), ["Documents/Éclair.pdf"]);
        // An exact atom (a substring, not a subsequence), and excluded ones.
        let mut exact = search(&mut s, "'file.pdf");
        exact.sort();
        assert_eq!(exact, ["Documents/file.pdf", "Documents/profile.pdf"]);
        let pdfs = search(&mut s, "pdf$ !clair !prof");
        assert_eq!(pdfs, ["Documents/file.pdf"]);
        // Directories are results.
        assert!(search(&mut s, "planets").contains(&"planets".to_string()));
        assert!(search(&mut s, "zzzz").is_empty());
        // What the query matched.
        search(&mut s, "mars");
        assert_eq!(s.highlight("planets/mars.txt"), [8..12]);
        search(&mut s, "ecl");
        assert_eq!(s.highlight("Documents/Éclair.pdf"), [10..14]);
        // A kept match stays among the top, even if it isn't in it.
        search(&mut s, "pdf");
        let top = s.top(1, Some("Documents/profile.pdf"));
        assert_eq!(top[0].rel, "Documents/profile.pdf");
        assert_eq!(s.top(1, Some("nope")).len(), 1);
        assert!(s.matched() >= 3 && s.seen() == 12 && s.walked());
        // Until a new query is matched, the matches are the last one's.
        assert!(s.current());
        s.set_query("pdf x");
        assert!(!s.current());
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn display() {
        let home = Path::new("/home/u");
        assert_eq!(tilde(Path::new("/home/u"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/home/u/a/b"), Some(home)), "~/a/b");
        assert_eq!(tilde(Path::new("/home/user"), Some(home)), "/home/user");
        assert_eq!(tilde(Path::new("/srv"), None), "/srv");
        let path = "~/projects/wl-shell/apps/wl-dock/PLAN.md";
        let fit = |m: &[Range<usize>], max| fit_path(path, m, max);
        assert_eq!(fit(&[], 100), (path.to_string(), vec![]));
        assert_eq!(fit(&[], 30).0, "~/…/apps/wl-dock/PLAN.md");
        assert_eq!(fit(&[], 10).0, "~/…/PLAN.md");
        // Matched directories stay, and the ranges move with them.
        let (out, m) = fit(&[14..16, 33..37], 10);
        assert_eq!(out, "~/…/wl-shell/…/PLAN.md");
        assert_eq!((&out[m[0].clone()], &out[m[1].clone()]), ("sh", "PLAN"));
        assert_eq!(fit_path("/a/b/c", &[], 1).0, "/…/c");
        assert_eq!(fit_path("x", &[0..1], 0), ("x".to_string(), vec![0..1]));
        let p = parts("a/bc/def.txt", &[0..1, 3..6, 8..9]);
        assert_eq!((p.name, p.parent), ("def.txt", "bc"));
        assert_eq!(
            (p.name_matched, p.parent_matched),
            (vec![0..1, 3..4], vec![1..2])
        );
        let p = parts("x.txt", &[0..1]);
        assert_eq!((p.name, p.parent), ("x.txt", ""));
        assert_eq!((p.name_matched, p.parent_matched), (vec![0..1], vec![]));
    }
}
