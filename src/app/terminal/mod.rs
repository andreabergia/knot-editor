//! Native terminal sessions and their views over `alacritty_terminal`.

pub(super) mod colors;
pub(super) mod input;
pub(super) mod layout;
pub(super) mod links;
pub(super) mod mouse;
pub(super) mod session;
#[cfg(all(test, unix))]
mod session_tests;
pub(super) mod view;
