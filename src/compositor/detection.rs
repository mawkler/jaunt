use anyhow::Context;
use enum_dispatch::enum_dispatch;

use super::{Compositor, integrations};

#[enum_dispatch(Compositor)]
pub(crate) enum ActiveCompositor {
    Hyprland(integrations::hyprland::Compositor),
    Niri(integrations::niri::Compositor),
}

const COMPOSITORS: [ActiveCompositor; 2] = [
    ActiveCompositor::Hyprland(integrations::hyprland::Compositor),
    ActiveCompositor::Niri(integrations::niri::Compositor),
];

pub(crate) fn detect() -> anyhow::Result<ActiveCompositor> {
    if let Ok(env_compositor) = std::env::var("JAUNT_COMPOSITOR") {
        return COMPOSITORS
            .into_iter()
            .find(|compositor| compositor.name() == env_compositor.to_lowercase())
            .with_context(|| {
                format!(
                    "`$JAUNT_COMPOSITOR` is set to '{env_compositor}', \
                     which is not a supported compositor"
                )
            });
    }

    COMPOSITORS
        .into_iter()
        .find(Compositor::is_running)
        .context("could not find any supported compositor running")
}
