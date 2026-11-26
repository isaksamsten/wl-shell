//! Declarative UI for plugins: a tree of rows, columns, grids, stacks
//! and components (`tree`), laid out with taffy (`layout`) and painted
//! with tiny-skia and cosmic-text (`paint`), canvases included
//! (`canvas`). It runs on the worker (`Assets::ui`, `Assets::ui_icon`):
//! a plugin's tree goes in, an image and the rects of the nodes the
//! host tracks come out.
//!
//! The tree is plain data and knows nothing of Lua; wl-shell-lua builds
//! it from a plugin's `dock.ui` constructors. Colors are the theme's by
//! default (`Theme`, from the host's appearance), so a tree looks native
//! without styling anything.

pub mod anim;
pub mod canvas;
pub mod images;
pub mod layout;
pub mod paint;
pub mod tree;

pub use anim::{Player, Sheet, render_sheet};
pub use canvas::{Cap, Cmd, Ink, Seg};
pub use layout::{Bounds, Laid, Rect};
pub use paint::{Hit, LivePart, NoResources, Rendered, Request, Resources, State, render};
pub use tree::{
    Align, Border, BoxVariant, ButtonVariant, Color, Drive, Edges, Fit, Frames, Handlers, Justify,
    Kind, Layout, Length, Live, Look, Node, Place, TextAlign, TextStyle, Theme, ThemeColor, Track,
};
