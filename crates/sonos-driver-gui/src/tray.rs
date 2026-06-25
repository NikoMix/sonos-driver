//! The Windows system-tray icon and its context menu.
//!
//! `tray-icon` and `muda` deliver menu and tray interactions through global
//! channels, which the UI drains each frame via [`Tray::collect_actions`] and
//! maps to [`Action`]s. The tray and menu must be created on the thread that
//! runs the `winit` event loop (the main thread), which is where
//! [`eframe`](eframe) constructs the [`crate::app::GuiApp`].

use sonos_driver_gui::Action;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Owns the live tray icon and the menu items whose ids drive the UI.
///
/// The [`TrayIcon`] is retained because dropping it removes the icon from the
/// notification area.
pub(crate) struct Tray {
    _icon: TrayIcon,
    show: MenuItem,
    hide: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    quit: MenuItem,
}

impl Tray {
    /// Build the tray icon and context menu.
    ///
    /// Returns `None` if the platform refuses to create the tray (for example
    /// in a headless session), so the app can run windowed-only.
    pub(crate) fn new(icon: Option<Icon>) -> Option<Self> {
        let menu = Menu::new();
        let show = MenuItem::new("Show window", true, None);
        let hide = MenuItem::new("Hide to tray", true, None);
        let start = MenuItem::new("Start streaming", true, None);
        let stop = MenuItem::new("Stop streaming", false, None);
        let quit = MenuItem::new("Quit", true, None);

        menu.append(&show).ok()?;
        menu.append(&hide).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&start).ok()?;
        menu.append(&stop).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&quit).ok()?;

        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Sonos Driver");
        if let Some(icon) = icon {
            builder = builder.with_icon(icon);
        }

        match builder.build() {
            Ok(tray) => Some(Self {
                _icon: tray,
                show,
                hide,
                start,
                stop,
                quit,
            }),
            Err(err) => {
                tracing::warn!(%err, "failed to create the system tray");
                None
            }
        }
    }

    /// Enable or disable the Start/Stop items to match the bridge state.
    pub(crate) fn set_active(&self, active: bool) {
        self.start.set_enabled(!active);
        self.stop.set_enabled(active);
    }

    /// Drain the global tray and menu channels, appending the resulting actions.
    pub(crate) fn collect_actions(&self, out: &mut Vec<Action>) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(action) = self.action_for(&event) {
                out.push(action);
            }
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            // A double-click on the tray icon reveals the window.
            if let TrayIconEvent::DoubleClick { .. } = event {
                out.push(Action::ShowWindow);
            }
        }
    }

    /// Map a menu event to the action its item represents, if any.
    fn action_for(&self, event: &MenuEvent) -> Option<Action> {
        let id = &event.id;
        if id == self.show.id() {
            Some(Action::ShowWindow)
        } else if id == self.hide.id() {
            Some(Action::HideWindow)
        } else if id == self.start.id() {
            Some(Action::StartBridge)
        } else if id == self.stop.id() {
            Some(Action::StopBridge)
        } else if id == self.quit.id() {
            Some(Action::Quit)
        } else {
            None
        }
    }
}
