//! The runtime end to end: real Lua states on the pool.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::{
    Event, InstanceSpec, Pending, Reply, RequestKind, Role, Runtime, Settings, Source,
};

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

/// The next event `pick` takes, skipping others.
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

fn loaded(recv: &Receiver<Event>, id: &str) -> wl_shell_lua::Capabilities {
    next(recv, |e| match e {
        Event::Loaded { instance, caps } if instance == id => Some(caps),
        Event::Failed { instance, error } if instance == id => panic!("failed: {error}"),
        _ => None,
    })
}

fn failed(recv: &Receiver<Event>, id: &str) -> String {
    next(recv, |e| match e {
        Event::Failed { instance, error } if instance == id => Some(error),
        _ => None,
    })
}

fn label(rt: &Runtime, recv: &Receiver<Event>, id: &str) -> Result<Option<String>, String> {
    let ticket = rt.request(id, RequestKind::Label, WAIT).unwrap();
    next(recv, |e| match e {
        Event::Answer(a) if a.id == ticket.id => Some(a.result.map(|r| match r {
            Reply::Label(l) => l,
            _ => unreachable!(),
        })),
        _ => None,
    })
}

#[test]
fn popup_handlers_run_on_the_instance() {
    let (rt, recv) = runtime();
    rt.configure(vec![code("p", "local n = 0; return { popup = function() return dock.ui.button{key='btn', label='Next', on_click=function() n=n+1 end} end, label=function() return n end }")]);
    assert!(loaded(&recv, "p").widget.unwrap().popup);
    let request = |kind| {
        let ticket = rt.request("p", kind, WAIT).unwrap();
        next(&recv, |e| match e {
            Event::Answer(a) if a.id == ticket.id => Some(a.result.unwrap()),
            _ => None,
        })
    };
    assert!(matches!(request(RequestKind::Popup), Reply::Popup(Some(_))));
    assert_eq!(
        request(RequestKind::Interaction {
            key: "btn".into(),
            event: wl_shell_lua::channel::PopupEvent::Click
        }),
        Reply::Done
    );
    assert_eq!(label(&rt, &recv, "p").unwrap().as_deref(), Some("1"));
}

#[test]
fn item_clicks_and_the_app() {
    let (rt, recv) = runtime();
    let source = "local got = 'none'
        return {
          on_click = function(button) got = button end,
          label = function() return tostring(dock.app) .. ':' .. got end,
        }";
    let mut app = code("app", source);
    app.app = Some("foot".into());
    rt.configure(vec![code("w", source), app]);
    // Either may load first.
    let mut waiting = vec!["w", "app"];
    while !waiting.is_empty() {
        let (id, caps) = next(&recv, |e| match e {
            Event::Loaded { instance, caps } => Some((instance, caps)),
            _ => None,
        });
        assert!(caps.widget.unwrap().on_click);
        waiting.retain(|w| *w != id);
    }
    let ticket = rt
        .request("w", RequestKind::Click("middle".into()), WAIT)
        .unwrap();
    let done = next(&recv, |e| match e {
        Event::Answer(a) if a.id == ticket.id => Some(a.result),
        _ => None,
    });
    assert_eq!(done, Ok(Reply::Done));
    assert_eq!(label(&rt, &recv, "w"), Ok(Some("nil:middle".into())));
    assert_eq!(label(&rt, &recv, "app"), Ok(Some("foot:none".into())));
}

#[test]
fn a_label_with_options() {
    let (rt, recv) = runtime();
    let mut spec = code(
        "a",
        "local name
         return { setup = function(o) name = o.name end, label = function() return name end }",
    );
    spec.options = serde_json::json!({ "name": "Hello" });
    rt.configure(vec![spec]);
    assert!(loaded(&recv, "a").widget.unwrap().label);
    assert_eq!(label(&rt, &recv, "a"), Ok(Some("Hello".into())));
}

