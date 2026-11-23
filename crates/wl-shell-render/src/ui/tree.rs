//! The tree itself: plain data, `Send`, compared by value, with nothing
//! of Lua in it. Handlers are replaced by node keys before a tree leaves
//! the plugin's thread; `handlers` only says which ones a node has.

use super::canvas::Cmd;

/// A color a theme names, so a plugin looks native in light and dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeColor {
    /// Body text.
    Text,
    /// Secondary text.
    Muted,
    /// Highlights: a filled button, a slider's fill, a selected day.
    Accent,
    /// Text and icons on the accent.
    AccentText,
    /// The popup's background, and a canvas's face.
    Surface,
    Border,
    /// Under the hovered button.
    Hover,
    /// Under the pressed button.
    Pressed,
    /// A slider's or progress bar's empty track.
    Track,
    /// `ui.divider`.
    Divider,
    /// `ui.box{variant = 'card'}`.
    Card,
    /// `ui.box{variant = 'inset'}`.
    Inset,
    /// `ui.box{variant = 'badge'}`, and its text.
    Badge,
    BadgeText,
}

impl ThemeColor {
    /// Every theme color, with its name in Lua (`dock.theme.<name>`).
    pub const ALL: [(&'static str, ThemeColor); 14] = [
        ("text", ThemeColor::Text),
        ("muted", ThemeColor::Muted),
        ("accent", ThemeColor::Accent),
        ("accent_text", ThemeColor::AccentText),
        ("surface", ThemeColor::Surface),
        ("border", ThemeColor::Border),
        ("hover", ThemeColor::Hover),
        ("pressed", ThemeColor::Pressed),
        ("track", ThemeColor::Track),
        ("divider", ThemeColor::Divider),
        ("card", ThemeColor::Card),
        ("inset", ThemeColor::Inset),
        ("badge", ThemeColor::Badge),
        ("badge_text", ThemeColor::BadgeText),
    ];

    pub fn name(self) -> &'static str {
        (Self::ALL.iter())
            .find(|(_, c)| *c == self)
            .map_or("text", |(n, _)| n)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
    }
}

/// A color: one of the theme's, or 0xRRGGBBAA (not premultiplied).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Rgba(u32),
    Theme(ThemeColor),
}

impl Color {
    /// A theme color's name (`accent`), or a CSS color (`#rrggbb`,
    /// `#rrggbbaa`, `rgb(...)`, `red`, `transparent`).
    pub fn parse(s: &str) -> Result<Color, String> {
        let s = s.trim();
        if let Some(c) = ThemeColor::from_name(s) {
            return Ok(Color::Theme(c));
        }
        let c: svgtypes::Color = s
            .parse()
            .map_err(|_| format!("{s:?} is not a color or a theme color"))?;
        Ok(Color::Rgba(u32::from_be_bytes([
            c.red, c.green, c.blue, c.alpha,
        ])))
    }
}

/// What the tree takes from the host's appearance: its colors, font and
/// sizes. Logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub font: String,
    /// The body text's size; the other styles are relative to it.
    pub font_size: f32,
    /// Per `ThemeColor`, in its order: 0xRRGGBBAA.
    pub colors: [u32; 14],
    /// Corners of boxes and buttons.
    pub radius: f32,
    /// Padding of cards and buttons.
    pub spacing: f32,
    /// `ui.icon` without a `size`.
    pub icon_size: f32,
    /// The icon theme for `ui.icon` and `ui.app_icon` (None: the
    /// desktop's).
    pub icon_theme: Option<String>,
}

impl Default for Theme {
    /// The dock's dark look.
    fn default() -> Self {
        let mut t = Theme {
            font: "sans-serif".into(),
            font_size: 13.0,
            colors: [0; 14],
            radius: 8.0,
            spacing: 8.0,
            icon_size: 16.0,
            icon_theme: None,
        };
        for (c, rgba) in [
            (ThemeColor::Text, 0xffffffff),
            (ThemeColor::Muted, 0xffffff8c),
            (ThemeColor::Accent, 0x81d0ffff),
            (ThemeColor::AccentText, 0x1f1f1fff),
            (ThemeColor::Surface, 0x262626eb),
            (ThemeColor::Border, 0xffffff1f),
            (ThemeColor::Hover, 0xffffff2e),
            (ThemeColor::Pressed, 0xffffff47),
            (ThemeColor::Track, 0xffffff33),
            (ThemeColor::Divider, 0xffffff26),
            (ThemeColor::Card, 0xffffff14),
            (ThemeColor::Inset, 0x0000004d),
            (ThemeColor::Badge, 0xff3b30ff),
            (ThemeColor::BadgeText, 0xffffffff),
        ] {
            t.set(c, rgba);
        }
        t
    }
}

impl Theme {
    pub fn get(&self, c: ThemeColor) -> u32 {
        self.colors[Self::index(c)]
    }

    pub fn set(&mut self, c: ThemeColor, rgba: u32) {
        self.colors[Self::index(c)] = rgba;
    }

