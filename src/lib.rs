//! Ratty terminal runtime and rendering library.
//!
//! This crate provides the terminal runtime, scene integration, protocol handling and widget
//! plumbing for Ratty.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used)]

pub mod cli;
pub mod config;
mod direct_render;
pub mod inline;
pub mod keyboard;
pub mod kitty;
pub mod model;
pub mod mouse;
pub mod paths;
pub mod plugin;
pub mod present;
pub mod rendering;
pub mod rgp;
pub mod runtime;
pub mod scene;
pub mod systems;
pub mod terminal;
