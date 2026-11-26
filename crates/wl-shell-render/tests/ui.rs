//! Golden images of the UI painter: every container and component, at
//! scales 1 and 2. A missing golden is written (and the test passes);
//! `WL_SHELL_BLESS=1` rewrites them all. A mismatch writes
//! `<name>.actual.png` next to the golden. Text depends on the system's
//! fonts, so pixels may differ a little: a few are allowed to.

use std::path::PathBuf;
use std::sync::Arc;

use wl_shell_render::Image;
use wl_shell_render::text::Text;
use wl_shell_render::ui::canvas::{Cap, Cmd, Ink};
use wl_shell_render::ui::{
    self, Align, Border, BoxVariant, ButtonVariant, Color, Edges, Fit, Kind, Length, Look, Node,
    Place, Request, Resources, State, TextAlign, TextStyle, Theme, ThemeColor, Track,
};

/// Made-up icons and images, the same everywhere.
struct Fake;

fn gradient(w: u32, h: u32, rgb: [u8; 3], round: bool) -> Arc<Image> {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let inside = !round || {
                let (dx, dy) = (fx - 0.5, fy - 0.5);
                dx * dx + dy * dy <= 0.25
            };
            let a: u8 = if inside { 255 } else { 0 };
            let c = |v: u8, k: f32| ((v as f32 * (0.5 + 0.5 * k)) * a as f32 / 255.0) as u8;
            data.extend_from_slice(&[c(rgb[2], fy), c(rgb[1], fx), c(rgb[0], 1.0 - fy), a]);
        }
    }
    Arc::new(Image {
        width: w,
        height: h,
        data,
    })
}

impl Resources for Fake {
    fn app_icon(&mut self, _: &str, px: u32) -> Option<Arc<Image>> {
        Some(gradient(px, px, [60, 140, 230], false))
    }
    fn icon(&mut self, _: &str, px: u32) -> Option<Arc<Image>> {
        Some(gradient(px, px, [255, 255, 255], true))
    }
    fn image(&mut self, src: &str) -> Option<Arc<Image>> {
        (src != "missing").then(|| gradient(64, 32, [230, 120, 40], false))
    }
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Straight RGBA bytes of a premultiplied BGRA image.
fn rgba(img: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.data.len());
    for px in img.data.as_chunks::<4>().0 {
        let a = px[3];
        let un = |v: u8| match a {
            0 => 0,
            a => ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8,
        };
        out.extend_from_slice(&[un(px[2]), un(px[1]), un(px[0]), a]);
    }
    out
}

