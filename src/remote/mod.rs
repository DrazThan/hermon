//! Wire protocol between a host `hermon` process and a remote agent (M10).
//!
//! This module owns only the message shapes and their line-delimited JSON
//! encode/decode helpers — no I/O, no process spawning. See [`proto`].

pub mod proto;
