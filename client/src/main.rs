use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::Result;
use clap::Parser;

use raisin::application::Application;
use raisin::{cli, ipc};

fn main() -> Result<()> {
    let args = cli::Args::parse();

    raisin::logging::init(args.log_file.as_deref())?;

    log::debug!("starting raisin with args: {args:#?}");

    match args.command {
        Some(cli::Command::Daemon { passthrough }) => exec_daemon(&passthrough),
        Some(cli::Command::Forward { app, app_id }) => {
            send_message(&ipc::Message::Forward { app, app_id })
        }
        Some(cli::Command::Backward { app, app_id }) => {
            send_message(&ipc::Message::Backward { app, app_id })
        }
        Some(cli::Command::Confirm) => send_message(&ipc::Message::Confirm),
        None => {
            let app = args
                .app
                .as_deref()
                .expect("clap enforces app when no subcommand is used");
            let compositor = raisin::compositor::detect()?;
            Application::new(compositor, app, args.app_id.as_deref()).run()
        }
    }
}

fn send_message(message: &ipc::Message) -> Result<()> {
    if ipc::send(message)? {
        return Ok(());
    }
    anyhow::bail!(
        "no raisin daemon is running; \
         start it with `raisin daemon` (e.g. in your compositor's startup config)"
    )
}

fn exec_daemon(passthrough: &[String]) -> Result<()> {
    let mut command = daemon_command(passthrough);
    let err = command.exec();
    Err(err.into())
}

fn daemon_command(passthrough: &[String]) -> Command {
    let mut command = match std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("raisin-daemon")))
        .filter(|path| path.exists())
    {
        Some(path) => Command::new(path),
        None => Command::new("raisin-daemon"),
    };
    command.args(passthrough);
    command
}