fn save(path: &PathBuf, img: &Image) {
    image::save_buffer(
        path,
        &rgba(img),
        img.width,
        img.height,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
}

/// Compare `img` with the golden `name`.
fn golden(name: &str, img: &Image) {
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    let actual = dir.join(format!("{name}.actual.png"));
    if std::env::var_os("WL_SHELL_BLESS").is_some() || !path.exists() {
        save(&path, img);
        let _ = std::fs::remove_file(&actual);
        return;
    }
    let want = image::open(&path).unwrap().into_rgba8();
    let got = rgba(img);
    let same_size = want.dimensions() == (img.width, img.height);
    let off = match same_size {
        true => (want.as_raw().as_chunks::<4>().0.iter())
            .zip(got.as_chunks::<4>().0)
            .filter(|(a, b)| (0..4).any(|k| a[k].abs_diff(b[k]) > 24))
            .count(),
        false => usize::MAX,
    };
    // Up to 1% of the pixels (antialiased text) may differ.
    let allowed = (img.width * img.height) as usize / 100;
    if off > allowed {
        save(&actual, img);
        panic!(
            "{name}: {} pixels differ (see {})",
            if same_size {
                off.to_string()
            } else {
                "all".into()
            },
            actual.display()
        );
    }
    let _ = std::fs::remove_file(&actual);
}

fn theme() -> Arc<Theme> {
    Arc::new(Theme::default())
}

fn paint(tree: Node, bounds: ui::Bounds, scale: i32, state: State) -> ui::Rendered {
    let req = Request {
        app: Some("app".into()),
        state,
        ..Request::new(Arc::new(tree), bounds, scale, theme())
    };
    ui::render(&req, &mut Text::new(), &mut Fake)
}

fn popup(tree: Node, scale: i32) -> ui::Rendered {
    paint(
        tree,
        ui::Bounds::Content {
            max_w: 400.0,
            max_h: 600.0,
        },
        scale,
        State::default(),
    )
}

/// Checks both scales, the second twice the size.
fn both(name: &str, tree: Node) {
    let one = popup(tree.clone(), 1);
    let two = popup(tree, 2);
    assert_eq!(
        (two.image.width, two.image.height),
        (one.image.width * 2, one.image.height * 2),
        "{name}"
    );
    golden(name, &one.image);
    golden(&format!("{name}@2"), &two.image);
}

// -- builders --------------------------------------------------------------

fn with(mut n: Node, f: impl FnOnce(&mut Node)) -> Node {
    f(&mut n);
    n
}

fn col(gap: f32, children: Vec<Node>) -> Node {
    with(Node::new(Kind::Col).with_children(children), |n| {
        n.layout.gap = Some(gap);
        n.layout.padding = Some(Edges::all(12.0));
        n.look.background = Some(Color::Theme(ThemeColor::Surface));
    })
}

fn row(gap: f32, children: Vec<Node>) -> Node {
    with(Node::new(Kind::Row).with_children(children), |n| {
        n.layout.gap = Some(gap);
    })
}

fn sized(n: Node, w: f32, h: f32) -> Node {
    with(n, |n| {
        n.layout.width = Some(Length::Px(w));
        n.layout.height = Some(Length::Px(h));
    })
}

fn block(c: u32, w: f32, h: f32) -> Node {
    sized(
        with(Node::new(Kind::Box { variant: None }), |n| {
            n.look.background = Some(Color::Rgba(c));
            n.look.radius = Some(3.0);
        }),
        w,
        h,
    )
}

fn text(s: &str, style: TextStyle) -> Node {
    with(Node::text(s), |n| {
        if let Kind::Text { style: st, .. } = &mut n.kind {
            *st = style;
        }
    })
}

fn icon(name: &str) -> Node {
    Node::new(Kind::Icon {
        name: name.into(),
        size: None,
        color: None,
    })
}

fn button(variant: ButtonVariant, key: &str, children: Vec<Node>) -> Node {
    with(
        Node::new(Kind::Button {
            variant,
            enabled: true,
            selected: false,
            tooltip: None,
        })
        .with_children(children),
        |n| {
            n.key = Some(key.into());
            n.handlers.click = true;
        },
    )
}

/// The clock face from the plan, in commands.
fn clock() -> Vec<Cmd> {
    let ink = |stroke: ThemeColor, width: f32| Ink {
        fill: None,
        stroke: Some(Color::Theme(stroke)),
        width,
        cap: Cap::Round,
    };
    let mut c = vec![Cmd::Circle {
        x: 50.0,
        y: 50.0,
        r: 48.0,
        ink: Ink {
            fill: Some(Color::Theme(ThemeColor::Surface)),
            ..Ink::default()
        },
    }];
    for i in 0..12 {
        let major = i % 3 == 0;
        c.extend([
            Cmd::Save,
            Cmd::Translate(50.0, 50.0),
            Cmd::Rotate(i as f32 * std::f32::consts::PI / 6.0),
            Cmd::Line {
                from: (0.0, -42.0),
                to: (0.0, if major { -35.0 } else { -39.0 }),
                ink: ink(ThemeColor::Muted, if major { 2.5 } else { 1.5 }),
            },
            Cmd::Restore,
        ]);
    }
    for (angle, len, width, color) in [
        (2.0, 24.0, 4.0, ThemeColor::Text),
        (4.0, 36.0, 3.0, ThemeColor::Text),
        (1.0, 40.0, 1.5, ThemeColor::Accent),
    ] {
        c.extend([
            Cmd::Save,
            Cmd::Translate(50.0, 50.0),
            Cmd::Rotate(angle),
            Cmd::Line {
                from: (0.0, 8.0),
                to: (0.0, -len),
                ink: ink(color, width),
            },
            Cmd::Restore,
        ]);
    }
    c.push(Cmd::Circle {
        x: 50.0,
        y: 50.0,
        r: 3.0,
        ink: Ink {
            fill: Some(Color::Theme(ThemeColor::Accent)),
            ..Ink::default()
        },
    });
    c
}

fn canvas(cmds: Vec<Cmd>) -> Node {
    Node::new(Kind::Canvas {
        viewbox: (100.0, 100.0),
        commands: cmds,
        anim: None,
    })
}

// -- the goldens -----------------------------------------------------------

#[test]
fn containers() {
    let grid = with(
        Node::new(Kind::Grid {
            columns: vec![Track::Fr(1.0); 4],
        })
        .with_children(
            (0..6)
                .map(|i| block(0x4080c0ff + (i << 16), 10.0, 14.0))
                .collect(),
        ),
        |n| n.layout.gap = Some(3.0),
    );
    let stack = sized(
        Node::new(Kind::Stack {
            place: Place {
                x: Align::End,
                y: Align::Start,
            },
        })
        .with_children(vec![
            Node::new(Kind::AppIcon {
                app: None,
                size: None,
            }),
            block(0xff3b30ff, 12.0, 12.0),
        ]),
        48.0,
        48.0,
    );
    let boxes = row(
        6.0,
        vec![
            Node::new(Kind::Box {
                variant: Some(BoxVariant::Card),
            })
            .with_children(vec![text("card", TextStyle::Body)]),
            Node::new(Kind::Box {
                variant: Some(BoxVariant::Inset),
            })
            .with_children(vec![text("inset", TextStyle::Body)]),
            Node::new(Kind::Box {
                variant: Some(BoxVariant::Badge),
            })
            .with_children(vec![text("3", TextStyle::Caption)]),
            with(
                Node::new(Kind::Box { variant: None })
                    .with_children(vec![text("bordered", TextStyle::Body)]),
                |n| {
                    n.layout.padding = Some(Edges::sym(2.0, 6.0));
                    n.look.border = Some(Border {
                        width: 1.0,
                        color: Color::Theme(ThemeColor::Accent),
                    });
                    n.look.radius = Some(4.0);
                },
            ),
        ],
    );
    let spaced = sized(
        row(
            0.0,
            vec![
                block(0xffffffff, 10.0, 10.0),
                Node::new(Kind::Spacer { size: None }),
                Node::new(Kind::Divider),
                Node::new(Kind::Spacer { size: Some(8.0) }),
                block(0xffffffff, 10.0, 10.0),
            ],
        ),
        200.0,
        14.0,
    );
    let mut scroll = Node::new(Kind::Scroll).with_children(
        (0..8)
            .map(|i| text(&format!("line {i}"), TextStyle::Body))
            .collect(),
    );
    scroll.layout.max_height = Some(Length::Px(40.0));
    scroll.key = Some("scroll".into());
    let tree = col(
        8.0,
        vec![grid, stack, boxes, spaced, Node::new(Kind::Divider), scroll],
    );
    both("containers", tree.clone());
    // Scrolled down.
    let state = State {
        scroll: vec![("scroll".into(), 30.0)],
        ..State::default()
    };
    let bounds = ui::Bounds::Content {
        max_w: 400.0,
        max_h: 600.0,
    };
    let scrolled = paint(tree, bounds, 1, state);
    golden("containers-scrolled", &scrolled.image);
    let hit = scrolled.hits.iter().find(|h| h.key == "scroll").unwrap();
    assert_eq!((hit.kind, hit.rect.h), ("scroll", 40.0));
    assert!(hit.content > 100.0);
}

/// A node that starts asking to be revealed scrolls its area as little
/// as shows it whole, once.
#[test]
fn revealing_scrolls_once() {
    // Ten rows 20 high in an area 50 high: row `i` spans 20i..20i+20.
    let area = |reveal: usize, tall: bool| {
        let rows = (0..10)
            .map(|i| {
                let h = if tall && i == reveal { 80.0 } else { 20.0 };
                with(block(0xffffffff, 100.0, h), |n| {
                    // Boxes would shrink to fit the area; text doesn't.
                    n.layout.min_height = Some(Length::Px(h));
                    n.key = Some(format!("r{i}"));
                    n.reveal = i == reveal;
                })
            })
            .collect();
        let mut scroll = Node::new(Kind::Scroll).with_children(rows);
        scroll.layout.max_height = Some(Length::Px(50.0));
        scroll.layout.gap = Some(0.0);
        scroll.key = Some("s".into());
        scroll
    };
    let bounds = ui::Bounds::Content {
        max_w: 400.0,
        max_h: 600.0,
    };
    let at = |offset: f32, revealed: &[&str]| State {
        scroll: vec![("s".into(), offset)],
        revealed: revealed.iter().map(|s| s.to_string()).collect(),
        ..State::default()
    };
    // Below: its bottom comes up to the area's.
    let r = paint(area(7, false), bounds, 1, at(0.0, &[]));
    assert_eq!(r.scroll, [("s".to_string(), 110.0)]);
    assert_eq!(r.revealed, ["r7"]);
    // Painted again, it asked already: the wheel may scroll it away.
    let r = paint(area(7, false), bounds, 1, at(0.0, &["r7"]));
    assert!(r.scroll.is_empty());
    // Above: its top goes down to the area's.
    let r = paint(area(2, false), bounds, 1, at(150.0, &["r7"]));
    assert_eq!(r.scroll, [("s".to_string(), 40.0)]);
    // Already shown: nothing moves.
    let r = paint(area(1, false), bounds, 1, at(0.0, &[]));
    assert!(r.scroll.is_empty());
    // Taller than the area: its top shows.
    let r = paint(area(3, true), bounds, 1, at(0.0, &[]));
    assert_eq!(r.scroll, [("s".to_string(), 60.0)]);
}

#[test]
fn texts() {
    let styled = |style, s: &str| text(s, style);
    let mut cut = text(
        "A long line of text that wraps onto more lines than it may show, so it ends in an ellipsis",
        TextStyle::Body,
    );
    if let Kind::Text { max_lines, .. } = &mut cut.kind {
        *max_lines = Some(2);
    }
    cut.layout.width = Some(Length::Px(180.0));
    let mut centered = text("centered", TextStyle::Body);
    if let Kind::Text {
        align,
        color,
        weight,
        ..
    } = &mut centered.kind
    {
        *align = TextAlign::Center;
        *color = Some(Color::Theme(ThemeColor::Accent));
        *weight = Some(700);
    }
    let tree = col(
        4.0,
        vec![
            styled(TextStyle::Title, "Title"),
            styled(TextStyle::Body, "Body text"),
            styled(TextStyle::Muted, "Muted text"),
            styled(TextStyle::Caption, "Caption text"),
            cut,
            centered,
            styled(TextStyle::Body, "Two\nlines"),
        ],
    );
    both("texts", tree);
}

#[test]
fn icons_and_images() {
    let image = |fit, radius, blur, opacity| {
        sized(
            Node::new(Kind::Image {
                src: "art.png".into(),
                fit,
                radius,
                blur,
                opacity,
            }),
            48.0,
            48.0,
        )
    };
    let tinted = with(icon("media-playback-start"), |n| {
        if let Kind::Icon { color, size, .. } = &mut n.kind {
            *color = Some(Color::Theme(ThemeColor::Accent));
            *size = Some(24.0);
        }
    });
    let tree = col(
        8.0,
        vec![
            row(
                8.0,
                vec![
                    icon("folder"),
                    icon("go-next-symbolic"),
                    tinted,
                    Node::new(Kind::AppIcon {
                        app: Some("other".into()),
                        size: Some(32.0),
                    }),
                ],
            ),
            row(
                8.0,
                vec![
                    image(Fit::Cover, 0.0, 0.0, 1.0),
                    image(Fit::Contain, 0.0, 0.0, 1.0),
                    image(Fit::Cover, 12.0, 0.0, 1.0),
                    image(Fit::Cover, 0.0, 8.0, 1.0),
                    image(Fit::Cover, 0.0, 0.0, 0.4),
                ],
            ),
        ],
    );
    both("images", tree);
}

#[test]
fn canvases() {
    let mut big = canvas(clock());
    big.layout.width = Some(Length::Px(120.0));
    let more = vec![
        Cmd::Rect {
            x: 10.0,
            y: 10.0,
            w: 80.0,
            h: 30.0,
            radius: 6.0,
            ink: Ink {
                fill: Some(Color::Rgba(0x30a050ff)),
                stroke: Some(Color::Theme(ThemeColor::Text)),
                width: 2.0,
                cap: Cap::Butt,
            },
        },
        Cmd::Arc {
            x: 30.0,
            y: 70.0,
            r: 18.0,
            start: 0.0,
            end: 4.0,
            pie: true,
            ink: Ink {
                fill: Some(Color::Theme(ThemeColor::Accent)),
                ..Ink::default()
            },
        },
        Cmd::Path {
            segs: ui::canvas::parse_path("M 60 90 L 75 55 L 90 90 Z").unwrap(),
            ink: Ink {
                stroke: Some(Color::Rgba(0xffcc00ff)),
                width: 3.0,
                cap: Cap::Round,
                fill: None,
            },
        },
        Cmd::Text {
            x: 50.0,
            y: 25.0,
            text: "12".into(),
            size: 14.0,
            color: None,
            bold: true,
            align: TextAlign::Center,
        },
    ];
    let mut small = canvas(more);
    small.layout.width = Some(Length::Px(120.0));
    both("canvases", col(8.0, vec![row(8.0, vec![big, small])]));
}

#[test]
fn controls() {
    let label = |s: &str| text(s, TextStyle::Body);
    let tree = |hovered: Option<&str>, pressed: Option<&str>| {
        let disabled = with(
            button(ButtonVariant::Plain, "off", vec![label("Disabled")]),
            |n| {
                if let Kind::Button { enabled, .. } = &mut n.kind {
                    *enabled = false;
                }
            },
        );
        let selected = with(
            button(ButtonVariant::Plain, "sel", vec![label("15")]),
            |n| {
                if let Kind::Button { selected, .. } = &mut n.kind {
                    *selected = true;
                }
            },
        );
        let slider = |value, enabled| {
            with(
                Node::new(Kind::Slider {
                    value,
                    min: 0.0,
                    max: 100.0,
                    enabled,
                    live: None,
                    transition: 0.0,
                }),
                |n| n.key = Some(format!("slider{value}")),
            )
        };
        let toggle = |value| {
            Node::new(Kind::Toggle {
                value,
                enabled: true,
            })
        };
        let hover_box = with(
            Node::new(Kind::Box { variant: None }).with_children(vec![label("hover me")]),
            |n| {
                n.key = Some("hb".into());
                n.layout.padding = Some(Edges::all(4.0));
                n.hover = Some(Look {
                    background: Some(Color::Theme(ThemeColor::Hover)),
                    radius: Some(4.0),
                    ..Look::default()
                });
            },
        );
        let state = State {
            hovered: hovered.map(String::from),
            pressed: pressed.map(String::from),
            ..State::default()
        };
        let t = col(
            8.0,
            vec![
                row(
                    6.0,
                    vec![
                        button(ButtonVariant::Plain, "plain", vec![label("Plain")]),
                        button(
                            ButtonVariant::Filled,
                            "filled",
                            vec![icon("media-playback-start-symbolic"), label("Play")],
                        ),
                        button(ButtonVariant::Ghost, "ghost", vec![icon("x-symbolic")]),
                        disabled,
                        selected,
                    ],
                ),
                slider(30.0, true),
                slider(70.0, false),
                row(8.0, vec![toggle(true), toggle(false), hover_box]),
                Node::new(Kind::Progress {
                    value: 0.25,
                    max: 1.0,
                    live: None,
                    indeterminate: false,
                    transition: 0.0,
                }),
            ],
        );
        (t, state)
    };
    let (t, _) = tree(None, None);
    both("controls", t);
    let (t, state) = tree(Some("plain"), Some("filled"));
    let bounds = ui::Bounds::Content {
        max_w: 400.0,
        max_h: 600.0,
    };
    let r = paint(t.clone(), bounds, 1, state);
    golden("controls-pointer", &r.image);
    let (_, state) = tree(Some("hb"), None);
    golden("controls-hover", &paint(t, bounds, 1, state).image);
    // The buttons, sliders and the hover box are tracked, in order.
    let keys: Vec<&str> = r.hits.iter().map(|h| h.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "plain", "filled", "ghost", "off", "sel", "slider30", "slider70", "hb"
        ]
    );
    assert!(!r.hits[3].enabled && r.hits[0].enabled);
}

