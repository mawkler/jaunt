use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

use raisin::compositor::{self, Compositor};
use raisin::ipc;

mod gui;
mod hypr_binds;
mod input;

#[derive(clap::Parser, Debug)]
#[command(
    author,
    version,
    about = "Long-running raisin server that owns the window switcher GUI"
)]
struct Args {
    /// Path to write logs to (in addition to stderr).
    #[arg(long)]
    log_file: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    raisin::logging::init(args.log_file.as_deref())?;

    if ipc::is_running() {
        anyhow::bail!("raisin daemon is already running");
    }

    log::info!("starting raisin daemon");

    gtk4::init().context("failed to initialize GTK")?;

    let compositor = compositor::detect()?;
    // Hyprland-only: auto-manage Super_L/R release binds that confirm the
    // switcher. Ephemeral (IPC, not config file), Lua-first with hyprlang
    // fallback. Leaked on purpose — Drop removes binds on exit, reload
    // watcher re-adds after `hyprctl reload`.
    let _bind_guard = if compositor.name() == "hyprland" {
        match hypr_binds::BindManager::install() {
            Ok(m) => {
                log::info!("installed Hyprland Super_L/R release binds");
                hypr_binds::spawn_reload_watcher();
                Some(m)
            }
            Err(e) => {
                log::warn!("failed to install Hyprland release binds, fallback to GTK: {e:#}");
                None
            }
        }
    } else {
        None
    };
    let listener = ipc::start_listener().context("failed to listen to socket")?;

    gui::run(compositor, listener)
}
