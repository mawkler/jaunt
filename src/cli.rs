use std::path::PathBuf;

#[derive(clap::Parser, Debug)]
#[command(
    author,
    version,
    about,
    arg_required_else_help = true,
    subcommand_negates_reqs = true,
    after_help = "Examples:\
        \n  jaunt ghostty \
        \n  jaunt ghostty com.mitchellh.ghostty \
        \n  jaunt daemon \
        "
)]
/// Run-or-raise for Hyprland and Niri
pub(crate) struct Args {
    /// Command to run the application (e.g., `ghostty`).
    #[arg(required = true)]
    pub(crate) app: Option<String>,

    /// Window app_id to match (e.g., `com.mitchellh.ghostty`). Optional.
    ///
    /// If omitted, the app name is used as a substring to match against
    /// window class names.
    pub(crate) app_id: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
pub(crate) enum Command {
    /// Run the switcher. Hyprland only.
    ///
    /// Start it once, for example from your Hyprland startup configuration.
    /// It binds Super + a letter for every application it knows about, taking
    /// those letters over from your own configuration while it runs;
    /// `hyprctl reload` gives them back.
    ///
    /// Super and Escape are shared rather than taken over, so your own
    /// bindings on them keep working.
    Daemon {
        /// Configuration file to read instead of
        /// `$XDG_CONFIG_HOME/jaunt/config.toml`.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },

    /// Print a configuration file with every setting at its default.
    DefaultConfig {
        /// Write it to `$XDG_CONFIG_HOME/jaunt/config.toml` rather than
        /// printing it, as long as there isn't one there already.
        #[arg(long)]
        write: bool,
    },

    /// Switch to an application through the running switcher.
    Switch {
        /// Command to run the application (e.g., `ghostty`).
        app: String,

        /// Window app_id to match (e.g., `com.mitchellh.ghostty`). Optional.
        app_id: Option<String>,
    },
}
