//! Scene building blocks shared by wl-shell programs that draw in software
//! onto subsurfaces (wl-switcher, wl-dock):
//!
//! - [`anim`]: springs, for everything that moves.
//! - [`geom`]: rects in logical pixels, and their rounding for the protocol.
//! - [`sub`]: a sync subsurface with a viewport and an alpha multiplier.
//! - [`cache`]: ready shm buffers for icons, labels and shapes.
//! - [`shapes`]: panels, rounded rects, dots and card shadows, and how they
//!   are placed on subsurfaces.

pub mod anim;
pub mod cache;
pub mod geom;
pub mod shapes;
pub mod sub;

pub use anim::{Spring, lerp};
pub use cache::Cache;
pub use geom::{IRect, Rect};
pub use shapes::{Shape, ShapeKey};
pub use sub::Sub;
