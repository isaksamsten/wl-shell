//! Laying a tree out with taffy: flex for rows, columns, boxes, buttons
//! and scroll areas, grid for grids and stacks (every layer in the one
//! cell). Text is measured through the caller, with cosmic-text.
//!
//! Defaults, unless a node says otherwise:
//!
//! - A column stretches its children across; a row centers them.
//! - A stack centers its layers (`align`), but a layer with no size of
//!   its own (an image, an app icon, a canvas) fills the stack.
//! - Images fill their box; app icons and canvases fill it at their
//!   aspect ratio (square, the viewbox's), unless sized.
//! - A spacer takes what is left along its parent; a divider is 1 px
//!   across its parent.
//! - Sliders are 20 px tall and stretch along a column (else 120 px
//!   wide); toggles are 36 x 20; progress bars 6 px tall.
//! - Buttons and cards get the theme's padding, badges a small one.

use taffy::prelude::*;
use taffy::{LayoutInput, Overflow, Point as TPoint, compute_leaf_layout};

use super::tree::{self, Align, BoxVariant, Justify, Kind, Node, Theme, Track};

/// A rect in logical pixels, from the root's top left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// How big the root may be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bounds {
    /// Exactly this (a tile).
    Exact(f32, f32),
    /// As big as its content, at most this (a popup).
    Content { max_w: f32, max_h: f32 },
}

/// A tree as laid out: per node in `Node::walk` order, its rect, and how
/// tall its content is (more than the rect for a scroll area that
/// scrolls).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Laid {
    pub rects: Vec<Rect>,
    pub content: Vec<f32>,
}

impl Laid {
    /// The root's size.
    pub fn size(&self) -> (f32, f32) {
        self.rects.first().map_or((0.0, 0.0), |r| (r.w, r.h))
    }
}

/// Measures a text node: its size wrapped to `max` (None: unwrapped).
pub type MeasureText<'a> = dyn FnMut(&Node, Option<f32>) -> (f32, f32) + 'a;

/// The default size of `ui.slider`, `ui.toggle`, `ui.progress`.
pub const SLIDER_HEIGHT: f32 = 20.0;
pub const TOGGLE_SIZE: (f32, f32) = (36.0, 20.0);
pub const PROGRESS_HEIGHT: f32 = 6.0;

fn len(l: tree::Length) -> Dimension {
    match l {
        tree::Length::Px(v) => Dimension::length(v),
        tree::Length::Percent(p) => Dimension::percent(p / 100.0),
    }
}

fn len_auto(l: tree::Length) -> LengthPercentageAuto {
    match l {
        tree::Length::Px(v) => LengthPercentageAuto::length(v),
        tree::Length::Percent(p) => LengthPercentageAuto::percent(p / 100.0),
    }
}

fn align_items(a: Align) -> AlignItems {
    match a {
        Align::Start => AlignItems::START,
        Align::Center => AlignItems::CENTER,
        Align::End => AlignItems::END,
        Align::Stretch => AlignItems::STRETCH,
    }
}

fn justify(j: Justify) -> JustifyContent {
    match j {
        Justify::Start => JustifyContent::START,
        Justify::Center => JustifyContent::CENTER,
        Justify::End => JustifyContent::END,
        Justify::Between => JustifyContent::SPACE_BETWEEN,
        Justify::Around => JustifyContent::SPACE_AROUND,
        Justify::Evenly => JustifyContent::SPACE_EVENLY,
    }
}

fn track(t: Track) -> TrackSizingFunction {
    match t {
        Track::Px(v) => length(v),
        // At least nothing, so cells never push their column wider.
        Track::Fr(f) => minmax(length(0.0), fr(f)),
        Track::Auto => auto(),
    }
}

/// Whether `n` fills its box when nothing sizes it.
fn filler(n: &Node) -> bool {
    let no_size = n.layout.width.is_none() && n.layout.height.is_none();
    match &n.kind {
        Kind::Image { .. } | Kind::Canvas { .. } => no_size,
        Kind::AppIcon { size, .. } => no_size && size.is_none(),
        _ => false,
    }
}

/// Which way `n` lays out its children (true: in a row).
fn in_row(n: &Node) -> bool {
    matches!(n.kind, Kind::Row | Kind::Button { .. })
}

