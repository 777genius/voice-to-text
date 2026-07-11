//! Shared runtime for one realtime speech interpretation session.
//!
//! The core owns capture, translation and output ports through dedicated tasks. Facades provide
//! preflighted ports and callbacks without duplicating queueing, framing, supervision or cleanup.

mod frame_assembler;
mod runtime_supervisor;
mod session;

pub(crate) use runtime_supervisor::*;
pub(crate) use session::*;
