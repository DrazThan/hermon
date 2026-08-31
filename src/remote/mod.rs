//! Wire protocol between a host `hermon` process and a remote agent (M10).
//!
//! See [`proto`] for the wire format both halves share, [`agent`] for the
//! in-container half that streams frames, and [`source`] for the host half
//! that spawns a transport, demuxes those frames, and presents the remote
//! as one more [`crate::source::Source`].

pub mod agent;
pub mod proto;
pub mod source;