/// The taffy style of `n`, a child of `parent` (None: the root).
fn style(n: &Node, parent: Option<&Node>, theme: &Theme) -> Style {
    let mut s = Style::default();
    let l = &n.layout;
    let row_parent = parent.is_some_and(in_row);
    // What each kind is.
    match &n.kind {
        Kind::Col | Kind::Box { .. } | Kind::Scroll => {
            s.display = Display::Flex;
            s.flex_direction = FlexDirection::Column;
            s.align_items = AlignItems::STRETCH;
        }
        Kind::Row => {
            s.display = Display::Flex;
            s.flex_direction = FlexDirection::Row;
            s.align_items = AlignItems::CENTER;
        }
        Kind::Button { .. } => {
            s.display = Display::Flex;
            s.flex_direction = FlexDirection::Row;
            s.align_items = AlignItems::CENTER;
            s.justify_content = JustifyContent::CENTER;
            s.gap = Size {
                width: LengthPercentage::length(6.0),
                height: LengthPercentage::length(6.0),
            };
            let only_icons = !n.children.is_empty()
                && (n.children.iter()).all(|c| matches!(c.kind, Kind::Icon { .. }));
            let p = match only_icons {
                true => tree::Edges::all(theme.spacing * 0.75),
                false => tree::Edges::sym(theme.spacing * 0.75, theme.spacing * 1.5),
            };
            s.padding = edges(p);
        }
        Kind::Grid { columns } => {
            s.display = Display::Grid;
            s.grid_template_columns = columns.iter().map(|&t| track(t).into()).collect();
            s.align_content = AlignContent::START;
            s.align_items = AlignItems::STRETCH;
            s.justify_items = AlignItems::STRETCH;
        }
        Kind::Stack { place } => {
            s.display = Display::Grid;
            s.grid_template_columns = vec![track(Track::Fr(1.0)).into()];
            s.grid_template_rows = vec![track(Track::Fr(1.0)).into()];
            s.justify_items = align_items(place.x);
            s.align_items = align_items(place.y);
        }
        Kind::Spacer { size } => match size {
            Some(v) => match row_parent {
                true => s.size.width = Dimension::length(*v),
                false => s.size.height = Dimension::length(*v),
            },
            None => {
                s.flex_grow = 1.0;
                s.flex_shrink = 1.0;
            }
        },
        Kind::Divider => {
            s.align_self = Some(AlignItems::STRETCH);
            s.flex_shrink = 0.0;
            match row_parent {
                true => s.size.width = Dimension::length(1.0),
                false => s.size.height = Dimension::length(1.0),
            }
        }
        Kind::Text { .. } => {}
        Kind::Icon { size, .. } => {
            let v = size.unwrap_or(theme.icon_size);
            s.size = Size {
                width: Dimension::length(v),
                height: Dimension::length(v),
            };
            s.flex_shrink = 0.0;
        }
        Kind::AppIcon { size, .. } => {
            if let Some(v) = size {
                s.size = Size {
                    width: Dimension::length(*v),
                    height: Dimension::length(*v),
                };
            }
            s.aspect_ratio = Some(1.0);
        }
        Kind::Image { .. } => {}
        Kind::Canvas { viewbox, .. } => {
            if viewbox.0 > 0.0 && viewbox.1 > 0.0 {
                s.aspect_ratio = Some(viewbox.0 / viewbox.1);
            }
        }
        Kind::Slider { .. } => {
            s.size.height = Dimension::length(SLIDER_HEIGHT);
            s.min_size.width = LengthPercentageAuto::length(60.0);
            if row_parent {
                s.size.width = Dimension::length(120.0);
            }
            s.flex_shrink = 0.0;
        }
        Kind::Toggle { .. } => {
            s.size = Size {
                width: Dimension::length(TOGGLE_SIZE.0),
                height: Dimension::length(TOGGLE_SIZE.1),
            };
            s.flex_shrink = 0.0;
        }
        Kind::Progress { .. } => {
            s.size.height = Dimension::length(PROGRESS_HEIGHT);
            s.min_size.width = LengthPercentageAuto::length(40.0);
            if row_parent {
                s.size.width = Dimension::length(120.0);
            }
            s.flex_shrink = 0.0;
        }
    }
    if let Kind::Box {
        variant: Some(variant),
    } = &n.kind
    {
        s.padding = edges(match variant {
            BoxVariant::Card | BoxVariant::Inset => tree::Edges::all(theme.spacing),
            BoxVariant::Badge => tree::Edges::sym(1.0, 5.0),
        });
        if *variant == BoxVariant::Badge {
            s.align_items = AlignItems::CENTER;
            s.justify_content = JustifyContent::CENTER;
            s.min_size.width = LengthPercentageAuto::length(16.0);
        }
    }
    if matches!(n.kind, Kind::Scroll) {
        // It may be shorter than its content.
        s.overflow = TPoint {
            x: Overflow::Hidden,
            y: Overflow::Scroll,
        };
        s.scrollbar_width = 0.0;
    }
    // A stack's sizeless layers fill it.
    if parent.is_some_and(|p| matches!(p.kind, Kind::Stack { .. })) {
        s.grid_row = Line {
            start: line(1),
            end: auto(),
        };
        s.grid_column = Line {
            start: line(1),
            end: auto(),
        };
        if filler(n) {
            s.align_self = Some(AlignItems::STRETCH);
            s.justify_self = Some(AlignItems::STRETCH);
            s.aspect_ratio = None;
        }
    } else if filler(n) && parent.is_some() {
        s.align_self = Some(AlignItems::STRETCH);
        // An image fills its parent along its row or column too.
        if matches!(n.kind, Kind::Image { .. }) {
            s.flex_grow = 1.0;
        }
    }
    // What the node says.
    if let Some(w) = l.width {
        s.size.width = len(w);
    }
    if let Some(h) = l.height {
        s.size.height = len(h);
    }
    if let Some(v) = l.min_width {
        s.min_size.width = len_auto(v);
    }
    if let Some(v) = l.min_height {
        s.min_size.height = len_auto(v);
    }
    if let Some(v) = l.max_width {
        s.max_size.width = len_auto(v);
    }
    if let Some(v) = l.max_height {
        s.max_size.height = len_auto(v);
    }
    if l.grow > 0.0 {
        s.flex_grow = l.grow;
    }
    if let Some(g) = l.gap {
        let row = l.row_gap.unwrap_or(g);
        s.gap = Size {
            width: LengthPercentage::length(g),
            height: LengthPercentage::length(row),
        };
    } else if let Some(row) = l.row_gap {
        s.gap.height = LengthPercentage::length(row);
    }
    if let Some(p) = l.padding {
        s.padding = edges(p);
    }
    if let Some(a) = l.align {
        s.align_items = align_items(a);
    }
    if let Some(j) = l.justify {
        s.justify_content = justify(j);
    }
    // A border takes room inside the edge.
    if let Some(b) = n.look.border {
        let w = LengthPercentage::length(b.width.max(0.0));
        s.border = taffy::Rect {
            left: w,
            right: w,
            top: w,
            bottom: w,
        };
    }
    s
}