    fn index(c: ThemeColor) -> usize {
        (ThemeColor::ALL.iter())
            .position(|(_, k)| *k == c)
            .unwrap_or(0)
    }

    /// `c` as 0xRRGGBBAA.
    pub fn rgba(&self, c: Color) -> u32 {
        match c {
            Color::Rgba(rgba) => rgba,
            Color::Theme(t) => self.get(t),
        }
    }
}

/// A size: logical pixels, or a share of the parent's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(f32),
    Percent(f32),
}

/// Space on each side, logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn all(v: f32) -> Self {
        Edges {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    /// `v` above and below, `h` on the sides.
    pub fn sym(v: f32, h: f32) -> Self {
        Edges {
            top: v,
            right: h,
            bottom: v,
            left: h,
        }
    }
}

/// Where children go across a row or column, or in a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

/// How children are spread along a row or column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
    Around,
    Evenly,
}

/// Where a stack puts its layers: across (`x`) and down (`y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub x: Align,
    pub y: Align,
}

impl Default for Place {
    fn default() -> Self {
        Place {
            x: Align::Center,
            y: Align::Center,
        }
    }
}

/// A grid column's width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Track {
    Px(f32),
    /// A share of what is left.
    Fr(f32),
    /// As wide as its widest cell.
    Auto,
}

/// Size and flex properties, any node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub width: Option<Length>,
    pub height: Option<Length>,
    pub min_width: Option<Length>,
    pub min_height: Option<Length>,
    pub max_width: Option<Length>,
    pub max_height: Option<Length>,
    /// Takes this share of the room left along its parent's row or
    /// column.
    pub grow: f32,
    /// Between children (between grid columns).
    pub gap: Option<f32>,
    /// Between grid rows; default: `gap`.
    pub row_gap: Option<f32>,
    pub padding: Option<Edges>,
    /// Children across a row or column.
    pub align: Option<Align>,
    /// Children along it.
    pub justify: Option<Justify>,
}

/// A border inside the node's edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    pub width: f32,
    pub color: Color,
}

/// How a node is painted under its content: any node; and what changes
/// while it is hovered (`Node::hover`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    pub background: Option<Color>,
    pub radius: Option<f32>,
    pub border: Option<Border>,
    /// 0 to 1, for the node and everything in it.
    pub opacity: Option<f32>,
}

impl Look {
    /// `self` with what `over` sets over it.
    pub fn with(self, over: &Look) -> Look {
        Look {
            background: over.background.or(self.background),
            radius: over.radius.or(self.radius),
            border: over.border.or(self.border),
            opacity: over.opacity.or(self.opacity),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxVariant {
    /// A raised panel: the card color, rounded, padded.
    Card,
    /// A sunken one.
    Inset,
    /// A small pill for counts, in the badge colors.
    Badge,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextStyle {
    /// Larger and bold.
    Title,
    #[default]
    Body,
    /// The body's size, in the muted color.
    Muted,
    /// Smaller.
    Caption,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    #[default]
    Start,
    Center,
    End,
}

/// How an image fills its box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// All of it, cut to the box.
    #[default]
    Cover,
    /// All of the image, inside the box.
    Contain,
    /// Stretched to the box.
    Fill,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    /// No background but while hovered or pressed.
    #[default]
    Plain,
    /// The accent.
    Filled,
    /// A faint background always.
    Ghost,
}

/// A value that moves on its own (a playing track's position): `value`
/// at `at` (seconds, the host's clock), changing by `rate` per second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Live {
    pub rate: f64,
    pub at: f64,
}

/// The handlers a node has (kept in its Lua state, by the node's key).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Handlers {
    pub click: bool,
    pub change: bool,
    pub commit: bool,
}

impl Handlers {
    pub fn any(&self) -> bool {
        self.click || self.change || self.commit
    }
}

/// What a node is, with what only that kind has.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A flex column.
    Col,
    /// A flex row.
    Row,
    Grid {
        columns: Vec<Track>,
    },
    /// Children layered, the first at the bottom.
    Stack {
        place: Place,
    },
    /// A styled container (a column).
    Box {
        variant: Option<BoxVariant>,
    },
    /// A column that scrolls vertically past `max_height`; the host owns
    /// the offset.
    Scroll,
    /// Flexible space; `size` fixes its length.
    Spacer {
        size: Option<f32>,
    },
    /// A thin line across its parent.
    Divider,
    Text {
        text: String,
        style: TextStyle,
        /// 100 to 900; default: the style's.
        weight: Option<u16>,
        color: Option<Color>,
        /// Logical pixels; default: the style's.
        size: Option<f32>,
        align: TextAlign,
        /// Cut with an ellipsis after this many lines.
        max_lines: Option<u32>,
    },
    /// A theme icon by name, or a file.
    Icon {
        name: String,
        size: Option<f32>,
        /// Recolors it (symbolic icons are, with the text's color).
        color: Option<Color>,
    },
    /// An app's icon, as the dock draws it; None: the widget's app.
    AppIcon {
        app: Option<String>,
        size: Option<f32>,
    },
    Image {
        /// A path (`~/` expands), a `file://` URL or an `http(s)://` one.
        src: String,
        fit: Fit,
        radius: f32,
        /// Gaussian blur radius, logical pixels.
        blur: f32,
        opacity: f32,
    },
    /// Vector drawing in viewbox units, scaled to the node's box.
    Canvas {
        viewbox: (f32, f32),
        commands: Vec<Cmd>,
    },
    /// Its content is its children (the builder turns `label` and `icon`
    /// into children).
    Button {
        variant: ButtonVariant,
        enabled: bool,
        selected: bool,
        tooltip: Option<String>,
    },
    Slider {
        value: f64,
        min: f64,
        max: f64,
        enabled: bool,
        live: Option<Live>,
    },
    Toggle {
        value: bool,
        enabled: bool,
    },
    Progress {
        value: f64,
        max: f64,
        live: Option<Live>,
    },
}

