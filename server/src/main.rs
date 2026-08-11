use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

use raisin::compositor;
use raisin::ipc;

mod gui;
mod input;
mod wayland;

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
    let listener = ipc::start_listener().context("failed to listen to socket")?;

    gui::run(compositor, listener)
}