/// Tiles: a canvas and an app icon with a badge on top, as the dock asks
/// for them at full magnification, at scales 1 and 2; and a 2x tile.
#[test]
fn tiles() {
    let tile = |tree: Node, w: f32, h: f32, scale| {
        paint(tree, ui::Bounds::Exact(w, h), scale, State::default())
    };
    let badge = Node::new(Kind::Stack {
        place: Place {
            x: Align::End,
            y: Align::Start,
        },
    })
    .with_children(vec![
        Node::new(Kind::AppIcon {
            app: None,
            size: None,
        }),
        Node::new(Kind::Box {
            variant: Some(BoxVariant::Badge),
        })
        .with_children(vec![text("7", TextStyle::Caption)]),
    ]);
    for scale in [1, 2] {
        let clock = tile(canvas(clock()), 96.0, 96.0, scale);
        assert_eq!(clock.image.width, 96 * scale as u32);
        golden(&format!("tile-clock@{scale}"), &clock.image);
        let b = tile(badge.clone(), 96.0, 96.0, scale);
        golden(&format!("tile-badge@{scale}"), &b.image);
    }
    let wide = row(
        8.0,
        vec![
            sized(canvas(clock()), 80.0, 80.0),
            col(
                0.0,
                vec![
                    text("Now playing", TextStyle::Title),
                    text("Artist", TextStyle::Muted),
                ],
            ),
        ],
    );
    let wide = with(wide, |n| n.layout.padding = Some(Edges::all(8.0)));
    let w = tile(wide, 192.0, 96.0, 1);
    assert_eq!((w.image.width, w.image.height), (192, 96));
    golden("tile-wide", &w.image);
}

