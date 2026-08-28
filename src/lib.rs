//! hermon-rs: live tmux monitor deck for Hermes and Claude Code sessions.
//!
//! Greenfield rewrite of `hermon.py`, developed in the same repository so
//! issues and PRs stay connected to the Python implementation it replaces.

pub mod cli;
pub mod config;
pub mod engine;
pub mod notify;
pub mod render;
pub mod roster;
pub mod source;
pub mod ui;
pub mod view;

use anyhow::bail;
use clap::Parser;

use cli::{Cli, Command};

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Command::Watch(_args) => bail!("hermon watch: not yet implemented"),
        Command::Ls(_args) => bail!("hermon ls: not yet implemented"),
    }
}
