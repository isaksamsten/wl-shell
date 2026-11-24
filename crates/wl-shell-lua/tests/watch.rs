//! `dock.watch` end to end: real files, the inotify thread, a state.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::{Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source};

const WAIT: Duration = Duration::from_secs(5);

fn runtime() -> (Runtime, Receiver<Event>) {
    let (send, recv) = channel();
    let send = Mutex::new(send);
    let settings = Settings {
        threads: 2,
        ..Settings::default()
    };
    let rt = Runtime::new(settings, move |e| {
        let _ = send.lock().unwrap().send(e);
    })
    .unwrap();
    (rt, recv)
}

fn code(id: &str, code: &str) -> InstanceSpec {
    InstanceSpec {
        id: id.into(),
        plugin: id.into(),
        source: Source::Code {
            name: id.into(),
            code: Arc::from(code),
        },
        role: Role::Widget,
        app: None,
        options: serde_json::json!({}),
        budget: 1_000_000,
    }
}

fn next<T>(recv: &Receiver<Event>, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    let end = Instant::now() + WAIT;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let e = recv.recv_timeout(left).expect("no such event in time");
        if let Some(t) = pick(e) {
            return t;
        }
    }
}

fn loaded(recv: &Receiver<Event>, id: &str) {
    next(recv, |e| match e {
        Event::Loaded { instance, .. } if instance == id => Some(()),
        Event::Failed { instance, error } if instance == id => panic!("failed: {error}"),
        _ => None,
    })
}

fn label(rt: &Runtime, recv: &Receiver<Event>, id: &str) -> String {
    let ticket = rt.request(id, RequestKind::Label, WAIT).unwrap();
    next(recv, |e| match e {
        Event::Answer(a) if a.id == ticket.id => match a.result {
            Ok(Reply::Label(l)) => Some(l.unwrap_or_default()),
            r => panic!("{r:?}"),
        },
        _ => None,
    })
}

/// The calls the plugin heard since the last time: each a list of paths.
fn calls(rt: &Runtime, recv: &Receiver<Event>) -> Vec<Vec<String>> {
    next(recv, |e| matches!(e, Event::Redraw(i) if i == "a").then_some(()));
    let text = label(rt, recv, "a");
    (text.split(';').filter(|c| !c.is_empty()))
        .map(|c| c.split('|').map(str::to_string).collect())
        .collect()
}

