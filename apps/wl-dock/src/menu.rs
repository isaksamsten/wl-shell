//! An item's menu: its rows, built from the item, its desktop entry and the
//! config, and how they are laid out in the speech bubble that points at
//! the icon. Pure, unit tested; menuview.rs shows it.
//!
//! Rows come in groups, top to bottom:
//!
//! 1. The app's windows, the focused one marked, when it has several.
//! 2. The desktop entry's actions (`[Desktop Action …]`).
//! 3. The app actions from its `[app.<id>]` table.
//! 4. The dock's: Keep in Dock or Remove from Dock, New Window (when
//!    running and launchable), Close or Close All Windows (with
//!    `close-command`).
//!
//! Empty groups are dropped. Groups are set apart by space; the dock's
//! group also gets a hairline above it, unless it comes first. Rows that
//! can't run aren't built at all.
//!
//! The Trash's menu (`trash`) has Open, and Empty Trash when it is full.
//! Empty Trash asks first: picking it turns the row into its `confirm`
//! text, and only picking it again runs it.
//!
//! A folder's menu (`folder`) has Open and Remove from Dock. Its popup
//! (`folder_popup`) is a menu too: a row per entry, with the entry's
//! thumbnail or type icon in a larger icon column, and Open in File
//! Manager. Its entries' rows can be dragged out as files. A folder with
//! `view = "stack"` gets the same rows as one group (`folder_stack`), cut
//! to what fits, with the folder's icon on Open in File Manager; stack.rs
//! lays them out instead.
//!
//! The layout is in logical pixels relative to the bubble's body: rows of
//! one height inside `padding`, an icon column when any row has an icon,
//! and as wide as the widest text (up to `max-width`). The tail sits on the
//! side facing the dock and points at the icon's center; the compositor
//! may slide the popup to keep it on the output, so where it points is
//! worked out from the popup's configured position (`configured_tail`),
//! and clamped so it never runs into a rounded corner.

use std::path::{Path, PathBuf};

use wl_shell_core::popup::{Anchor, Gravity};
use wl_shell_render::bubble::{self, Side};
use wl_shell_render::files::FileEntry;
use wl_shell_render::icons::DesktopAction;
use wl_shell_render::shadow;
use wl_shell_ui::Rect;

use crate::config::{AppAction, Position, When};
use crate::dock::Item;
use crate::strip::{Arrow, Key};

/// Text starts this far into a row (or its icon column does).
pub const INSET: f64 = 12.0;
/// A row's icon.
pub const ICON: f64 = 16.0;
/// A file's icon (or thumbnail).
pub const FILE_ICON: f64 = 32.0;
/// Above and below a file's icon in its row.
pub const FILE_ROW_PAD: f64 = 4.0;

/// Between the icon column and the text.
pub const ICON_GAP: f64 = 8.0;
/// The narrowest menu.
pub const MIN_WIDTH: f64 = 140.0;
/// How far the tail stays from where a rounded corner starts.
pub const CORNER_ROOM: f64 = 4.0;
/// Between the tail's tip and the icon.
pub const GAP: f64 = 4.0;

/// What picking a row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Activate this window.
    Activate(String),
    /// Launch the app again.
    NewWindow,
    /// Launch the desktop entry's action.
    Desktop(DesktopAction),
    /// Run an app action's command (not filled in yet).
    Command(String),
    Pin,
    Unpin,
    /// Close these windows.
    Close(Vec<String>),
    /// Open the Trash.
    OpenTrash,
    EmptyTrash,
    /// Open this file or folder (`open-command`).
    Open(PathBuf),
}

/// What the icon column shows for a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    /// An icon name or path.
    Icon(String),
    /// The focused window's dot.
    Focused,
    /// A file's thumbnail or icons (paths or names, best first), larger.
    File(Vec<String>),
}

impl Mark {
    /// The icon column's width for marks like this one.
    fn column(&self) -> f64 {
        match self {
            Mark::None => 0.0,
            Mark::File(_) => FILE_ICON,
            Mark::Icon(_) | Mark::Focused => ICON,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub mark: Mark,
    pub action: Action,
    /// Picking it first only shows this instead of the label; picking it
    /// again runs it.
    pub confirm: Option<String>,
    /// The file the row can be dragged out as.
    pub file: Option<PathBuf>,
}

/// A group of rows; `line`: a hairline above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub line: bool,
    pub entries: Vec<Entry>,
}

/// What a menu is built from.
pub struct Source<'a> {
    pub item: &'a Item,
    /// The focused window's ID, if known.
    pub focused: Option<&'a str>,
    /// The desktop entry's actions to show (none if disabled).
    pub desktop: &'a [DesktopAction],
    /// The app's `[app.<id>]` actions.
    pub actions: &'a [AppAction],
    /// The app can be launched (it has an entry or a launch command).
    pub can_launch: bool,
    /// `close-command` is set.
    pub can_close: bool,
}

