//! Wire protocol between a host `hermon` process and a remote agent (M10).
//!
//! See [`proto`] for the wire format both halves share, [`agent`] for the
//! in-container half that streams frames, [`source`] for the host half that
//! spawns a transport, demuxes those frames, and presents the remote as one
//! more [`crate::source::Source`], [`spec`] for turning a user's `--remote`
//! flag into the `Command` `source` spawns, and [`discover`] for
//! `--docker-auto`'s label-driven discovery of the same.

pub mod agent;
pub mod discover;
pub mod proto;
pub mod source;
pub mod spec;
