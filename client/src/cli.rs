use std::path::PathBuf;

#[derive(clap::Parser, Debug)]
#[command(
    author,
    version,
    about,
    arg_required_else_help = true,
    subcommand_negates_reqs = true,
    after_help = "Examples:\
        \n  raisin ghostty \
        \n  raisin ghostty com.mitchellh.ghostty \
        \n  raisin forward ghostty \
        \n  raisin daemon \
        "
)]
/// Run-or-raise for Hyprland and Niri
pub struct Args {
    /// Command to run the application (e.g., `ghostty`).

    // This is an Option because of a clap issue where it thinks the
    // argument hasn't been provided, even when it has
    // TODO: check if it's possible to improve this. I'm guessing it's because we still have non-GUI
    // support
    #[arg(required = true)]
    pub app: Option<String>,
    /// Window app ID to match (e.g., `com.mitchellh.ghostty`). Optional.
    ///
    /// If omitted, the app name is used as a substring to match against
    /// window class names.
    pub app_id: Option<String>,

    /// Path to write logs to (in addition to stderr).
    #[arg(long)]
    pub log_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Show the window switcher for the application, cycling forward on
    /// subsequent invocations.
    Forward {
        /// Command to run the application (e.g., `ghostty`).
        app: String,
        /// Window app ID to match (e.g., `com.mitchellh.ghostty`). Optional.
        ///
        /// If omitted, the app name is used as a substring to match against
        /// window class names.
        app_id: Option<String>,
    },
    /// Show the window switcher for the application, cycling backward on
    /// subsequent invocations.
    Backward {
        /// Command to run the application (e.g., `ghostty`).
        app: String,
        /// Window app ID to match (e.g., `com.mitchellh.ghostty`). Optional.
        ///
        /// If omitted, the app name is used as a substring to match against
        /// window class names.
        app_id: Option<String>,
    },
    /// Confirm the current switcher selection (internal: called by compositor release bind).
    #[command(hide = true)]
    Confirm,
    /// Start the long-running daemon that owns the window switcher GUI.
    Daemon {
        /// Arguments passed through to the daemon binary.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        passthrough: Vec<String>,
    },
}