/// The rows for `s`, in groups; empty groups are dropped.
pub fn build(s: &Source) -> Vec<Group> {
    let item = s.item;
    let windows: Vec<_> = item.windows.iter().chain(&item.elsewhere).collect();
    let running = !windows.is_empty();
    let mut groups = Vec::new();

    let mut rows = Vec::new();
    if windows.len() > 1 {
        for w in &windows {
            let label = match w.title.trim() {
                "" if item.app_id.is_empty() => "Untitled window".to_string(),
                "" => item.app_id.clone(),
                title => title.to_string(),
            };
            let focused = s.focused == Some(w.id.as_str());
            rows.push(Entry {
                label,
                mark: if focused { Mark::Focused } else { Mark::None },
                action: Action::Activate(w.id.clone()),
                confirm: None,
                file: None,
            });
        }
    }
    groups.push(Group {
        line: false,
        entries: rows,
    });

    let icon = |i: &Option<String>| i.clone().map_or(Mark::None, Mark::Icon);
    groups.push(Group {
        line: false,
        entries: (s.desktop.iter())
            .map(|a| Entry {
                label: a.name.clone(),
                mark: icon(&a.icon),
                action: Action::Desktop(a.clone()),
                confirm: None,
                file: None,
            })
            .collect(),
    });
    groups.push(Group {
        line: false,
        entries: (s.actions.iter())
            .filter(|a| running || a.when == When::Always)
            .filter(|a| !a.name.trim().is_empty() && !a.command.trim().is_empty())
            .map(|a| Entry {
                label: a.name.trim().to_string(),
                mark: icon(&a.icon),
                action: Action::Command(a.command.clone()),
                confirm: None,
                file: None,
            })
            .collect(),
    });

    let mut dock = Vec::new();
    if !item.app_id.is_empty() {
        dock.push(match item.pinned {
            true => entry("Remove from Dock", Action::Unpin),
            false => entry("Keep in Dock", Action::Pin),
        });
    }
    if running && s.can_launch {
        dock.push(entry("New Window", Action::NewWindow));
    }
    if running && s.can_close {
        let ids = windows.iter().map(|w| w.id.clone()).collect();
        let label = if windows.len() == 1 {
            "Close"
        } else {
            "Close All Windows"
        };
        dock.push(entry(label, Action::Close(ids)));
    }
    groups.push(Group {
        line: true,
        entries: dock,
    });

    groups.retain(|g| !g.entries.is_empty());
    if let Some(first) = groups.first_mut() {
        first.line = false;
    }
    groups
}

fn entry(label: &str, action: Action) -> Entry {
    Entry {
        label: label.to_string(),
        mark: Mark::None,
        action,
        confirm: None,
        file: None,
    }
}

/// The Trash's menu: Open, and Empty Trash (asking first) if it is
/// `full`.
pub fn trash(full: bool) -> Vec<Group> {
    let mut entries = vec![entry("Open", Action::OpenTrash)];
    if full {
        entries.push(Entry {
            confirm: Some("Empty Trash: click again".into()),
            ..entry("Empty Trash", Action::EmptyTrash)
        });
    }
    vec![Group {
        line: false,
        entries,
    }]
}

/// A folder's menu: Open, if it `exists`, and Remove from Dock.
pub fn folder(path: &Path, exists: bool) -> Vec<Group> {
    let mut groups = Vec::new();
    if exists {
        groups.push(Group {
            line: false,
            entries: vec![entry("Open", Action::Open(path.to_path_buf()))],
        });
    }
    groups.push(Group {
        line: exists,
        entries: vec![entry("Remove from Dock", Action::Unpin)],
    });
    groups
}

/// A folder's popup: a row per entry, then Open in File Manager.
pub fn folder_popup(path: &Path, files: &[FileEntry]) -> Vec<Group> {
    let entries: Vec<Entry> = (files.iter())
        .map(|f| Entry {
            label: f.name.clone(),
            mark: Mark::File(f.icons.clone()),
            action: Action::Open(f.path.clone()),
            confirm: None,
            file: Some(f.path.clone()),
        })
        .collect();
    let open = entry("Open in File Manager", Action::Open(path.to_path_buf()));
    let mut groups = vec![Group {
        line: true,
        entries: vec![open],
    }];
    if !entries.is_empty() {
        groups.insert(
            0,
            Group {
                line: false,
                entries,
            },
        );
    } else {
        groups[0].line = false;
    }
    groups
}

