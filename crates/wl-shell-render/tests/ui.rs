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
        tree: Arc::new(tree),
        bounds,
        scale,
        theme: theme(),
        app: Some("app".into()),
        state,
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
