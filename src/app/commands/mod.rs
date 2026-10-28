//! The command catalog, dispatch, keymaps, extension invocation, and native
//! handler declarations. Command bodies live beside the state they act on.

pub(crate) mod catalog;
pub(super) mod declarations;
pub(super) mod dispatch;
pub(super) mod extension;
pub(super) mod keymaps;
