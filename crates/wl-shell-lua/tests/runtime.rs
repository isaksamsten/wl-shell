//! The runtime end to end: real Lua states on the pool.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::{
    AppInfo, Event, InstanceSpec, Pending, Reply, RequestKind, Role, Runtime, Settings, Source,
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
        local app = dock.app
        return {
          on_click = function(button) got = button end,
          on_app = function() got = 'renamed' end,
          label = function()
            if not app then return 'nil:' .. got end
            return app.id .. ':' .. tostring(app.desktop) .. ':' .. app.name .. ':' .. got
          end,
        }";
    let mut app = code("app", source);
    app.app = Some(AppInfo::new("foot"));
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
    assert_eq!(
        label(&rt, &recv, "app"),
        Ok(Some("foot:nil:foot:none".into()))
    );
    // The dock learns the app's other names: the same table changes, the
    // plugin hears of it and redraws.
    let names = AppInfo {
        desktop: Some("org.codeberg.dnkl.foot".into()),
        name: Some("Foot".into()),
        ..AppInfo::new("foot")
    };
    rt.set_app("app", names.clone());
    next(&recv, |e| {
        matches!(e, Event::Redraw(i) if i == "app").then_some(())
    });
    assert_eq!(
        label(&rt, &recv, "app"),
        Ok(Some("foot:org.codeberg.dnkl.foot:Foot:renamed".into()))
    );
    // The same names again: nothing.
    rt.set_app("app", names);
    assert!(recv.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn bundled_modules_and_the_clock() {
    let (send, recv) = channel();
    let send = Mutex::new(send);
    let settings = Settings {
        threads: 1,
        modules: &[("greet", "return { hi = function() return 'hi' end }")],
        ..Settings::default()
    };
    let rt = Runtime::new(settings, move |e| {
        let _ = send.lock().unwrap().send(e);
    })
    .unwrap();
    rt.configure(vec![code(
        "a",
        "local g = require 'greet'
         assert(require 'greet' == g)
         assert(not pcall(require, 'other'))
         return { label = function()
           local now = dock.now()
           assert(math.type(now) == 'float' and math.abs(now - os.time()) < 2)
           return g.hi()
         end }",
    )]);
    loaded(&recv, "a");
    assert_eq!(label(&rt, &recv, "a"), Ok(Some("hi".into())));
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
        "return { actions = { { id = 'x', run = function() end } }, setup = function() end }",
    );
    rt.configure(vec![w, l]);
    assert!(loaded(&recv, "l").launcher());
    assert!(label(&rt, &recv, "w").unwrap_err().contains("not running"));
}

