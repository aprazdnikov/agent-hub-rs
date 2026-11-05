//! Linux theme input from the desktop portal (winit does not provide it on Linux).

use eframe::egui;
use futures::StreamExt as _;
use std::{collections::HashMap, time::Duration};
use zbus::{Proxy, zvariant::OwnedValue};

const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
const DEADLINE: Duration = Duration::from_secs(3);
const RETRY: Duration = Duration::from_secs(30);

/// Runs on the existing application runtime and ends with it. Never blocks the UI.
pub fn follow_system(runtime: &tokio::runtime::Handle, ctx: egui::Context) {
    runtime.spawn(async move {
        loop {
            if let Err(error) = watch(&ctx).await {
                tracing::debug!(%error, "desktop theme portal unavailable; retrying in 30 seconds");
            }
            tokio::time::sleep(RETRY).await;
        }
    });
}

async fn watch(ctx: &egui::Context) -> anyhow::Result<()> {
    let (proxy, mut changes) = tokio::time::timeout(DEADLINE, async {
        let connection = zbus::Connection::session().await?;
        let proxy = Proxy::new_owned(
            connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )
        .await?;
        // Subscribe before the read so a concurrent theme change cannot be lost.
        let changes = proxy.receive_signal("SettingChanged").await?;
        Ok::<_, zbus::Error>((proxy, changes))
    })
    .await??;
    refresh(&proxy, ctx).await?;
    let mut refresh_timer = tokio::time::interval(RETRY);
    refresh_timer.tick().await;
    loop {
        tokio::select! {
            signal = changes.next() => {
                let signal = signal.ok_or_else(|| anyhow::anyhow!("desktop settings stream closed"))?;
                let (namespace, key, value): (String, String, OwnedValue) = signal.body().deserialize()?;
                if namespace == APPEARANCE && key == COLOR_SCHEME {
                    apply_scheme(ctx, u32::try_from(&value)?);
                }
            }
            _ = refresh_timer.tick() => {
                // Also recovers changes missed while the portal restarts.
                refresh(&proxy, ctx).await?;
            }
        }
    }
}

async fn refresh(proxy: &Proxy<'_>, ctx: &egui::Context) -> anyhow::Result<()> {
    // ReadAll works with both portal v1 and v2, without Read's nested variant ambiguity.
    let values: HashMap<String, HashMap<String, OwnedValue>> =
        tokio::time::timeout(DEADLINE, proxy.call("ReadAll", &([APPEARANCE].as_slice(),)))
            .await??;
    let scheme = values.get(APPEARANCE).and_then(|values| values.get(COLOR_SCHEME));
    apply_scheme(ctx, scheme.map_or(Ok(0), u32::try_from)?);
    Ok(())
}

fn apply_scheme(ctx: &egui::Context, scheme: u32) {
    // Portal: 1 = dark, 2 = light; 0 and unknown values mean no preference.
    let theme = if scheme == 1 { egui::Theme::Dark } else { egui::Theme::Light };
    let changed = ctx.options_mut(|options| {
        let changed = options.fallback_theme != theme;
        // Linux supplies no native theme input. Keep System selected and supply its fallback.
        options.fallback_theme = theme;
        changed
    });
    if changed {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_changes_apply_without_native_theme_events() {
        let ctx = egui::Context::default();
        super::super::configure(&ctx);
        for (scheme, expected) in [
            (2, egui::Theme::Light),
            (1, egui::Theme::Dark),
            (0, egui::Theme::Light),
            (99, egui::Theme::Light),
        ] {
            apply_scheme(&ctx, scheme);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(ui.visuals().dark_mode, expected == egui::Theme::Dark);
            });
            output.textures_delta.clear();
            assert_eq!(
                ctx.options(|options| options.theme_preference),
                egui::ThemePreference::System
            );
        }
    }
}
