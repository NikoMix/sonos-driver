//! Pure, platform-independent naming and slot-assignment logic.
//!
//! Nothing here touches Windows, so the whole binding algorithm is unit-tested
//! on every platform.

use std::collections::BTreeSet;

use sonos_vad_control::{device_name_for, RoomKey};

use crate::VadEndpoint;

/// `PKEY_DeviceInterface_FriendlyName` of the sonosvad adapter, as declared by
/// `DeviceDesc` in `driver/sonosvad/Source/Main/sonosvad.inx`.
pub const DEFAULT_ADAPTER_NAME: &str = "Sonos Virtual Audio Device";

/// Prefix of the factory endpoint names declared in the driver INF.
pub const FACTORY_NAME_PREFIX: &str = "Sonos Output";

/// The factory name of slot `slot` (1-based), matching the driver INF.
#[must_use]
pub fn factory_name_for_slot(slot: u32) -> String {
    format!("{FACTORY_NAME_PREFIX} {slot}")
}

/// Recovers the 1-based slot number from an endpoint's factory description.
///
/// Returns `None` when `desc` is not one of the driver's factory names, which
/// is how endpoints belonging to some other device are ignored.
///
/// # Examples
///
/// ```
/// use sonos_vad_endpoint::slot_from_factory_name;
///
/// assert_eq!(slot_from_factory_name("Sonos Output 3"), Some(3));
/// assert_eq!(slot_from_factory_name("Speakers"), None);
/// ```
#[must_use]
pub fn slot_from_factory_name(desc: &str) -> Option<u32> {
    let rest = desc.trim().strip_prefix(FACTORY_NAME_PREFIX)?;
    rest.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

/// One endpoint bound to one Sonos room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Stable WASAPI endpoint id to loopback-capture.
    pub device_id: String,
    /// The room this endpoint streams to.
    pub room: RoomKey,
    /// The sound-picker name the endpoint should carry.
    pub name: String,
    /// Whether the endpoint's current name differs from [`Self::name`].
    ///
    /// Bindings are reported even when nothing needs renaming, so the caller
    /// always learns which endpoint belongs to which room; this flag is what
    /// keeps a steady-state reconcile tick from issuing pointless writes.
    pub rename_needed: bool,
}

/// One endpoint returned to its factory name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Stable WASAPI endpoint id.
    pub device_id: String,
    /// The factory name to restore.
    pub name: String,
}

/// The renames needed to make the endpoint pool match the discovered rooms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingPlan {
    /// Endpoints to return to their factory name, applied first so their names
    /// are free for reuse in the same pass.
    pub releases: Vec<Release>,
    /// Endpoints to point at a room.
    pub binds: Vec<Binding>,
    /// Rooms that could not be placed because every slot was taken.
    pub unplaced: usize,
}

