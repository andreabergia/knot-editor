//! Extension packages, their runtime lifecycle, and the application surfaces
//! they reach: buffers, semantic contributions, and record storage.

pub(super) mod buffers;
pub(super) mod bundled;
pub(super) mod host;
pub(super) mod load;
pub(crate) mod package;
pub(super) mod semantics;
pub(super) mod storage;