/// A popup painted `apart`: its live slider is left out of the image and
/// listed, painted alone it looks as it does inline, and a live bar in a
/// scroll area stays inline. The image's corners are cut round.
#[test]
fn live_parts_apart() {
    let live = Some(ui::Live {
        rate: 1.0,
        at: 100.0,
    });
    let seek = with(
        sized(
            Node::new(Kind::Slider {
                value: 10.0,
                min: 0.0,
                max: 100.0,
                enabled: true,
                live,
                transition: 0.0,
            }),
            200.0,
            20.0,
        ),
        |n| n.key = Some("seek".into()),
    );
    let bar = sized(
        Node::new(Kind::Progress {
            value: 0.5,
            max: 1.0,
            live,
            indeterminate: false,
            transition: 0.0,
        }),
        200.0,
        6.0,
    );
    let scroll = with(Node::new(Kind::Scroll).with_children(vec![bar]), |n| {
        n.key = Some("list".into());
        n.layout.max_height = Some(Length::Px(40.0));
    });
    let tree = with(
        col(8.0, vec![block(0xff0000ff, 100.0, 20.0), seek, scroll]),
        |n| n.look.background = Some(Color::Rgba(0x202020ff)),
    );
    let request = |apart, radius| Request {
        app: Some("app".into()),
        state: State {
            now: 110.0,
            ..State::default()
        },
        radius,
        apart,
        ..Request::new(
            Arc::new(tree.clone()),
            ui::Bounds::Content {
                max_w: 400.0,
                max_h: 400.0,
            },
            1,
            theme(),
        )
    };
    let mut text = Text::new();
    let inline = ui::render(&request(false, 0.0), &mut text, &mut Fake);
    let apart = ui::render(&request(true, 10.0), &mut text, &mut Fake);
    assert!(inline.live.is_empty());
    assert_eq!(
        apart.live.len(),
        1,
        "only the slider outside the scroll area"
    );
    let part = &apart.live[0];
    assert_eq!(part.node.key.as_deref(), Some("seek"));
    let r = part.rect;
    assert_eq!((r.x, r.y, r.w, r.h), (12.0, 40.0, 200.0, 20.0));
    // Still a hit, for dragging it.
    assert!(apart.hits.iter().any(|h| h.key == "seek"));

    let px = |img: &Image, x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        [0, 1, 2, 3].map(|k| img.data[i + k])
    };
    // Where the slider was, only the background (as in the padding).
    let bg = px(&apart.image, 5, 50);
    for (x, y) in [(12, 50), (56, 50), (200, 50)] {
        assert_eq!(px(&apart.image, x, y), bg, "({x}, {y})");
    }
    assert_ne!(px(&inline.image, 56, 50), bg);
    // Corners cut round, the inline one square.
    assert_eq!(px(&apart.image, 0, 0)[3], 0);
    assert_eq!(px(&inline.image, 0, 0)[3], 255);

    // Alone, as big as its rect, it is what the inline image shows there
    // (the track and knob over transparency, not the background).
    let alone = ui::render(&request(true, 10.0).part(part), &mut text, &mut Fake);
    assert_eq!((alone.image.width, alone.image.height), (200, 20));
    assert!(alone.live.is_empty());
    // Its value moved with the clock: 20 of 100 at 110 s, so the knob is
    // at 8 + 184 * 0.2 (at 10, it would be at 26).
    assert_eq!(px(&alone.image, 44, 10)[3], 255);
    assert_eq!(px(&inline.image, 12 + 44, 40 + 10)[3], 255);
    let differ = (0..200)
        .flat_map(|x| (0..20).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let a = px(&alone.image, x, y);
            let b = px(&inline.image, 12 + x, 40 + y);
            // Where the part is opaque, it covers the background alike.
            a[3] == 255 && a != b
        })
        .count();
    assert_eq!(differ, 0);
}

