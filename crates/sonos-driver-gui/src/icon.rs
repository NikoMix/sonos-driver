//! The embedded application icon, decoded for the window and the system tray.

/// The embedded RGBA icon source, in PNG form.
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// A decoded RGBA image: a flat `width * height * 4` byte buffer.
pub(crate) struct DecodedIcon {
    /// Tightly packed `R, G, B, A` bytes, row-major.
    pub(crate) rgba: Vec<u8>,
    /// Image width in pixels.
    pub(crate) width: u32,
    /// Image height in pixels.
    pub(crate) height: u32,
}

/// Decode the embedded PNG into RGBA pixels.
fn decode_rgba() -> Result<DecodedIcon, image::ImageError> {
    let image = image::load_from_memory(ICON_PNG)?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(DecodedIcon {
        rgba: image.into_raw(),
        width,
        height,
    })
}

/// Decode the embedded icon for the application window, logging and returning
/// `None` if it cannot be decoded.
pub(crate) fn decode() -> Option<DecodedIcon> {
    match decode_rgba() {
        Ok(icon) => Some(icon),
        Err(err) => {
            tracing::warn!(%err, "failed to decode the embedded window icon");
            None
        }
    }
}

/// Decode the embedded icon as a system-tray [`tray_icon::Icon`].
pub(crate) fn tray_icon() -> Option<tray_icon::Icon> {
    let icon = decode()?;
    match tray_icon::Icon::from_rgba(icon.rgba, icon.width, icon.height) {
        Ok(icon) => Some(icon),
        Err(err) => {
            tracing::warn!(%err, "failed to build the tray icon");
            None
        }
    }
}