/// Plans the renames that converge `current` onto `rooms`.
///
/// Slot assignment is **stable**: a room already bound to a slot keeps it, so a
/// discovery tick that merely re-reports the same rooms produces an empty plan
/// and Windows never sees a device churn. Remaining rooms fill the lowest free
/// slots in discovery order, which keeps the picker order predictable.
///
/// Endpoints whose factory description is not a sonosvad factory name are
/// ignored entirely.
#[must_use]
pub fn plan_bindings(current: &[VadEndpoint], rooms: &[RoomKey]) -> BindingPlan {
    let mut slots: Vec<&VadEndpoint> = current
        .iter()
        .filter(|e| slot_from_factory_name(&e.device_desc).is_some())
        .collect();
    slots.sort_by_key(|e| slot_from_factory_name(&e.device_desc).unwrap_or(u32::MAX));

    // Desired name per room, de-duplicated the same way the rest of the project
    // derives device names.
    let mut taken = BTreeSet::new();
    let desired: Vec<(RoomKey, String)> = rooms
        .iter()
        .map(|room| {
            let name = device_name_for(room, &taken);
            taken.insert(name.clone());
            (room.clone(), name)
        })
        .collect();

    // Keep a room on the slot that already carries its name.
    let mut assignment: Vec<Option<usize>> = vec![None; slots.len()];
    let mut placed = vec![false; desired.len()];

    for (room_index, (_, name)) in desired.iter().enumerate() {
        if let Some(slot_index) = slots
            .iter()
            .enumerate()
            .position(|(i, e)| assignment[i].is_none() && e.friendly_name == *name)
        {
            assignment[slot_index] = Some(room_index);
            placed[room_index] = true;
        }
    }

    // Fill the remaining rooms into the lowest free slots.
    for (room_index, _) in desired.iter().enumerate() {
        if placed[room_index] {
            continue;
        }
        if let Some(slot_index) = assignment.iter().position(Option::is_none) {
            assignment[slot_index] = Some(room_index);
            placed[room_index] = true;
        }
    }

    let mut plan = BindingPlan {
        unplaced: placed.iter().filter(|p| !**p).count(),
        ..BindingPlan::default()
    };

    for (slot_index, endpoint) in slots.iter().enumerate() {
        if let Some(room_index) = assignment[slot_index] {
            let (room, name) = &desired[room_index];
            // Reported even when the name already matches, so the caller always
            // learns which endpoint to loopback-capture for this room;
            // `apply_plan` short-circuits identical renames at the OS layer.
            plan.binds.push(Binding {
                device_id: endpoint.device_id.clone(),
                room: room.clone(),
                name: name.clone(),
                rename_needed: endpoint.friendly_name != *name,
            });
        } else if endpoint.friendly_name != endpoint.device_desc {
            plan.releases.push(Release {
                device_id: endpoint.device_id.clone(),
                name: endpoint.device_desc.clone(),
            });
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(slot: u32, friendly: &str) -> VadEndpoint {
        VadEndpoint {
            device_id: format!("id-{slot}"),
            friendly_name: friendly.to_owned(),
            device_desc: factory_name_for_slot(slot),
            adapter: DEFAULT_ADAPTER_NAME.to_owned(),
            slot: Some(slot),
            active: true,
        }
    }

    #[test]
    fn parses_and_rejects_factory_names() {
        assert_eq!(slot_from_factory_name("Sonos Output 1"), Some(1));
        assert_eq!(slot_from_factory_name("  Sonos Output 12  "), Some(12));
        assert_eq!(slot_from_factory_name("Sonos Output 0"), None);
        assert_eq!(slot_from_factory_name("Sonos Output"), None);
        assert_eq!(slot_from_factory_name("Realtek Speakers"), None);
    }

    #[test]
    fn ignores_endpoints_from_other_devices() {
        let mut foreign = endpoint(1, "Speakers");
        foreign.device_desc = "Speakers".to_owned();
        let plan = plan_bindings(&[foreign], &[RoomKey::new("u1", "Kitchen")]);
        assert!(plan.binds.is_empty());
        assert_eq!(plan.unplaced, 1);
    }

    #[test]
    fn keeps_a_room_on_the_slot_it_already_occupies() {
        let current = vec![
            endpoint(1, "Sonos Output 1"),
            endpoint(2, "Sonos Kitchen"),
            endpoint(3, "Sonos Output 3"),
        ];
        let rooms = vec![
            RoomKey::new("u-kitchen", "Kitchen"),
            RoomKey::new("u-office", "Office"),
        ];

        let plan = plan_bindings(&current, &rooms);

        // Kitchen stays on slot 2; only Office is actually renamed.
        let renames: Vec<_> = plan
            .binds
            .iter()
            .filter(|b| b.device_id != "id-2")
            .map(|b| (b.device_id.as_str(), b.name.as_str()))
            .collect();
        assert_eq!(renames, vec![("id-1", "Sonos Office")]);
        assert!(plan.releases.is_empty());
        assert_eq!(plan.unplaced, 0);
    }

    #[test]
    fn releases_slots_whose_room_is_gone() {
        let current = vec![endpoint(1, "Sonos Kitchen"), endpoint(2, "Sonos Office")];
        let plan = plan_bindings(&current, &[RoomKey::new("u-kitchen", "Kitchen")]);

        assert_eq!(
            plan.releases,
            vec![Release {
                device_id: "id-2".to_owned(),
                name: "Sonos Output 2".to_owned(),
            }]
        );
    }

    #[test]
    fn de_duplicates_rooms_that_share_a_name() {
        let current = vec![endpoint(1, "Sonos Output 1"), endpoint(2, "Sonos Output 2")];
        let rooms = vec![RoomKey::new("u1", "Office"), RoomKey::new("u2", "Office")];

        let plan = plan_bindings(&current, &rooms);

        let names: Vec<_> = plan.binds.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["Sonos Office", "Sonos Office (2)"]);
    }

    #[test]
    fn counts_rooms_that_do_not_fit() {
        let current = vec![endpoint(1, "Sonos Output 1")];
        let rooms = vec![RoomKey::new("u1", "A"), RoomKey::new("u2", "B")];
        assert_eq!(plan_bindings(&current, &rooms).unplaced, 1);
    }

    #[test]
    fn orders_slots_numerically_not_lexicographically() {
        let current = vec![
            endpoint(10, "Sonos Output 10"),
            endpoint(2, "Sonos Output 2"),
            endpoint(1, "Sonos Output 1"),
        ];
        let plan = plan_bindings(&current, &[RoomKey::new("u1", "Kitchen")]);
        assert_eq!(plan.binds[0].device_id, "id-1");
    }
}