/// The image-shrink and follow-logs examples, through the runtime: what
/// applies is found in Rust, choices come back with their own fields,
/// and `run` gets the subject and the choice.
#[test]
fn launcher_actions_choose_and_run() {
    use wl_shell_lua::{Choice, Registry, Subject};
    let (rt, recv) = runtime();
    let source = "
        return {
          actions = {
            { id = 'shrink', name = 'Shrink image', icon = 'image-x-generic',
              when = { kind = 'file', mime = 'image/*' },
              choices = function(s)
                return { { id = '50', name = '50%', factor = 0.5 }, { id = '25', name = '25%' },
                         { id = '50', name = 'again' } }
              end,
              run = function(s, choice)
                dock.clipboard(s.name .. ' ' .. choice.id .. ' ' .. tostring(choice.factor))
              end },
            { id = 'logs', name = 'Follow logs', icon = 'utilities-terminal',
              when = { kind = 'app', running = true },
              run = function(s, choice) dock.clipboard('logs ' .. s.id .. ' ' .. tostring(choice)) end },
            { id = 'stuck', when = { kind = 'file' },
              choices = function(s) while true do end end,
              run = function() end },
          },
        }";
    let spec = InstanceSpec {
        role: Role::Plugin,
        ..code("plugin:demo", source)
    };
    rt.configure(vec![spec]);
    let mut registry = Registry::default();
    registry.insert("plugin:demo", loaded(&recv, "plugin:demo"));
    let image = Subject::File {
        path: "/tmp/cat.png".into(),
        name: "cat.png".into(),
        dir: false,
        mime: vec!["image/png".into(), "application/octet-stream".into()],
    };
    let app = |running| Subject::App {
        id: "foot".into(),
        name: "Foot".into(),
        running,
        pinned: true,
        windows: Vec::new(),
    };
    let ids = |s: &Subject| {
        (registry.actions_for(s, |_| true).into_iter())
            .map(|(_, a)| a.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&image), ["shrink", "stuck"]);
    assert_eq!(ids(&app(true)), ["logs"]);
    assert!(ids(&app(false)).is_empty());

    let ask = |kind| {
        let ticket = rt.request("plugin:demo", kind, WAIT).unwrap();
        next(&recv, |e| match e {
            Event::Answer(a) if a.id == ticket.id => Some(a.result),
            _ => None,
        })
    };
    let Ok(Reply::Choices(choices)) = ask(RequestKind::Choices {
        action: "shrink".into(),
        subject: image.clone(),
    }) else {
        panic!("no choices");
    };
    // A repeated id is dropped.
    let names: Vec<&str> = choices.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["50%", "25%"]);
    let choice: Choice = choices[0].clone();
    let clipboard = |recv: &Receiver<Event>| {
        next(recv, |e| match e {
            Event::Clipboard { text, .. } => Some(text),
            _ => None,
        })
    };
    let run = RequestKind::Run {
        action: "shrink".into(),
        subject: image.clone(),
        choice: Some(choice),
    };
    rt.request("plugin:demo", run, WAIT).unwrap();
    assert_eq!(clipboard(&recv), "cat.png 50 0.5");
    let run = RequestKind::Run {
        action: "logs".into(),
        subject: app(true),
        choice: None,
    };
    rt.request("plugin:demo", run, WAIT).unwrap();
    assert_eq!(clipboard(&recv), "logs foot nil");
    // An unknown action is an error, not a crash.
    let e = ask(RequestKind::Choices {
        action: "nope".into(),
        subject: image.clone(),
    });
    assert!(e.unwrap_err().contains("no action"));
    // `choices` that never answers within the host's deadline: the
    // answer is late (dropped by `Pending`), and the state restarts.
    let mut pending = Pending::default();
    let ticket = rt
        .request(
            "plugin:demo",
            RequestKind::Choices {
                action: "stuck".into(),
                subject: image,
            },
            Duration::from_millis(500),
        )
        .unwrap();
    pending.insert("plugin:demo", ticket.clone());
    let answer = next(&recv, |e| match e {
        Event::Answer(a) if a.id == ticket.id => Some(a),
        _ => None,
    });
    assert!(answer.result.unwrap_err().contains("budget"));
    loaded(&recv, "plugin:demo");
}