fn edges(e: tree::Edges) -> taffy::Rect<LengthPercentage> {
    taffy::Rect {
        left: LengthPercentage::length(e.left),
        right: LengthPercentage::length(e.right),
        top: LengthPercentage::length(e.top),
        bottom: LengthPercentage::length(e.bottom),
    }
}

/// Lay `root` out in `bounds`.
pub fn layout(root: &Node, bounds: Bounds, theme: &Theme, measure: &mut MeasureText) -> Laid {
    let nodes = root.walk();
    let mut taffy: TaffyTree<usize> = TaffyTree::with_capacity(nodes.len());
    let mut ids: Vec<NodeId> = Vec::with_capacity(nodes.len());
    // Post-order building: children first, so build recursively.
    fn build(
        taffy: &mut TaffyTree<usize>,
        n: &Node,
        parent: Option<&Node>,
        theme: &Theme,
        next: &mut usize,
        ids: &mut Vec<(usize, NodeId)>,
    ) -> NodeId {
        let index = *next;
        *next += 1;
        let style = style(n, parent, theme);
        let children: Vec<NodeId> = (n.children.iter())
            .map(|c| build(taffy, c, Some(n), theme, next, ids))
            .collect();
        let id = match children.is_empty() {
            true => taffy.new_leaf_with_context(style, index),
            false => taffy.new_with_children(style, &children),
        }
        .expect("taffy never fails to add a node");
        ids.push((index, id));
        id
    }
    let mut pairs = Vec::with_capacity(nodes.len());
    let mut next = 0;
    let root_id = build(&mut taffy, root, None, theme, &mut next, &mut pairs);
    pairs.sort_by_key(|(i, _)| *i);
    ids.extend(pairs.into_iter().map(|(_, id)| id));

    let available = match bounds {
        Bounds::Exact(w, h) => {
            let mut s = taffy.style(root_id).cloned().unwrap_or_default();
            s.size = Size {
                width: Dimension::length(w),
                height: Dimension::length(h),
            };
            let _ = taffy.set_style(root_id, s);
            Size {
                width: AvailableSpace::Definite(w),
                height: AvailableSpace::Definite(h),
            }
        }
        Bounds::Content { max_w, max_h } => {
            let mut s = taffy.style(root_id).cloned().unwrap_or_default();
            let cap = |cur: LengthPercentageAuto, max: f32| match cur
                .resolve_to_option(max, |_, _| 0.0)
            {
                Some(v) if v <= max => cur,
                _ => LengthPercentageAuto::length(max),
            };
            s.max_size.width = cap(s.max_size.width, max_w);
            s.max_size.height = cap(s.max_size.height, max_h);
            let _ = taffy.set_style(root_id, s);
            Size {
                width: AvailableSpace::Definite(max_w),
                height: AvailableSpace::MaxContent,
            }
        }
    };
    let measured = |input: LayoutInput, _: NodeId, ctx: Option<&mut usize>, s: &Style| {
        let node = ctx.and_then(|i| nodes.get(*i)).copied();
        let text = node.filter(|n| matches!(n.kind, Kind::Text { .. }));
        compute_leaf_layout(
            input,
            s,
            |_, _| 0.0,
            |known, avail| {
                let Some(node) = text else {
                    return Size {
                        width: known.width.unwrap_or(0.0),
                        height: known.height.unwrap_or(0.0),
                    };
                };
                if let (Some(width), Some(height)) = (known.width, known.height) {
                    return Size { width, height };
                }
                let max = known.width.or(match avail.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(0.0),
                    AvailableSpace::MaxContent => None,
                });
                let (w, h) = measure(node, max);
                Size {
                    width: known.width.unwrap_or(w),
                    height: known.height.unwrap_or(h),
                }
            },
        )
    };
    if let Err(e) = taffy.compute_layout_with_measure(root_id, available, measured) {
        wl_shell_log::warn!("ui layout: {e}");
    }

    // Absolute rects: each node's location is relative to its parent.
    let mut laid = Laid {
        rects: vec![Rect::default(); nodes.len()],
        content: vec![0.0; nodes.len()],
    };
    fn place(
        taffy: &TaffyTree<usize>,
        n: &Node,
        ids: &[NodeId],
        at: (f32, f32),
        next: &mut usize,
        laid: &mut Laid,
    ) {
        let index = *next;
        *next += 1;
        let l = taffy.layout(ids[index]).copied().unwrap_or_default();
        let (x, y) = (at.0 + l.location.x, at.1 + l.location.y);
        laid.rects[index] = Rect {
            x,
            y,
            w: l.size.width,
            h: l.size.height,
        };
        let mut bottom: f32 = 0.0;
        for c in &n.children {
            let ci = *next;
            place(taffy, c, ids, (x, y), next, laid);
            let r = laid.rects[ci];
            bottom = bottom.max(r.y + r.h - y);
        }
        laid.content[index] = (bottom + l.padding.bottom + l.border.bottom).max(l.size.height);
    }
    let mut next = 0;
    place(&taffy, root, &ids, (0.0, 0.0), &mut next, &mut laid);
    laid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tree::{Edges, Length, Place};

    /// Every character 7 x 16; wraps at spaces.
    fn measure(n: &Node, max: Option<f32>) -> (f32, f32) {
        let Kind::Text { text, .. } = &n.kind else {
            return (0.0, 0.0);
        };
        let words: Vec<usize> = text.split(' ').map(|w| w.chars().count()).collect();
        let max_chars = max.map_or(usize::MAX, |m| (m / 7.0).floor() as usize);
        let (mut lines, mut cur, mut widest) = (1, 0, 0);
        for w in words {
            let next = if cur == 0 { w } else { cur + 1 + w };
            if cur > 0 && next > max_chars {
                lines += 1;
                widest = widest.max(cur);
                cur = w;
            } else {
                cur = next;
            }
        }
        widest = widest.max(cur);
        (widest as f32 * 7.0, lines as f32 * 16.0)
    }

    fn lay(root: &Node, bounds: Bounds) -> Laid {
        layout(root, bounds, &Theme::default(), &mut measure)
    }

    fn sized(mut n: Node, w: f32, h: f32) -> Node {
        n.layout.width = Some(Length::Px(w));
        n.layout.height = Some(Length::Px(h));
        n
    }

    #[test]
    fn columns_and_rows() {
        let mut col = Node::new(Kind::Col).with_children(vec![
            Node::text("hello"),
            Node::new(Kind::Row).with_children(vec![
                sized(Node::new(Kind::Col), 10.0, 10.0),
                Node::new(Kind::Spacer { size: None }),
                sized(Node::new(Kind::Col), 20.0, 30.0),
            ]),
        ]);
        col.layout.gap = Some(4.0);
        col.layout.padding = Some(Edges::all(8.0));
        let l = lay(&col, Bounds::Exact(100.0, 100.0));
        assert_eq!(l.size(), (100.0, 100.0));
        // The text, stretched across inside the padding.
        assert_eq!(
            l.rects[1],
            Rect {
                x: 8.0,
                y: 8.0,
                w: 84.0,
                h: 16.0
            }
        );
        // The row below, its children centered across it.
        let row = l.rects[2];
        assert_eq!((row.y, row.h), (28.0, 30.0));
        assert_eq!((l.rects[3].x, l.rects[3].y), (8.0, 38.0));
        // The spacer pushes the last child to the end.
        assert_eq!(l.rects[5].x + l.rects[5].w, 92.0);
    }

    #[test]
    fn text_wraps_in_content_bounds() {
        let col = Node::new(Kind::Col).with_children(vec![Node::text("aaaa bbbb cccc")]);
        let wide = lay(
            &col,
            Bounds::Content {
                max_w: 500.0,
                max_h: 500.0,
            },
        );
        assert_eq!(wide.size(), (98.0, 16.0));
        let narrow = lay(
            &col,
            Bounds::Content {
                max_w: 70.0,
                max_h: 500.0,
            },
        );
        // Wrapped, and as wide as its widest line.
        assert_eq!(narrow.size(), (63.0, 32.0));
    }

    #[test]
    fn grids() {
        let cells: Vec<Node> = (0..10).map(|i| Node::text(i.to_string())).collect();
        let mut grid = Node::new(Kind::Grid {
            columns: vec![Track::Fr(1.0); 7],
        })
        .with_children(cells);
        grid.layout.gap = Some(2.0);
        let l = lay(&grid, Bounds::Exact(138.0, 100.0));
        // 7 columns of (138 - 12) / 7 = 18.
        assert_eq!(
            l.rects[1],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 18.0,
                h: 16.0
            }
        );
        assert_eq!(l.rects[2].x, 20.0);
        // The eighth wraps to the next row.
        assert_eq!((l.rects[8].x, l.rects[8].y), (0.0, 18.0));
    }

    #[test]
    fn stacks_fill_with_sizeless_layers() {
        let stack = Node::new(Kind::Stack {
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
            sized(Node::new(Kind::Box { variant: None }), 10.0, 10.0),
        ]);
        let l = lay(&stack, Bounds::Exact(64.0, 48.0));
        assert_eq!(
            l.rects[1],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 64.0,
                h: 48.0
            }
        );
        assert_eq!(
            l.rects[2],
            Rect {
                x: 54.0,
                y: 0.0,
                w: 10.0,
                h: 10.0
            }
        );
    }

    #[test]
    fn canvases_keep_their_aspect() {
        let mut c = Node::new(Kind::Canvas {
            viewbox: (100.0, 50.0),
            commands: Vec::new(),
            anim: None,
        });
        c.layout.width = Some(Length::Px(80.0));
        let col = Node::new(Kind::Col).with_children(vec![c]);
        let l = lay(
            &col,
            Bounds::Content {
                max_w: 300.0,
                max_h: 300.0,
            },
        );
        assert_eq!(
            l.rects[1],
            Rect {
                x: 0.0,
                y: 0.0,
                w: 80.0,
                h: 40.0
            }
        );
    }

    #[test]
    fn scroll_areas_cut_their_content() {
        let items: Vec<Node> = (0..10).map(|i| Node::text(format!("row {i}"))).collect();
        let mut scroll = Node::new(Kind::Scroll).with_children(items);
        scroll.layout.max_height = Some(Length::Px(50.0));
        let col = Node::new(Kind::Col).with_children(vec![scroll]);
        let l = lay(
            &col,
            Bounds::Content {
                max_w: 300.0,
                max_h: 300.0,
            },
        );
        assert_eq!(l.rects[1].h, 50.0);
        assert_eq!(l.content[1], 160.0);
        assert_eq!(l.size().1, 50.0);
    }

    #[test]
    fn controls_have_sizes() {
        let row = Node::new(Kind::Row).with_children(vec![
            Node::new(Kind::Toggle {
                value: true,
                enabled: true,
            }),
            Node::new(Kind::Slider {
                value: 0.5,
                min: 0.0,
                max: 1.0,
                enabled: true,
                live: None,
                transition: 0.0,
            }),
            Node::new(Kind::Divider),
        ]);
        let l = lay(
            &row,
            Bounds::Content {
                max_w: 400.0,
                max_h: 100.0,
            },
        );
        assert_eq!((l.rects[1].w, l.rects[1].h), TOGGLE_SIZE);
        assert_eq!((l.rects[2].w, l.rects[2].h), (120.0, SLIDER_HEIGHT));
        assert_eq!((l.rects[3].w, l.rects[3].h), (1.0, SLIDER_HEIGHT));
    }
}