/// Nothing heard for a while.
fn quiet(recv: &Receiver<Event>) {
    let end = Instant::now() + Duration::from_millis(500);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match recv.recv_timeout(left) {
            Ok(Event::Redraw(_)) => panic!("a call it shouldn't hear"),
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

/// A plugin that watches `path` with `opts`, and tells what it heard.
fn watcher(path: &Path, opts: &str) -> String {
    format!(
        "local log = {{}}
         return {{
           setup = function()
             assert(dock.watch({:?}, function(paths)
               log[#log + 1] = table.concat(paths, '|')
               dock.redraw()
             end, {opts}))
           end,
           label = function() local s = table.concat(log, ';'); log = {{}}; return s end,
         }}",
        path.to_string_lossy()
    )
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wl-shell-lua-watch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn a_file_created_written_and_renamed_over() {
    let dir = temp("file");
    let file = dir.join("cal.ics");
    let (rt, recv) = runtime();
    // It doesn't exist yet.
    rt.configure(vec![code("a", &watcher(&file, "nil"))]);
    loaded(&recv, "a");

    // Others in its folder aren't news.
    std::fs::write(dir.join("other"), "x").unwrap();
    quiet(&recv);

    // Created and written a few times: one call.
    for i in 0..3 {
        std::fs::write(&file, format!("v{i}")).unwrap();
    }
    assert_eq!(calls(&rt, &recv), [[s(&file)]]);

    // Replaced by a rename, as editors save.
    std::fs::write(dir.join("cal.ics.tmp"), "new").unwrap();
    std::fs::rename(dir.join("cal.ics.tmp"), &file).unwrap();
    assert_eq!(calls(&rt, &recv), [[s(&file)]]);

    // Removed.
    std::fs::remove_file(&file).unwrap();
    assert_eq!(calls(&rt, &recv), [[s(&file)]]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn folders_and_subfolders() {
    let dir = temp("tree");
    std::fs::create_dir_all(dir.join("old")).unwrap();
    std::fs::create_dir_all(dir.join(".hidden")).unwrap();
    let (rt, recv) = runtime();
    rt.configure(vec![code("a", &watcher(&dir, "{ recursive = true }"))]);
    loaded(&recv, "a");

    // A subfolder there from the start.
    let a = dir.join("old/a.ics");
    std::fs::write(&a, "x").unwrap();
    assert_eq!(calls(&rt, &recv), [[s(&a)]]);

    // A new folder, and what is made in it right away (maybe before its
    // watch): all of it, gathered.
    let new = dir.join("new");
    std::fs::create_dir(&new).unwrap();
    let b = new.join("b.ics");
    std::fs::write(&b, "x").unwrap();
    let heard: Vec<String> = calls(&rt, &recv).concat();
    assert!(heard.contains(&s(&new)) && heard.contains(&s(&b)), "{heard:?}");

    // And later.
    let c = new.join("c.ics");
    std::fs::write(&c, "x").unwrap();
    assert_eq!(calls(&rt, &recv), [[s(&c)]]);

    // Dot folders aren't watched (though their own entry is news).
    std::fs::write(dir.join(".hidden/x"), "x").unwrap();
    quiet(&recv);

    // Moved out of the tree: its folder goes with it.
    let out = temp("tree-out");
    std::fs::rename(&new, out.join("new")).unwrap();
    assert_eq!(calls(&rt, &recv), [[s(&new)]]);
    std::fs::write(out.join("new/d.ics"), "x").unwrap();
    quiet(&recv);
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(out).unwrap();
}

#[test]
fn not_recursive_sees_only_the_folder() {
    let dir = temp("flat");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let (rt, recv) = runtime();
    rt.configure(vec![code("a", &watcher(&dir, "nil"))]);
    loaded(&recv, "a");
    std::fs::write(dir.join("sub/x"), "x").unwrap();
    quiet(&recv);
    std::fs::write(dir.join("y"), "x").unwrap();
    assert_eq!(calls(&rt, &recv), [[s(&dir.join("y"))]]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn errors_limits_cancelling_and_reloading() {
    let dir = temp("limits");
    let (rt, recv) = runtime();
    let source = format!(
        "local dir = {:?}
         local hits = 0
         return {{
           setup = function()
             local none, err = dock.watch('/nonexistent/folder/file', function() end)
             assert(none == nil and err:find('/nonexistent/folder'), err)
             local handles = {{}}
             for i = 1, 256 do handles[i] = assert(dock.watch(dir, function() hits = hits + 1 end)) end
             assert(not pcall(dock.watch, dir, function() end))
             -- Cancelled, they make room, and are heard no more.
             for i = 1, 256 do handles[i]:cancel() end
             handles[1]:cancel()
             assert(dock.watch(dir, function() hits = hits + 1; dock.redraw() end))
           end,
           label = function() return tostring(hits) end,
         }}",
        s(&dir)
    );
    rt.configure(vec![code("a", &source)]);
    loaded(&recv, "a");
    std::fs::write(dir.join("x"), "x").unwrap();
    next(&recv, |e| matches!(e, Event::Redraw(i) if i == "a").then_some(()));
    assert_eq!(label(&rt, &recv, "a"), "1");

    // Gone with the state.
    rt.configure(vec![]);
    std::fs::write(dir.join("y"), "x").unwrap();
    quiet(&recv);
    std::fs::remove_dir_all(dir).unwrap();
}