#[test]
fn timers_redraw() {
    let (rt, recv) = runtime();
    rt.configure(vec![code(
        "a",
        "local n = 0
         return {
           setup = function()
             local t = dock.every(20, function() n = n + 1; dock.redraw() end)
             dock.timer(10, function() end):cancel()
           end,
           label = function() return tostring(n) end,
         }",
    )]);
    loaded(&recv, "a");
    for _ in 0..3 {
        next(&recv, |e| {
            matches!(e, Event::Redraw(i) if i == "a").then_some(())
        });
    }
    let n: u32 = label(&rt, &recv, "a").unwrap().unwrap().parse().unwrap();
    assert!(n >= 3);
}

#[test]
fn spawned_lines_and_exit() {
    let (rt, recv) = runtime();
    rt.configure(vec![code(
        "a",
        "local lines, code = {}, nil
         return {
           setup = function()
             dock.spawn({'sh', '-c', 'echo one; echo two; exit 3'}, {
               on_line = function(l) lines[#lines + 1] = l end,
               on_exit = function(c) code = c; dock.redraw() end,
             })
           end,
           label = function() return table.concat(lines, ',') .. ':' .. tostring(code) end,
         }",
    )]);
    loaded(&recv, "a");
    next(&recv, |e| matches!(e, Event::Redraw(_)).then_some(()));
    assert_eq!(label(&rt, &recv, "a"), Ok(Some("one,two:3".into())));
}

#[test]
fn json_read_and_clipboard() {
    let (rt, recv) = runtime();
    let file = std::env::temp_dir().join(format!("wl-shell-lua-read-{}", std::process::id()));
    std::fs::write(&file, "text").unwrap();
    let code_ = format!(
        "return {{ label = function()
           local v = dock.json.decode('{{\"a\": [1, 2], \"b\": null}}')
           assert(v.a[2] == 2 and v.b == nil)
           local s = dock.json.encode({{ x = 'y' }})
           local t = dock.read({:?})
           local none, err = dock.read('/nonexistent/file')
           assert(none == nil and err)
           dock.clipboard('copied')
           return s .. t
         end }}",
        file.to_string_lossy()
    );
    rt.configure(vec![code("a", &code_)]);
    loaded(&recv, "a");
    let ticket = rt.request("a", RequestKind::Label, WAIT).unwrap();
    let mut copied = None;
    let answer = next(&recv, |e| match e {
        Event::Clipboard { text, .. } => {
            copied = Some(text);
            None
        }
        Event::Answer(a) if a.id == ticket.id => Some(a.result),
        _ => None,
    });
    assert_eq!(answer, Ok(Reply::Label(Some("{\"x\":\"y\"}text".into()))));
    assert_eq!(copied.as_deref(), Some("copied"));
    std::fs::remove_file(file).unwrap();
}

#[test]
fn the_sandbox() {
    let (rt, recv) = runtime();
    rt.configure(vec![code(
        "a",
        "return { label = function()
           assert(io == nil and debug == nil and package == nil)
           assert(os.execute == nil and os.remove == nil and os.exit == nil)
           assert(dofile == nil and loadfile == nil and string.dump == nil)
           assert(type(os.date('%Y')) == 'string' and type(os.time()) == 'number')
           assert(load('return 1 + 1')() == 2)
           assert(load('return x', 'c', 't', { x = 5 })() == 5)
           assert(not pcall(require, 'foo'))
           assert(not pcall(require, '../etc'))
           return 'ok'
         end }",
    )]);
    loaded(&recv, "a");
    assert_eq!(label(&rt, &recv, "a"), Ok(Some("ok".into())));
}

#[test]
fn a_hook_error_keeps_the_state() {
    let (rt, recv) = runtime();
    rt.configure(vec![code(
        "a",
        "local n = 0
         return { label = function() n = n + 1; if n == 1 then error('boom') end return 'x' .. n end }",
    )]);
    loaded(&recv, "a");
    assert!(label(&rt, &recv, "a").unwrap_err().contains("boom"));
    assert_eq!(label(&rt, &recv, "a"), Ok(Some("x2".into())));
}