/// Images side by side, top aligned.
fn strip(images: &[Image]) -> Image {
    let w: u32 = images.iter().map(|i| i.width).sum();
    let h = images.iter().map(|i| i.height).max().unwrap_or(0);
    let mut data = vec![0u8; (w * h * 4) as usize];
    let mut x0 = 0;
    for img in images {
        for y in 0..img.height {
            let src = &img.data[(y * img.width * 4) as usize..((y + 1) * img.width * 4) as usize];
            let at = ((y * w + x0) * 4) as usize;
            data[at..at + src.len()].copy_from_slice(src);
        }
        x0 += img.width;
    }
    Image {
        width: w,
        height: h,
        data,
    }
}

/// A spinner: a quarter arc turning, sampled at 8 values of `t`.
fn spinner() -> Node {
    let frames: Vec<Vec<Cmd>> = (0..8)
        .map(|i| {
            let a = i as f32 / 8.0 * std::f32::consts::TAU;
            vec![
                Cmd::Circle {
                    x: 50.0,
                    y: 50.0,
                    r: 40.0,
                    ink: Ink {
                        stroke: Some(Color::Theme(ThemeColor::Track)),
                        width: 10.0,
                        ..Ink::default()
                    },
                },
                Cmd::Arc {
                    x: 50.0,
                    y: 50.0,
                    r: 40.0,
                    start: a,
                    end: a + 1.6,
                    pie: false,
                    ink: Ink {
                        stroke: Some(Color::Theme(ThemeColor::Accent)),
                        width: 10.0,
                        cap: Cap::Round,
                        fill: None,
                    },
                },
            ]
        })
        .collect();
    with(
        Node::new(Kind::Canvas {
            viewbox: (100.0, 100.0),
            commands: frames[0].clone(),
            anim: Some(Box::new(ui::Frames {
                frames,
                drive: ui::Drive::Loop {
                    period: 1.2,
                    mirror: false,
                },
            })),
        }),
        |n| {
            n.key = Some("spin".into());
            n.layout.width = Some(Length::Px(24.0));
            n.layout.height = Some(Length::Px(24.0));
        },
    )
}

