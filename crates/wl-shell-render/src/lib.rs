//! Software rendering shared by wl-shell programs: images, text, shadows and
//! app icons, and a worker thread that does the slow parts. Everything here
//! produces premultiplied ARGB8888 [`Image`]s and knows nothing about
//! Wayland.

pub mod icons;
pub mod image;
pub mod shadow;
pub mod text;
pub mod worker;

pub use image::Image;
