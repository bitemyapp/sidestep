//! Core Animation's layer trees as plain data, animated and composited on
//! the render thread: layer properties ([`props`]), animations
//! ([`spec`], [`keyframe`], [`math`]), the render thread's copy of each
//! window's trees ([`tree`]) and compositing a tree into drawing ops
//! ([`render`]). `sidestep-appkit`'s `quartzcore` builds `CALayer` and
//! `CAAnimation` on them and sends commits.

pub mod keyframe;
pub mod math;
pub mod props;
pub mod render;
pub mod spec;
pub mod tree;