/// A tile painted apart for frame sheets: its spinner is left out (and
/// listed), its still progress bar isn't; the spinner's sheet has every
/// frame, as big as its rect, at scales 1 and 2.
#[test]
fn frame_sheets() {
    let bar = with(
        Node::new(Kind::Progress {
            value: 0.0,
            max: 1.0,
            live: None,
            indeterminate: true,
            transition: 0.0,
        }),
        |n| n.key = Some("bar".into()),
    );
    let tile = Node::new(Kind::Stack {
        place: Place {
            x: Align::End,
            y: Align::End,
        },
    })
    .with_children(vec![
        Node::new(Kind::AppIcon {
            app: None,
            size: None,
        }),
        spinner(),
        with(bar, |n| n.layout.width = Some(Length::Px(30.0))),
    ]);
    let mut text = Text::new();
    for scale in [1, 2] {
        let req = Request {
            app: Some("app".into()),
            apart: true,
            sheets_only: true,
            ..Request::new(
                Arc::new(tile.clone()),
                ui::Bounds::Exact(48.0, 48.0),
                scale,
                theme(),
            )
        };
        let r = ui::render(&req, &mut text, &mut Fake);
        let keys: Vec<_> = r.live.iter().map(|p| p.node.key.as_deref()).collect();
        assert_eq!(keys, [Some("spin")], "only the canvas plays apart");
        let part = &r.live[0];
        assert_eq!((part.rect.x, part.rect.y, part.rect.w), (24.0, 24.0, 24.0));
        let sheet = ui::render_sheet(&req.part(part), &mut text, &mut Fake, 16 << 20);
        assert_eq!((sheet.images.len(), sheet.step), (8, 1));
        let px = 24 * scale as u32;
        assert!(
            (sheet.images.iter()).all(|i| (i.width, i.height) == (px, px)),
            "as big as the part"
        );
        assert_ne!(sheet.images[0], sheet.images[2], "the frames differ");
        golden(&format!("sheet@{scale}"), &strip(&sheet.images));
        // Over a cap that fits three: every fourth frame.
        let small = ui::render_sheet(
            &req.part(part),
            &mut text,
            &mut Fake,
            (px * px * 4 * 3) as usize,
        );
        assert_eq!((small.images.len(), small.step), (2, 4));
        assert_eq!(small.images[1], sheet.images[4]);
    }
    // Painted inline (in a popup that isn't apart, or with animations
    // off), a canvas shows its still frame.
    let still = ui::render(
        &Request::new(
            Arc::new(spinner()),
            ui::Bounds::Exact(24.0, 24.0),
            1,
            theme(),
        ),
        &mut text,
        &mut Fake,
    );
    let req = Request::new(
        Arc::new(spinner()),
        ui::Bounds::Exact(24.0, 24.0),
        1,
        theme(),
    );
    let first = ui::render_sheet(&req, &mut text, &mut Fake, 16 << 20);
    assert_eq!(still.image, first.images[0]);
}