#[test]
fn an_endless_loop_hits_the_budget_and_restarts() {
    let (rt, recv) = runtime();
    let mut spec = code(
        "a",
        "return { label = function() while true do pcall(function() while true do end end) end end }",
    );
    spec.budget = 100_000;
    rt.configure(vec![spec.clone()]);
    loaded(&recv, "a");
    rt.configure(vec![
        spec,
        code("b", "return { label = function() return 'b' end }"),
    ]);
    loaded(&recv, "b");
    let started = Instant::now();
    let err = label(&rt, &recv, "a").unwrap_err();
    assert!(err.contains("budget"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
    // (It said Failed before the answer.) Not running until it restarts.
    assert!(label(&rt, &recv, "a").unwrap_err().contains("not running"));
    // The others go on, and it comes back after the backoff.
    assert_eq!(label(&rt, &recv, "b"), Ok(Some("b".into())));
    loaded(&recv, "a");
}

#[test]
fn a_late_answer_is_dropped() {
    let (rt, recv) = runtime();
    // Busy for a while (under its budget), then answers.
    let mut spec = code(
        "a",
        "return { label = function()
           local t = os.clock() + 0.2
           while os.clock() < t do end
           return 'late'
         end }",
    );
    spec.budget = u64::MAX / 2;
    rt.configure(vec![spec]);
    loaded(&recv, "a");
    let mut pending = Pending::default();
    let ticket = rt
        .request("a", RequestKind::Label, Duration::from_millis(50))
        .unwrap();
    pending.insert("a", ticket.clone());
    // A request that is already late isn't run at all: only one answer.
    let skipped = rt
        .request("a", RequestKind::Label, Duration::from_millis(50))
        .unwrap();
    let answer = next(&recv, |e| match e {
        Event::Answer(a) => Some(a),
        _ => None,
    });
    assert_eq!(answer.id, ticket.id);
    assert_eq!(answer.result, Ok(Reply::Label(Some("late".into()))));
    assert!(Instant::now() > ticket.deadline);
    assert!(pending.accept(answer, Instant::now()).is_none());
    assert!(recv.recv_timeout(Duration::from_millis(300)).map_or(
        true,
        |e| !matches!(e, Event::Answer(a) if a.id == skipped.id)
    ));
}

#[test]
fn compile_errors_wait_for_a_reload() {
    let (rt, recv) = runtime();
    let dir = std::env::temp_dir().join(format!("wl-shell-lua-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("p.lua");
    std::fs::write(&path, "return {").unwrap();
    std::fs::write(dir.join("helper.lua"), "return { v = 'mod' }").unwrap();
    let spec = InstanceSpec {
        source: Source::File(path.clone()),
        ..code("a", "")
    };
    rt.configure(vec![spec.clone()]);
    failed(&recv, "a");
    std::fs::write(
        &path,
        "local h = require('helper')
         return { label = function() return h.v end }",
    )
    .unwrap();
    rt.reload(&path);
    loaded(&recv, "a");
    assert_eq!(label(&rt, &recv, "a"), Ok(Some("mod".into())));
    // Configuring the same spec again changes nothing; gone, it stops.
    rt.configure(vec![spec]);
    rt.configure(vec![]);
    assert!(rt.instances().is_empty());
    assert!(rt.request("a", RequestKind::Label, WAIT).is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plugin_tables_only_keep_launcher_states() {
    let (rt, recv) = runtime();
    let spec = |id: &str, code_: &str| InstanceSpec {
        role: Role::Plugin,
        ..code(id, code_)
    };
    let w = spec(
        "w",
        "return { setup = function() error('not set up') end, label = function() return 'w' end }",
    );
    rt.configure(vec![w.clone()]);
    assert!(loaded(&recv, "w").widget.is_some());
    let l = spec(
        "l",
        "return { actions = { { id = 'x' } }, setup = function() end }",
    );
    rt.configure(vec![w, l]);
    assert!(loaded(&recv, "l").launcher());
    assert!(label(&rt, &recv, "w").unwrap_err().contains("not running"));
}
