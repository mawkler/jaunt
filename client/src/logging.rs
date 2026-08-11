use anyhow::Context;
use log::LevelFilter;

const DEFAULT_LEVEL: LevelFilter = LevelFilter::Info;

/// Initializes the global logger.
///
/// # Errors
///
/// Returns an error if the log file could not be created or if a logger was
/// already initialized.
pub fn init(log_file: Option<&std::path::Path>) -> anyhow::Result<()> {
    let level = match std::env::var("RUST_LOG") {
        Ok(level) => level
            .parse()
            .inspect_err(|_| {
                eprintln!(
                    "$RUST_LOG is set to an invalid level '{level}', defaulting to {DEFAULT_LEVEL}"
                );
            })
            .unwrap_or(DEFAULT_LEVEL),
        Err(_) => DEFAULT_LEVEL,
    };

    let mut dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            let format = chrono::Local::now().format("%H:%M:%S%.3f");
            let level = record.level();
            let target = record.target();
            out.finish(format_args!("{format} [{level}][{target}] {message}"));
        })
        .level(level)
        .chain(std::io::stderr());

    if let Some(path) = log_file {
        let file = fern::log_file(path).context("failed to create log file")?;
        dispatch = dispatch.chain(file);
    }

    dispatch.apply().context("failed to initialize logging")?;

    Ok(())
}