/// An indeterminate bar's segment is where its phase says; an eased
/// value shows the host's value, not the node's.
#[test]
fn values_from_the_host() {
    let bar = |indeterminate| {
        sized(
            with(
                Node::new(Kind::Progress {
                    value: 1.0,
                    max: 1.0,
                    live: None,
                    indeterminate,
                    transition: 300.0,
                }),
                |n| n.key = Some("bar".into()),
            ),
            90.0,
            6.0,
        )
    };
    let at = |tree: Node, v: f64| {
        let state = State {
            values: vec![("bar".into(), v)],
            ..State::default()
        };
        paint(tree, ui::Bounds::Exact(90.0, 6.0), 1, state).image
    };
    let alpha_at = |img: &Image, x: u32| img.data[((3 * img.width + x) * 4 + 3) as usize];
    let accent = |img: &Image, x: u32| {
        let i = ((3 * img.width + x) * 4) as usize;
        // Blue-ish: the accent (0x81d0ff), not the track (white).
        img.data[i] > img.data[i + 2] + 40
    };
    // Phase 0: the segment's at the start; 1: at the end.
    let start = at(bar(true), 0.0);
    assert!(accent(&start, 10) && !accent(&start, 80));
    let end = at(bar(true), 1.0);
    assert!(!accent(&end, 10) && accent(&end, 80));
    // Eased to 0.5: half full, though the node says 1.
    let half = at(bar(false), 0.5);
    assert!(accent(&half, 30) && !accent(&half, 60));
    assert_eq!(alpha_at(&half, 60), alpha_at(&start, 80));
}
