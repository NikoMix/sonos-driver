//! Binary entry point for the Sonos Router desktop app.
//!
//! Wires the headless core (`sonos_driver_gui`) to the `eframe`/`egui` shell,
//! the system tray, and the embedded icon. The `windows` subsystem attribute
//! keeps a console window from appearing in release builds.

#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icon;
mod tray;

use std::sync::Arc;

use eframe::egui;
use eframe::NativeOptions;
use tracing_subscriber::EnvFilter;

use crate::app::GuiApp;

fn main() -> eframe::Result<()> {
    init_tracing();

    let options = NativeOptions {
        viewport: build_viewport(),
        ..NativeOptions::default()
    };

    eframe::run_native(
        "Sonos Router",
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}

/// Initialise tracing to stderr, honouring `RUST_LOG` and defaulting to `info`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Build the main window's viewport, including the embedded icon when available.
fn build_viewport() -> egui::ViewportBuilder {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Sonos Router")
        .with_inner_size([620.0, 700.0])
        .with_min_inner_size([520.0, 480.0]);

    if let Some(decoded) = icon::decode() {
        viewport = viewport.with_icon(Arc::new(egui::IconData {
            rgba: decoded.rgba,
            width: decoded.width,
            height: decoded.height,
        }));
    }

    viewport
}
