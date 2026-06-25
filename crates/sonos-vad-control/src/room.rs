//! Stable room identity and Windows device-name derivation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Friendly-name prefix applied to every generated Windows output device.
pub const DEVICE_NAME_PREFIX: &str = "Sonos";

/// Stable identity of a discovered Sonos room.
///
/// `uuid` is the durable key — it survives IP-address changes and room renames —
/// while `name` is the human-facing label used to build the Windows device name.
/// Two rooms can transiently share a `name` (for example two zones both called
/// `Office`), so device names are disambiguated by [`device_name_for`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoomKey {
    /// Stable Sonos room/zone UUID (for example `RINCON_...`).
    pub uuid: String,
    /// Human-facing room/zone name (for example `Living Room`).
    pub name: String,
}

impl RoomKey {
    /// Builds a [`RoomKey`] from a stable `uuid` and a human-facing `name`.
    #[must_use]
    pub fn new(uuid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            name: name.into(),
        }
    }
}

/// Derives a sanitised, unique Windows friendly name for `room`.
///
/// The base name is `"<DEVICE_NAME_PREFIX> <room name>"` with runs of whitespace
/// collapsed to single spaces; if `room.name` is blank the UUID is used in its
/// place. When the base name already appears in `taken` (the names already
/// assigned to other endpoints) a numeric suffix — `" (2)"`, `" (3)"`, ... — is
/// appended until the result is unique.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeSet;
/// use sonos_vad_control::{device_name_for, RoomKey};
///
/// let room = RoomKey::new("RINCON_1", "Living Room");
/// assert_eq!(device_name_for(&room, &BTreeSet::new()), "Sonos Living Room");
///
/// let taken = BTreeSet::from(["Sonos Office".to_owned()]);
/// let office = RoomKey::new("RINCON_2", "Office");
/// assert_eq!(device_name_for(&office, &taken), "Sonos Office (2)");
/// ```
#[must_use]
pub fn device_name_for(room: &RoomKey, taken: &BTreeSet<String>) -> String {
    let label = collapse_whitespace(&room.name);
    let base = if label.is_empty() {
        format!("{DEVICE_NAME_PREFIX} {}", collapse_whitespace(&room.uuid))
    } else {
        format!("{DEVICE_NAME_PREFIX} {label}")
    };
    if !taken.contains(&base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base} ({n})");
        if !taken.contains(&candidate) {
            break candidate;
        }
        n += 1;
    }
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_collapses_whitespace() {
        let room = RoomKey::new("u1", "  Living   Room ");
        assert_eq!(
            device_name_for(&room, &BTreeSet::new()),
            "Sonos Living Room"
        );
    }

    #[test]
    fn falls_back_to_uuid_when_name_is_blank() {
        let room = RoomKey::new("RINCON_9", "   ");
        assert_eq!(device_name_for(&room, &BTreeSet::new()), "Sonos RINCON_9");
    }

    #[test]
    fn appends_numeric_suffix_until_unique() {
        let mut taken = BTreeSet::from(["Sonos Office".to_owned()]);
        let room = RoomKey::new("u2", "Office");
        let first = device_name_for(&room, &taken);
        assert_eq!(first, "Sonos Office (2)");
        taken.insert(first);
        assert_eq!(device_name_for(&room, &taken), "Sonos Office (3)");
    }
}
