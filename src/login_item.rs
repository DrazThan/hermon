//! Launch-at-login integration for `hermon menubar` on macOS via LaunchAgent.
//!
//! This module provides `--install-login-item` and `--uninstall-login-item` to
//! write/remove a LaunchAgent plist and register it with launchctl.

use anyhow::{Result, bail};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const PLIST_ID: &str = "dev.hermon.menubar";
const PLIST_FILENAME: &str = "dev.hermon.menubar.plist";

/// Path where the LaunchAgent plist is stored.
fn plist_path() -> Result<PathBuf> {
    let config_dir =
        dirs::config_dir().ok_or_else(|| anyhow::anyhow!("cannot determine config directory"))?;
    let launch_agents = config_dir.join("hermon").join("LaunchAgents");
    Ok(launch_agents.join(PLIST_FILENAME))
}

/// Path to the hermon binary.
fn hermon_binary_path() -> Result<String> {
    let exe = std::env::current_exe()?;
    Ok(exe.to_string_lossy().to_string())
}

/// Generate the LaunchAgent plist content as a string.
fn plist_content(hermon_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{}</string>
	<key>Program</key>
	<string>{}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>menubar</string>
	</array>
	<key>KeepAlive</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>StandardOutPath</key>
	<string>$HOME/.local/share/hermon/menubar.log</string>
	<key>StandardErrorPath</key>
	<string>$HOME/.local/share/hermon/menubar-error.log</string>
</dict>
</plist>
"#,
        PLIST_ID, hermon_path, hermon_path
    )
}

/// Install the LaunchAgent and register it with launchctl.
pub fn install() -> Result<()> {
    let plist_path = plist_path()?;
    let hermon_path = hermon_binary_path()?;

    // Create directory if it doesn't exist
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Create log directory
    if let Some(home) = dirs::home_dir() {
        let log_dir = home.join(".local/share/hermon");
        fs::create_dir_all(log_dir)?;
    }

    // Write the plist
    let content = plist_content(&hermon_path);
    fs::write(&plist_path, content)?;

    // Unload the service first in case it's already loaded (idempotent)
    let _ = Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path)
        .output();

    // Load the service
    let output = Command::new("launchctl")
        .arg("load")
        .arg(&plist_path)
        .output()?;

    if !output.status.success() {
        bail!(
            "launchctl load failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    println!("✓ Launch at login installed. hermon menubar will start when you log in.");
    println!("  Plist: {}", plist_path.display());
    println!("  To uninstall, run: hermon menubar --uninstall-login-item");

    Ok(())
}

/// Uninstall the LaunchAgent and deregister it with launchctl.
pub fn uninstall() -> Result<()> {
    let plist_path = plist_path()?;

    // Unload the service first
    let output = Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path)
        .output()?;

    if !output.status.success() {
        // If it wasn't loaded, that's fine — proceed to delete the file
        eprintln!(
            "launchctl unload: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Delete the plist
    if plist_path.exists() {
        fs::remove_file(&plist_path)?;
    }

    println!("✓ Launch at login uninstalled.");
    println!("  Plist deleted: {}", plist_path.display());

    Ok(())
}