/// A folder's stack: like its popup, a row per entry (at most `max - 1`
/// of them), then Open in File Manager, here with `icons` (names, best
/// first) since every item of a stack has one. One group: a stack has no
/// hairlines.
pub fn folder_stack(
    path: &Path,
    files: &[FileEntry],
    max: usize,
    icons: Vec<String>,
) -> Vec<Group> {
    let shown = &files[..files.len().min(max.saturating_sub(1))];
    let mut groups = folder_popup(path, shown);
    let mut entries: Vec<Entry> = groups.drain(..).flat_map(|g| g.entries).collect();
    if let Some(open) = entries.last_mut() {
        open.mark = Mark::File(icons);
    }
    vec![Group {
        line: false,
        entries,
    }]
}

/// The rows of `groups`, in order.
pub fn entries(groups: &[Group]) -> Vec<&Entry> {
    groups.iter().flat_map(|g| &g.entries).collect()
}

/// Sizes, from `[appearance]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub row_height: f64,
    pub padding: f64,
    pub group_gap: f64,
    pub max_width: f64,
    pub radius: f64,
    /// The tail's base width and height.
    pub tail: (f64, f64),
}

impl Metrics {
    /// The widest a row's text gets, with an icon `column` wide (0: none).
    pub fn text_max(&self, column: f64) -> f64 {
        let column = if column > 0.0 { column + ICON_GAP } else { 0.0 };
        (self.max_width - 2.0 * self.padding - 2.0 * INSET - column).max(1.0)
    }
}

/// The icon column's width for `entries` (0: none).
pub fn column<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> f64 {
    (entries.into_iter()).fold(0.0, |c, e| c.max(e.mark.column()))
}

/// Where everything goes in the body, logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The body's size (whole pixels).
    pub w: f64,
    pub h: f64,
    /// Each row's top.
    pub rows: Vec<f64>,
    /// The hairlines' `y`.
    pub lines: Vec<f64>,
    /// The icon column's left edge, if there is one.
    pub icon_x: Option<f64>,
    /// Where text starts.
    pub text_x: f64,
    pub row_height: f64,
    /// The icon column's width.
    column: f64,
    padding: f64,
}

/// Lay out `groups` whose rows' texts are `widths` wide.
pub fn layout(groups: &[Group], widths: &[f64], m: &Metrics) -> Layout {
    let column = column(groups.iter().flat_map(|g| &g.entries));
    let icons = column > 0.0;
    // Rows of files are as tall as their icons need.
    let row_height = match column >= FILE_ICON {
        true => m.row_height.max(FILE_ICON + 2.0 * FILE_ROW_PAD),
        false => m.row_height,
    };
    let p = m.padding;
    let mut rows = Vec::new();
    let mut lines = Vec::new();
    let mut y = p;
    for (k, g) in groups.iter().enumerate() {
        if k > 0 {
            if g.line {
                lines.push(y + m.group_gap / 2.0);
            }
            y += m.group_gap;
        }
        for _ in &g.entries {
            rows.push(y);
            y += row_height;
        }
    }
    let gap = if icons { column + ICON_GAP } else { 0.0 };
    let widest = (widths.iter()).fold(0.0_f64, |a, w| a.max(w.ceil()));
    let content = 2.0 * p + 2.0 * INSET + gap + widest.min(m.text_max(column));
    // Room for the tail between the corners, on any side.
    let tail_room = 2.0 * (m.radius + CORNER_ROOM) + m.tail.0 + 2.0 * bubble::JOIN;
    let w = content
        .max(MIN_WIDTH)
        .max(tail_room)
        .min(m.max_width.max(MIN_WIDTH));
    Layout {
        w: w.ceil(),
        h: (y + p).ceil(),
        rows,
        lines,
        icon_x: icons.then_some(p + INSET),
        text_x: p + INSET + gap,
        row_height,
        column,
        padding: p,
    }
}

impl Layout {
    /// Row `i`'s rect (what its highlight covers).
    pub fn row(&self, i: usize) -> Option<Rect> {
        let y = *self.rows.get(i)?;
        Some(Rect::new(
            self.padding,
            y,
            self.w - 2.0 * self.padding,
            self.row_height,
        ))
    }

