//! The tray icon and its menu (ADR-0026 §2, ADR-0027). It draws what `crisol-client` decided
//! (`Summary`): which icon, which entry the menu offers. Choosing the icon variant by the
//! taskbar's theme is the only thing it adds.

use std::sync::Mutex;

use crisol_client::{Overall, Summary, Toggle};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Wry};

use crate::i18n::Dictionary;

/// The ids of the menu entries.
pub mod id {
    pub const TOGGLE: &str = "toggle";
    pub const OPEN: &str = "open";
    pub const QUIT: &str = "quit";
    pub const STOP_QUIT: &str = "stop_quit";
}

/// The PNG of the icon for a state on a dark or a light taskbar (`icons/tray/`, made from
/// `assets/brand/tray/` by `src/scripts/gui-icons.sh`).
fn icon_png(overall: Overall, light_taskbar: bool) -> &'static [u8] {
    macro_rules! icons {
        ($($state:ident => $name:literal),* $(,)?) => {
            match (overall, light_taskbar) {
                $(
                    (Overall::$state, false) => include_bytes!(concat!("../icons/tray/", $name, "-dark.png")),
                    (Overall::$state, true) => include_bytes!(concat!("../icons/tray/", $name, "-light.png")),
                )*
            }
        };
    }
    icons! {
        Running => "running",
        Starting => "starting",
        Stopped => "stopped",
        Failed => "failed",
        NoDaemon => "no-daemon",
    }
}

/// What the tray shows now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    overall: Overall,
    toggle: Toggle,
    light_taskbar: bool,
}

/// The tray icon and the entries that change.
pub struct Tray {
    icon: TrayIcon<Wry>,
    toggle: MenuItem<Wry>,
    words: Dictionary,
    shown: Mutex<Shown>,
}

impl Tray {
    /// Builds the icon and its menu, in the state of a daemon that is not there yet.
    pub fn build(
        app: &AppHandle,
        words: &Dictionary,
        light_taskbar: bool,
        on_left_click: impl Fn(&AppHandle) + Send + Sync + 'static,
        on_entry: impl Fn(&AppHandle, &str) + Send + Sync + 'static,
    ) -> tauri::Result<Self> {
        let toggle = MenuItem::with_id(
            app,
            id::TOGGLE,
            words.get("tray.start_all"),
            true,
            None::<&str>,
        )?;
        let open = MenuItem::with_id(app, id::OPEN, words.get("tray.open"), true, None::<&str>)?;
        let quit = MenuItem::with_id(app, id::QUIT, words.get("tray.quit"), true, None::<&str>)?;
        let stop_quit = MenuItem::with_id(
            app,
            id::STOP_QUIT,
            words.get("tray.stop_quit"),
            true,
            None::<&str>,
        )?;
        let menu = Menu::with_items(
            app,
            &[
                &toggle,
                &open,
                &PredefinedMenuItem::separator(app)?,
                &quit,
                &stop_quit,
            ],
        )?;
        let shown = Shown {
            overall: Overall::NoDaemon,
            toggle: Toggle::StartAll,
            light_taskbar,
        };
        let icon = TrayIconBuilder::with_id("crisol")
            .icon(Image::from_bytes(icon_png(shown.overall, light_taskbar))?)
            .tooltip(tooltip(words, shown.overall))
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(move |app, event| on_entry(app, event.id.as_ref()))
            .on_tray_icon_event(move |tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    on_left_click(tray.app_handle());
                }
            })
            .build(app)?;
        Ok(Self {
            icon,
            toggle,
            words: words.clone(),
            shown: Mutex::new(shown),
        })
    }

    /// Shows the state the daemon is in.
    pub fn show(&self, summary: &Summary) {
        self.change(|shown| {
            shown.overall = summary.overall;
            shown.toggle = summary.toggle;
        });
    }

    /// The taskbar changed theme: the icon is chosen again.
    pub fn set_light_taskbar(&self, light_taskbar: bool) {
        self.change(|shown| shown.light_taskbar = light_taskbar);
    }

    /// Applies `edit` and redraws only what is different from what was drawn.
    fn change(&self, edit: impl FnOnce(&mut Shown)) {
        let Ok(mut shown) = self.shown.lock() else {
            return;
        };
        let before = *shown;
        edit(&mut shown);
        let now = *shown;
        if now == before {
            return;
        }
        if (now.overall, now.light_taskbar) != (before.overall, before.light_taskbar) {
            if let Ok(image) = Image::from_bytes(icon_png(now.overall, now.light_taskbar)) {
                let _ = self.icon.set_icon(Some(image));
            }
            let _ = self
                .icon
                .set_tooltip(Some(tooltip(&self.words, now.overall)));
        }
        if now.toggle != before.toggle {
            let _ = self.toggle.set_text(self.words.get(match now.toggle {
                Toggle::StartAll => "tray.start_all",
                Toggle::StopAll => "tray.stop_all",
            }));
        }
    }
}

fn tooltip(words: &Dictionary, overall: Overall) -> String {
    let state = words.get(&format!(
        "overall.{}",
        overall.icon_name().replace('-', "_")
    ));
    words.with("tray.tooltip", &[("state", &state)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_a_32_pixel_png_in_both_themes() {
        for overall in [
            Overall::Running,
            Overall::Starting,
            Overall::Stopped,
            Overall::Failed,
            Overall::NoDaemon,
        ] {
            for light in [false, true] {
                let png = icon_png(overall, light);
                assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{overall:?} {light}");
                // The IHDR chunk holds the width and the height.
                assert_eq!(
                    &png[16..24],
                    &[0, 0, 0, 32, 0, 0, 0, 32],
                    "{overall:?} {light}"
                );
            }
        }
    }

    #[test]
    fn the_two_themes_are_different_pictures() {
        assert_ne!(
            icon_png(Overall::Running, false),
            icon_png(Overall::Running, true)
        );
    }

    #[test]
    fn the_tooltip_names_the_state_in_the_language_of_the_user() {
        let es = Dictionary::of(crisol_client::Language::Spanish);
        assert_eq!(tooltip(&es, Overall::NoDaemon), "Crisol: Sin daemon");
        assert_eq!(tooltip(&es, Overall::Failed), "Crisol: Algo falló");
        let en = Dictionary::of(crisol_client::Language::English);
        assert_eq!(tooltip(&en, Overall::Running), "Crisol: Running");
    }
}
