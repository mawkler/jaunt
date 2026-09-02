use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::thread;

use anyhow::{Context, Result};

#[allow(dead_code)]
fn raisin_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            // Prefer the canonical `raisin` binary next to `raisin-daemon` if it exists,
            // so the compositor `exec` doesn't depend on the daemon's temp build path.
            let dir = exe.parent()?;
            let raisin = dir.join("raisin");
            if raisin.exists() {
                Some(raisin)
            } else {
                Some(exe)
            }
        })
        .map_or_else(|| "raisin".to_string(), |p| p.display().to_string())
}

fn run_hyprctl(args: &[&str]) -> Result<()> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .with_context(|| format!("failed to run hyprctl {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!("hyprctl {} failed: {}{}", args.join(" "), stdout, stderr);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.starts_with("warning:") {
        log::warn!("hyprctl {} warning: {}", args.join(" "), stdout.trim());
    }
    Ok(())
}

/// Hard-coded Hyprland binds: `SUPER + key` where `key` is `a-z`.
/// For now just `SUPER + i` → `brave`. Extend this list to add more.
#[derive(Clone, Copy)]
struct Bind {
    /// Keys in Lua format, e.g. `"SUPER + i"`.
    keys: &'static str,
    /// Command to execute.
    command: &'static str,
}

const BINDS: &[Bind] = &[Bind {
    keys: "SUPER + i",
    command: "brave",
}];

fn add_lua(bind: &Bind) -> Result<()> {
    let lua = format!(
        r#"hl.bind("{}", hl.dsp.exec_cmd("{}"))"#,
        bind.keys, bind.command
    );
    run_hyprctl(&["eval", &lua])
}

fn add_legacy(bind: &Bind) -> Result<()> {
    // Legacy hyprlang: `keyword bind MODS,KEY,exec,CMD`
    // Convert `"SUPER + i"` → `"SUPER,i"`.
    let spec = format!("{},exec,{}", bind.keys.replace(" + ", ","), bind.command);
    run_hyprctl(&["keyword", "bind", &spec])
}

fn remove_lua(bind: &Bind) -> Result<()> {
    let lua = format!(r#"hl.unbind("{}")"#, bind.keys);
    run_hyprctl(&["eval", &lua])
}

fn remove_legacy(bind: &Bind) -> Result<()> {
    let spec = bind.keys.replace(" + ", ",");
    run_hyprctl(&["keyword", "unbind", &spec])
}

fn hypr_socket2_path() -> Option<std::path::PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(std::path::PathBuf::from(runtime).join(format!("hypr/{sig}/.socket2.sock")))
}

/// Ephemeral Hyprland binds defined in [`BINDS`].
/// Created via IPC at daemon start, removed on drop/reload.
pub struct BindManager {
    binds: Vec<Bind>,
}

impl BindManager {
    #[allow(clippy::unnecessary_wraps)]
    pub fn install() -> Result<Self> {
        // Clean up old switcher confirms that are no longer in BINDS,
        // so `SUPER + Super_L/R` from the previous implementation don't linger.
        for keys in ["SUPER + Super_L", "SUPER + Super_R"] {
            let _ = run_hyprctl(&["eval", &format!(r#"hl.unbind("{keys}")"#)]);
            let spec = keys.replace(" + ", ",");
            let _ = run_hyprctl(&["keyword", "unbind", &spec]);
        }

        let binds = BINDS.to_vec();
        for bind in &binds {
            // Clean up stale instance before (re-)installing. Best-effort.
            let _ = remove_lua(bind);
            let _ = remove_legacy(bind);
            // Try Lua first (0.55+), fall back to legacy hyprlang.
            if add_lua(bind).is_ok() {
                log::info!(
                    "installed Hyprland Lua bind {} -> {}",
                    bind.keys,
                    bind.command
                );
                continue;
            }
            match add_legacy(bind) {
                Ok(()) => log::info!(
                    "installed Hyprland legacy bind {} -> {}",
                    bind.keys,
                    bind.command
                ),
                Err(e) => log::warn!("failed to install Hyprland bind {}: {e:#}", bind.keys),
            }
        }
        Ok(Self { binds })
    }

    fn remove_all(&self) {
        for bind in &self.binds {
            if remove_lua(bind).is_ok() {
                log::info!("removed Hyprland Lua bind {}", bind.keys);
                continue;
            }
            match remove_legacy(bind) {
                Ok(()) => log::info!("removed Hyprland legacy bind {}", bind.keys),
                Err(e) => log::debug!("failed to remove Hyprland bind {}: {e:#}", bind.keys),
            }
        }
    }
}

impl Drop for BindManager {
    fn drop(&mut self) {
        self.remove_all();
    }
}

/// Spawn a background thread that re-installs the binds after `hyprctl reload`
/// (which clears ephemeral `keyword`/`hl.bind` binds).
pub fn spawn_reload_watcher() {
    let Some(path) = hypr_socket2_path() else {
        log::debug!("hyprland socket2 not found, reload watcher not started");
        return;
    };
    thread::spawn(move || {
        loop {
            let stream = match UnixStream::connect(&path) {
                Ok(s) => s,
                Err(e) => {
                    log::debug!("hyprland socket2 connect failed: {e:#}, retrying in 5s");
                    thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };
            let reader = BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("configreloaded") {
                    log::info!("Hyprland config reloaded, re-installing binds");
                    for bind in BINDS {
                        let _ = remove_lua(bind);
                        let _ = remove_legacy(bind);
                        if add_lua(bind).is_ok() {
                            log::info!("re-installed Hyprland Lua bind {}", bind.keys);
                        } else if add_legacy(bind).is_ok() {
                            log::info!("re-installed Hyprland legacy bind {}", bind.keys);
                        }
                    }
                }
            }
            log::debug!("hyprland socket2 disconnected, reconnecting in 2s");
            thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}