    /// The row at (`x`, `y`), if any.
    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        (0..self.rows.len()).find(|&i| self.row(i).is_some_and(|r| r.contains(x, y)))
    }

    /// Where row `i`'s `w` x `h` text goes: centered across the row.
    pub fn text(&self, i: usize, w: f64, h: f64) -> Option<Rect> {
        let r = self.row(i)?;
        Some(Rect::new(
            self.text_x,
            (r.y + (r.h - h) / 2.0).round(),
            w,
            h,
        ))
    }

    /// Where row `i`'s `size` x `size` mark goes, centered in the column.
    pub fn mark(&self, i: usize, size: f64) -> Option<Rect> {
        let (x, r) = (self.icon_x?, self.row(i)?);
        Some(Rect::new(
            (x + (self.column - size) / 2.0).round(),
            (r.y + (r.h - size) / 2.0).round(),
            size,
            size,
        ))
    }

    /// The hairlines, inset from both sides of the body.
    pub fn separators(&self) -> Vec<bubble::Separator> {
        (self.lines.iter())
            .map(|&y| bubble::Separator {
                y,
                x0: self.padding + INSET / 2.0,
                x1: self.w - self.padding - INSET / 2.0,
            })
            .collect()
    }
}

/// The side of the bubble that faces a dock at `position`.
pub fn side(position: Position) -> Side {
    match position {
        Position::Bottom => Side::Bottom,
        Position::Left => Side::Left,
        Position::Right => Side::Right,
    }
}

/// The tail's center along an edge `len` long starting at `start`, for
/// an icon centered at `icon` (all along the same axis): pointing at the
/// icon, but never closer than `CORNER_ROOM` to a rounded corner. On an
/// edge too short for that, centered.
pub fn tail_at(icon: f64, start: f64, len: f64, radius: f64, tail_width: f64) -> f64 {
    let lo = radius + CORNER_ROOM + tail_width / 2.0 + bubble::JOIN;
    let hi = len - lo;
    if lo > hi {
        return len / 2.0;
    }
    (icon - start).clamp(lo, hi)
}

/// Where a menu goes relative to the icon it was opened from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub position: Position,
    /// The icon on the dock surface.
    pub icon: Rect,
    /// The bubble's image, relative to the body (`Bubble::bounds`).
    pub bounds: shadow::Rect,
    /// The body's size.
    pub body: (f64, f64),
    pub tail_height: f64,
}

impl Place {
    /// The body's top left corner on the popup's surface.
    pub fn body_origin(&self) -> (f64, f64) {
        (-self.bounds.x as f64, -self.bounds.y as f64)
    }

    /// The popup's surface size.
    pub fn size(&self) -> (i32, i32) {
        (self.bounds.w.max(1), self.bounds.h.max(1))
    }

    /// The positioner's anchor (on the icon), gravity and offset: the
    /// body centered on the icon along the dock, the tail's tip `GAP` from
    /// the icon across it.
    pub fn positioner(&self) -> (Anchor, Gravity, (i32, i32)) {
        let (iw, ih) = (self.bounds.w as f64, self.bounds.h as f64);
        let (ox, oy) = self.body_origin();
        let (w, h) = self.body;
        let t = self.tail_height;
        let (anchor, gravity, x, y) = match self.position {
            // The popup's bottom center at the icon's top center.
            Position::Bottom => (
                Anchor::Top,
                Gravity::Top,
                iw / 2.0 - (ox + w / 2.0),
                ih - (oy + h + t) - GAP,
            ),
            // Its left (or right) middle at the icon's right (or left).
            Position::Left => (
                Anchor::Right,
                Gravity::Right,
                GAP - (ox - t),
                ih / 2.0 - (oy + h / 2.0),
            ),
            Position::Right => (
                Anchor::Left,
                Gravity::Left,
                iw - (ox + w + t) - GAP,
                ih / 2.0 - (oy + h / 2.0),
            ),
        };
        (anchor, gravity, (x.round() as i32, y.round() as i32))
    }

    /// Where the tail goes along its edge with the popup at `at` on the
    /// dock surface (from its configure), so it points at the icon.
    pub fn configured_tail(&self, at: (f64, f64), radius: f64, tail_width: f64) -> f64 {
        let (ox, oy) = self.body_origin();
        let (cx, cy) = self.icon.center();
        let (w, h) = self.body;
        match self.position {
            Position::Bottom => tail_at(cx, at.0 + ox, w, radius, tail_width),
            Position::Left | Position::Right => tail_at(cy, at.1 + oy, h, radius, tail_width),
        }
    }
}

/// What a key does on the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Pick,
    Close,
}