impl Kind {
    /// The builder's name for it (`ui.<name>`).
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Col => "col",
            Kind::Row => "row",
            Kind::Grid { .. } => "grid",
            Kind::Stack { .. } => "stack",
            Kind::Box { .. } => "box",
            Kind::Scroll => "scroll",
            Kind::Spacer { .. } => "spacer",
            Kind::Divider => "divider",
            Kind::Text { .. } => "text",
            Kind::Icon { .. } => "icon",
            Kind::AppIcon { .. } => "app_icon",
            Kind::Image { .. } => "image",
            Kind::Canvas { .. } => "canvas",
            Kind::Button { .. } => "button",
            Kind::Slider { .. } => "slider",
            Kind::Toggle { .. } => "toggle",
            Kind::Progress { .. } => "progress",
        }
    }

    /// Whether the pointer does something to it (the host tracks its
    /// hover and pressed states).
    pub fn interactive(&self) -> bool {
        matches!(
            self,
            Kind::Button { .. } | Kind::Slider { .. } | Kind::Toggle { .. } | Kind::Scroll
        )
    }
}

/// One node of a tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: Kind,
    pub layout: Layout,
    pub look: Look,
    /// Over `look` while the pointer is on it.
    pub hover: Option<Look>,
    /// Stable identity, for state and handlers: given, or generated for
    /// interactive nodes and nodes with handlers.
    pub key: Option<String>,
    pub handlers: Handlers,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(kind: Kind) -> Self {
        Node {
            kind,
            layout: Layout::default(),
            look: Look::default(),
            hover: None,
            key: None,
            handlers: Handlers::default(),
            children: Vec::new(),
        }
    }

    pub fn with_children(mut self, children: Vec<Node>) -> Self {
        self.children = children;
        self
    }

    /// The nodes in it, itself included.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    /// The node and its descendants, depth first, itself first: the
    /// order `Laid::rects` is in.
    pub fn walk(&self) -> Vec<&Node> {
        fn go<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
            out.push(n);
            for c in &n.children {
                go(c, out);
            }
        }
        let mut out = Vec::with_capacity(self.count());
        go(self, &mut out);
        out
    }

    /// Whether the tree has an image of `src` (so it is painted again
    /// when a fetch brings it).
    pub fn shows_image(&self, src: &str) -> bool {
        matches!(&self.kind, Kind::Image { src: s, .. } if s == src)
            || self.children.iter().any(|c| c.shows_image(src))
    }

    pub fn text(text: impl Into<String>) -> Self {
        Node::new(Kind::Text {
            text: text.into(),
            style: TextStyle::Body,
            weight: None,
            color: None,
            size: None,
            align: TextAlign::Start,
            max_lines: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(Color::parse("accent"), Ok(Color::Theme(ThemeColor::Accent)));
        assert_eq!(Color::parse("#ff0000"), Ok(Color::Rgba(0xff0000ff)));
        assert_eq!(Color::parse("#00ff0080"), Ok(Color::Rgba(0x00ff0080)));
        assert_eq!(Color::parse("white"), Ok(Color::Rgba(0xffffffff)));
        assert_eq!(Color::parse("transparent"), Ok(Color::Rgba(0)));
        assert!(Color::parse("nope").is_err());
        let t = Theme::default();
        assert_eq!(t.rgba(Color::Theme(ThemeColor::Text)), 0xffffffff);
        for (name, c) in ThemeColor::ALL {
            assert_eq!(c.name(), name);
        }
    }

    #[test]
    fn walking() {
        let tree = Node::new(Kind::Col).with_children(vec![
            Node::new(Kind::Row).with_children(vec![Node::text("a")]),
            Node::text("b"),
        ]);
        assert_eq!(tree.count(), 4);
        let kinds: Vec<&str> = tree.walk().iter().map(|n| n.kind.name()).collect();
        assert_eq!(kinds, ["col", "row", "text", "text"]);
    }
}
