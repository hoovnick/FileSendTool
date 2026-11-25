//! Non-UI guts of FST: the share table, the HTTP server and the tunnel.
//!
//! Split out from the binary so tests and example harnesses can drive the
//! server without standing up a window.

pub mod server;
pub mod share;
pub mod tunnel;