impl MenuKey {
    pub fn from_key(key: Key) -> Option<Self> {
        match key {
            Key::Move(Arrow::Up) => Some(MenuKey::Up),
            Key::Move(Arrow::Down) => Some(MenuKey::Down),
            Key::Activate => Some(MenuKey::Pick),
            Key::Close => Some(MenuKey::Close),
            Key::Move(_) | Key::Pick(_) => None,
        }
    }
}

/// The row after (`down`) or before `current` of `n`, wrapping around;
/// from nothing, the first (or last).
pub fn step(current: Option<usize>, n: usize, down: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    Some(match (current, down) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n.max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::{Kind, Window};

    fn window(id: &str, title: &str) -> Window {
        Window {
            id: id.into(),
            title: title.into(),
        }
    }

    fn item(pinned: bool, windows: &[Window], elsewhere: &[Window]) -> Item {
        Item {
            app_id: "app".into(),
            kind: Kind::App,
            pinned,
            windows: windows.to_vec(),
            elsewhere: elsewhere.to_vec(),
            active: false,
        }
    }

    fn action(name: &str, when: When) -> AppAction {
        AppAction {
            name: name.into(),
            command: format!("run {name}"),
            icon: None,
            when,
        }
    }

    fn desktop(name: &str) -> DesktopAction {
        DesktopAction {
            id: name.into(),
            name: name.into(),
            icon: Some("icon".into()),
            exec: name.into(),
        }
    }

    fn source<'a>(
        item: &'a Item,
        desktop: &'a [DesktopAction],
        actions: &'a [AppAction],
    ) -> Source<'a> {
        Source {
            item,
            focused: None,
            desktop,
            actions,
            can_launch: true,
            can_close: true,
        }
    }

    fn labels(groups: &[Group]) -> Vec<Vec<&str>> {
        (groups.iter())
            .map(|g| g.entries.iter().map(|e| e.label.as_str()).collect())
            .collect()
    }

    #[test]
    fn every_group() {
        let it = item(
            true,
            &[window("1", "Inbox"), window("2", "")],
            &[window("3", "Other")],
        );
        let d = [desktop("Compose")];
        let a = [action("Sync", When::Running), action("Check", When::Always)];
        let mut s = source(&it, &d, &a);
        s.focused = Some("2");
        let g = build(&s);
        assert_eq!(
            labels(&g),
            [
                vec!["Inbox", "app", "Other"],
                vec!["Compose"],
                vec!["Sync", "Check"],
                vec!["Remove from Dock", "New Window", "Close All Windows"],
            ]
        );
        // Only the dock's group gets a line.
        assert_eq!(
            g.iter().map(|g| g.line).collect::<Vec<_>>(),
            [false, false, false, true]
        );
        assert_eq!(g[0].entries[1].mark, Mark::Focused);
        assert_eq!(g[0].entries[2].action, Action::Activate("3".into()));
        assert_eq!(g[1].entries[0].mark, Mark::Icon("icon".into()));
        assert_eq!(g[2].entries[0].action, Action::Command("run Sync".into()));
        assert_eq!(
            g[3].entries[2].action,
            Action::Close(vec!["1".into(), "2".into(), "3".into()])
        );
    }

    #[test]
    fn rows_that_cant_run_are_left_out() {
        // Not running: no windows, New Window, Close, or running-only
        // actions.
        let it = item(false, &[], &[]);
        let a = [action("Sync", When::Running), action("Check", When::Always)];
        let g = build(&source(&it, &[], &a));
        assert_eq!(labels(&g), [vec!["Check"], vec!["Keep in Dock"]]);
        // The first group never gets a line, so no line is drawn first.
        assert!(!g[0].line && g[1].line);
        // One window: no window rows; no close-command, no Close.
        let it = item(false, &[window("1", "Only")], &[]);
        let mut s = source(&it, &[], &[]);
        s.can_close = false;
        s.can_launch = false;
        let g = build(&s);
        assert_eq!(labels(&g), [vec!["Keep in Dock"]]);
        assert!(!g[0].line);
        s.can_close = true;
        assert_eq!(labels(&build(&s)), [vec!["Keep in Dock", "Close"]]);
        // Without an app ID there is nothing to pin.
        let mut nameless = item(false, &[], &[]);
        nameless.app_id.clear();
        assert!(build(&source(&nameless, &[], &[])).is_empty());
    }

    #[test]
    fn separators_only_between_non_empty_groups() {
        let m = metrics();
        let it = item(false, &[window("1", "a"), window("2", "b")], &[]);
        for (d, a) in [
            (vec![], vec![]),
            (vec![desktop("x")], vec![]),
            (vec![], vec![action("y", When::Always)]),
            (vec![desktop("x")], vec![action("y", When::Always)]),
        ] {
            let g = build(&source(&it, &d, &a));
            let l = layout(&g, &vec![50.0; entries(&g).len()], &m);
            // One line, before the dock's group, between its rows and the
            // last row of the group before.
            assert_eq!(l.lines.len(), 1);
            let dock = l.rows.len() - 3;
            assert!(l.rows[dock - 1] + m.row_height < l.lines[0]);
            assert!(l.lines[0] < l.rows[dock]);
        }
        // The dock's group alone: no line at all.
        let it = item(false, &[], &[]);
        let g = build(&source(&it, &[], &[]));
        assert!(layout(&g, &[50.0], &m).lines.is_empty());
    }

    fn metrics() -> Metrics {
        Metrics {
            row_height: 28.0,
            padding: 6.0,
            group_gap: 8.0,
            max_width: 320.0,
            radius: 12.0,
            tail: (10.0, 7.0),
        }
    }

    #[test]
    fn layout_sizes() {
        let m = metrics();
        let it = item(true, &[window("1", "a"), window("2", "b")], &[]);
        let g = build(&source(&it, &[], &[]));
        let l = layout(&g, &[60.0, 80.0, 120.0, 70.0, 90.0], &m);
        // 2 + 3 rows, a gap, and padding.
        assert_eq!(l.h, 6.0 + 5.0 * 28.0 + 8.0 + 6.0);
        // The widest text plus insets, no icon column.
        assert_eq!(l.w, 120.0 + 2.0 * 12.0 + 2.0 * 6.0);
        assert_eq!((l.icon_x, l.text_x), (None, 18.0));
        // Long texts are cut at the widest; short menus get a minimum.
        assert_eq!(layout(&g, &[900.0; 5], &m).w, 320.0);
        assert_eq!(layout(&g, &[10.0; 5], &m).w, MIN_WIDTH);
        // With icons, a column for them.
        let d = [desktop("x")];
        let g = build(&source(&it, &d, &[]));
        let l = layout(&g, &[100.0; 6], &m);
        assert_eq!((l.icon_x, l.text_x), (Some(18.0), 18.0 + 16.0 + 8.0));
        assert_eq!(l.mark(2, 16.0).unwrap().x, 18.0);
        assert_eq!(m.text_max(ICON), 320.0 - 12.0 - 24.0 - 24.0);
    }

    #[test]
    fn same_size_at_any_scale() {
        // Sizes are whole logical pixels, so the bubble is whole buffer
        // pixels at any scale, and nothing in the layout depends on it.
        let m = metrics();
        let it = item(true, &[window("1", "a"), window("2", "b")], &[]);
        let g = build(&source(&it, &[desktop("x")], &[]));
        let widths = [77.3, 101.6, 54.0, 66.6, 80.2, 99.9];
        let l = layout(&g, &widths, &m);
        assert_eq!(l, layout(&g, &widths.map(|w: f64| w.ceil()), &m));
        assert_eq!((l.w.fract(), l.h.fract()), (0.0, 0.0));
        for i in 0..widths.len() {
            let t = l.text(i, 50.0, 18.0).unwrap();
            assert_eq!(t.y.fract(), 0.0);
        }
    }

    #[test]
    fn hits_and_steps() {
        let m = metrics();
        let it = item(true, &[window("1", "a"), window("2", "b")], &[]);
        let g = build(&source(&it, &[], &[]));
        let l = layout(&g, &[50.0; 5], &m);
        assert_eq!(l.hit(50.0, 7.0), Some(0));
        assert_eq!(l.hit(50.0, 6.0 + 28.0 + 1.0), Some(1));
        // The gap, the padding and the sides are no row.
        assert_eq!(l.hit(50.0, 6.0 + 2.0 * 28.0 + 1.0), None);
        assert_eq!(l.hit(50.0, 2.0), None);
        assert_eq!(l.hit(3.0, 20.0), None);
        assert_eq!(l.hit(50.0, l.rows[2] + 1.0), Some(2));
        assert_eq!(step(None, 3, true), Some(0));
        assert_eq!(step(None, 3, false), Some(2));
        assert_eq!(step(Some(2), 3, true), Some(0));
        assert_eq!(step(Some(0), 3, false), Some(2));
        assert_eq!(step(Some(0), 0, true), None);
    }

    #[test]
    fn the_tail_stays_on_the_straight_part() {
        let (r, tw) = (12.0, 10.0);
        for len in [60.0, 100.0, 333.0] {
            for k in -50..=50 {
                let icon = k as f64 * len / 40.0;
                let at = tail_at(icon, 0.0, len, r, tw);
                // The tail's base and the curves joining it stay off the
                // corners.
                assert!(at - tw / 2.0 - bubble::JOIN >= r, "{len} {icon}");
                assert!(at + tw / 2.0 + bubble::JOIN <= len - r, "{len} {icon}");
                // Where it can, it points at the icon.
                if (r + 9.0 + 8.0..=len - r - 9.0 - 8.0).contains(&icon) {
                    assert_eq!(at, icon);
                }
            }
        }
        // Too short: centered.
        assert_eq!(tail_at(0.0, 0.0, 40.0, r, tw), 20.0);
    }

    fn place(position: Position) -> Place {
        let b = bubble::Bubble {
            w: 200.0,
            h: 150.0,
            radius: 12.0,
            side: side(position),
            tail_at: 0.0,
            tail_width: 10.0,
            tail_height: 7.0,
            fill: 0,
            border: 0,
            separators: Vec::new(),
            separator: 0,
            shadow: Some(shadow::Settings {
                color: 0x00000080,
                softness: 18.0,
                spread: 0,
                offset: (0, 4),
            }),
            scale: 1,
        };
        let icon = match position {
            Position::Bottom => Rect::new(500.0, 30.0, 48.0, 48.0),
            Position::Left => Rect::new(12.0, 400.0, 48.0, 48.0),
            Position::Right => Rect::new(40.0, 400.0, 48.0, 48.0),
        };
        Place {
            position,
            icon,
            bounds: b.bounds(),
            body: (200.0, 150.0),
            tail_height: 7.0,
        }
    }

    /// Where an unconstrained compositor puts the popup, on the dock
    /// surface.
    fn placed(p: &Place) -> (f64, f64) {
        let (anchor, _, (dx, dy)) = p.positioner();
        let (w, h) = (p.bounds.w as f64, p.bounds.h as f64);
        let i = p.icon;
        let (ax, ay) = match anchor {
            Anchor::Top => (i.x + i.w / 2.0, i.y),
            Anchor::Right => (i.right(), i.y + i.h / 2.0),
            Anchor::Left => (i.x, i.y + i.h / 2.0),
            _ => unreachable!(),
        };
        let (x, y) = match p.position {
            Position::Bottom => (ax - w / 2.0, ay - h),
            Position::Left => (ax, ay - h / 2.0),
            Position::Right => (ax - w, ay - h / 2.0),
        };
        (x + dx as f64, y + dy as f64)
    }

    /// The tail's tip on the dock surface, with the popup at `at`.
    fn tip(p: &Place, at: (f64, f64)) -> (f64, f64) {
        let along = p.configured_tail(at, 12.0, 10.0);
        let (ox, oy) = p.body_origin();
        let (w, h) = p.body;
        let (bx, by) = (at.0 + ox, at.1 + oy);
        match p.position {
            Position::Bottom => (bx + along, by + h + 7.0),
            Position::Left => (bx - 7.0, by + along),
            Position::Right => (bx + w + 7.0, by + along),
        }
    }

    #[test]
    fn the_tail_points_at_the_icon() {
        for position in [Position::Bottom, Position::Left, Position::Right] {
            let p = place(position);
            let at = placed(&p);
            let (tx, ty) = tip(&p, at);
            let (cx, cy) = p.icon.center();
            let i = p.icon;
            // At the icon's center, GAP away from it.
            match position {
                Position::Bottom => assert_eq!((tx, ty), (cx, i.y - GAP)),
                Position::Left => assert_eq!((tx, ty), (i.right() + GAP, cy)),
                Position::Right => assert_eq!((tx, ty), (i.x - GAP, cy)),
            }
            // Slid along the dock by the compositor: still at the icon.
            let slid = match position {
                Position::Bottom => (at.0 + 60.0, at.1),
                _ => (at.0, at.1 - 40.0),
            };
            let (sx, sy) = tip(&p, slid);
            assert_eq!((sx, sy), (tx, ty), "{position:?}");
            // Slid too far: clamped off the corner.
            let far = match position {
                Position::Bottom => (at.0 + 150.0, at.1),
                _ => (at.0, at.1 + 150.0),
            };
            let along = p.configured_tail(far, 12.0, 10.0);
            assert_eq!(along, 12.0 + CORNER_ROOM + 5.0 + bubble::JOIN);
        }
    }

    #[test]
    fn folder_menus() {
        let p = Path::new("/home/u/Downloads");
        let g = folder(p, true);
        assert_eq!(labels(&g), [vec!["Open"], vec!["Remove from Dock"]]);
        assert_eq!(g[0].entries[0].action, Action::Open(p.into()));
        assert!(g[1].line);
        let gone = folder(p, false);
        assert_eq!(labels(&gone), [vec!["Remove from Dock"]]);
        assert!(!gone[0].line);
    }

    #[test]
    fn folder_popups() {
        let p = Path::new("/d");
        let empty = folder_popup(p, &[]);
        assert_eq!(labels(&empty), [vec!["Open in File Manager"]]);
        assert!(!empty[0].line);
        let dir = std::env::temp_dir().join(format!("wl-dock-popup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let request = wl_shell_render::files::FolderRequest {
            dir: dir.clone(),
            sort: Default::default(),
            count: 5,
        };
        let files = wl_shell_render::files::list(&request, &Default::default()).unwrap();
        let g = folder_popup(&dir, &files);
        assert_eq!(labels(&g), [vec!["a.txt"], vec!["Open in File Manager"]]);
        assert!(!g[0].line && g[1].line);
        let row = &g[0].entries[0];
        assert_eq!(row.file.as_deref(), Some(dir.join("a.txt").as_path()));
        assert!(matches!(row.mark, Mark::File(_)));
        assert_eq!(g[1].entries[0].file, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folder_stacks() {
        let dir = std::env::temp_dir().join(format!("wl-dock-stack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let request = wl_shell_render::files::FolderRequest {
            dir: dir.clone(),
            sort: wl_shell_render::files::Sort::Name,
            count: 5,
        };
        let files = wl_shell_render::files::list(&request, &Default::default()).unwrap();
        let icons = vec!["system-file-manager".to_string()];
        let g = folder_stack(&dir, &files, 10, icons.clone());
        assert_eq!(labels(&g), [vec!["a", "b", "c", "Open in File Manager"]]);
        let open = &g[0].entries[3];
        assert_eq!(open.mark, Mark::File(icons.clone()));
        assert_eq!(open.action, Action::Open(dir.clone()));
        // Cut to fit, Open in File Manager kept.
        let g = folder_stack(&dir, &files, 3, icons.clone());
        assert_eq!(labels(&g), [vec!["a", "b", "Open in File Manager"]]);
        let g = folder_stack(&dir, &[], 1, icons);
        assert_eq!(labels(&g), [vec!["Open in File Manager"]]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_rows_are_taller() {
        let m = metrics();
        let file = Entry {
            label: "a.png".into(),
            mark: Mark::File(vec!["image-png".into()]),
            action: Action::Open("/d/a.png".into()),
            confirm: None,
            file: Some("/d/a.png".into()),
        };
        let g = vec![
            Group {
                line: false,
                entries: vec![file],
            },
            Group {
                line: true,
                entries: vec![entry("Open in File Manager", Action::Open("/d".into()))],
            },
        ];
        let l = layout(&g, &[50.0, 50.0], &m);
        assert_eq!(l.row_height, FILE_ICON + 2.0 * FILE_ROW_PAD);
        assert_eq!(l.text_x, 6.0 + INSET + FILE_ICON + ICON_GAP);
        // A file's icon fills the column; a small one is centered in it.
        assert_eq!(l.mark(0, FILE_ICON).unwrap().x, 18.0);
        assert_eq!(l.mark(1, ICON).unwrap().x, 18.0 + 8.0);
    }

    #[test]
    fn trash_menu() {
        assert_eq!(labels(&trash(false)), [vec!["Open"]]);
        let g = trash(true);
        assert_eq!(labels(&g), [vec!["Open", "Empty Trash"]]);
        // Emptying asks first; opening doesn't.
        assert_eq!(g[0].entries[0].confirm, None);
        assert_eq!(g[0].entries[1].action, Action::EmptyTrash);
        assert!(g[0].entries[1].confirm.is_some());
    }

    #[test]
    fn menu_keys() {
        assert_eq!(MenuKey::from_key(Key::Move(Arrow::Up)), Some(MenuKey::Up));
        assert_eq!(MenuKey::from_key(Key::Activate), Some(MenuKey::Pick));
        assert_eq!(MenuKey::from_key(Key::Close), Some(MenuKey::Close));
        assert_eq!(MenuKey::from_key(Key::Move(Arrow::Left)), None);
        assert_eq!(MenuKey::from_key(Key::Pick(1)), None);
    }
}