/// The SSH and calculator examples, through the runtime: `items()` is a
/// list, `search` emits (at once, and later from a timer) with its
/// query's generation, `run` gets the item back, and `invalidate()`
/// says the items changed.
#[test]
fn launcher_sources_list_search_and_run() {
    use wl_shell_lua::{Item, ItemKind};
    let (rt, recv) = runtime();
    let source = r#"
        local config = 'Host alpha\n  User me\nHost beta *.example.com\nHost gamma\n'
        return {
          sources = {
            { id = 'ssh', name = 'SSH', trigger = '>', icon = 'network-server',
              items = function()
                local out = {}
                for host in ('\n' .. config):gmatch('\nHost%s+([^%s*]+)') do
                  out[#out + 1] = { id = host, title = host, icon = 'network-server' }
                end
                return out
              end,
              run = function(item) dock.clipboard('ssh ' .. item.id) end },
            { id = 'calc', name = 'Calculator', trigger = '=',
              search = function(query, emit)
                if query == 'later' then
                  dock.timer(10, function() emit{ { id = 'late', title = 'late' } } end)
                  return
                end
                local f = load('return ' .. query, 'calc', 't', { math = math })
                local ok, v = pcall(f or error)
                if ok and v then emit{ { id = 'result', title = tostring(v), subtitle = query } } end
              end,
              run = function(item) dock.clipboard(item.title) end },
            { id = 'files', trigger = "'", items = function()
                return { { kind = 'file', title = 'notes', path = '/tmp/notes.md' } }
              end },
          },
          actions = { { id = 'refresh', run = function()
            config = config .. 'Host delta\n'
            dock.invalidate()
          end } },
        }"#;
    let spec = InstanceSpec {
        role: Role::Plugin,
        ..code("plugin:demo", source)
    };
    rt.configure(vec![spec]);
    let caps = loaded(&recv, "plugin:demo");
    let triggers: Vec<char> = caps.sources.iter().map(|s| s.trigger).collect();
    assert_eq!(triggers, ['>', '=', '\'']);
    assert!(caps.sources[1].search && !caps.sources[0].search);
    let ask = |kind| {
        let ticket = rt.request("plugin:demo", kind, WAIT).unwrap();
        next(&recv, |e| match e {
            Event::Answer(a) if a.id == ticket.id => Some(a.result),
            _ => None,
        })
    };
    let items = |source: &str| match ask(RequestKind::Items {
        source: source.into(),
    }) {
        Ok(Reply::Items(items)) => items,
        r => panic!("no items: {r:?}"),
    };
    let hosts: Vec<String> = items("ssh").into_iter().map(|i| i.id).collect();
    assert_eq!(hosts, ["alpha", "beta", "gamma"]);
    let files = items("files");
    assert_eq!(
        files[0].kind,
        ItemKind::File {
            path: "/tmp/notes.md".into()
        }
    );
    let clipboard = |recv: &Receiver<Event>| {
        next(recv, |e| match e {
            Event::Clipboard { text, .. } => Some(text),
            _ => None,
        })
    };
    let open = |source: &str, item: Item| {
        let kind = RequestKind::Open {
            source: source.into(),
            item,
        };
        rt.request("plugin:demo", kind, WAIT).unwrap();
    };
    open("ssh", items("ssh").remove(1));
    assert_eq!(clipboard(&recv), "ssh beta");

    // A search emits with its generation.
    let emitted = |recv: &Receiver<Event>| {
        next(recv, |e| match e {
            Event::Emit {
                source,
                generation,
                items,
                ..
            } => Some((source, generation, items)),
            _ => None,
        })
    };
    let search = |query: &str, generation| {
        let kind = RequestKind::Search {
            source: "calc".into(),
            query: query.into(),
            generation,
        };
        rt.request("plugin:demo", kind, WAIT).unwrap();
    };
    search("2 * 21", 1);
    let (source, generation, results) = emitted(&recv);
    assert_eq!((source.as_str(), generation), ("calc", 1));
    assert_eq!(results[0].title, "42");
    assert_eq!(results[0].subtitle.as_deref(), Some("2 * 21"));
    open("calc", results[0].clone());
    assert_eq!(clipboard(&recv), "42");
    // Later, from a timer, still with its own generation.
    search("later", 2);
    search("1 + 1", 3);
    let mut got = [emitted(&recv), emitted(&recv)];
    got.sort_by_key(|(_, g, _)| *g);
    assert_eq!((got[0].1, got[0].2[0].title.as_str()), (2, "late"));
    assert_eq!((got[1].1, got[1].2[0].title.as_str()), (3, "2"));
    // An unknown source is an error, not a crash.
    let e = ask(RequestKind::Items {
        source: "nope".into(),
    });
    assert!(e.unwrap_err().contains("no source"));

    // `invalidate()`: the items are asked for again, and have changed.
    let kind = RequestKind::Run {
        action: "refresh".into(),
        subject: files[0].subject("files"),
        choice: None,
    };
    rt.request("plugin:demo", kind, WAIT).unwrap();
    next(&recv, |e| match e {
        Event::Invalidate(i) if i == "plugin:demo" => Some(()),
        _ => None,
    });
    assert_eq!(items("ssh").len(), 4);
}
