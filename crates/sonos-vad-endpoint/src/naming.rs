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

/// Sound-picker name of the aggregate output that plays to the whole
/// household at once.
pub const AGGREGATE_NAME: &str = "All Sonos";

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

/// The endpoint bound to the aggregate "All Sonos" output.
///
/// Kept separate from [`Binding`] rather than generalising `Binding.room` into
/// a one-or-many enum: every existing consumer of `binds` means "this endpoint
/// is exactly this room", and blurring that would push a match over an enum
/// into code that only ever cares about the single-room case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateBinding {
    /// Stable WASAPI endpoint id to loopback-capture.
    pub device_id: String,
    /// The sound-picker name the endpoint should carry ([`AGGREGATE_NAME`]).
    pub name: String,
    /// Every room this output plays to, in binding order. The first is the one
    /// that will coordinate the Sonos group.
    pub rooms: Vec<RoomKey>,
    /// Whether the endpoint's current name differs from [`Self::name`].
    pub rename_needed: bool,
}

/// What a plan should produce beyond the plain room-to-endpoint mapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanOptions {
    /// Disable the endpoints no room was bound to, so the Windows sound picker
    /// shows exactly the household's rooms.
    pub hide_spares: bool,
    /// Reserve an endpoint for the aggregate "All Sonos" output.
    ///
    /// Ignored when fewer than two rooms exist, where an "everything" output
    /// would just duplicate the only room.
    pub aggregate: bool,
}

impl PlanOptions {
    /// What the service and the CLI use: rooms, the aggregate output, and no
    /// leftover placeholders.
    #[must_use]
    pub fn product() -> Self {
        Self {
            hide_spares: true,
            aggregate: true,
        }
    }
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
    /// Endpoint ids to make visible in the Windows sound UI, because a room is
    /// bound to them but Windows currently has them disabled.
    pub show: Vec<String>,
    /// Endpoint ids to hide from the Windows sound UI, because no room is bound
    /// to them.
    ///
    /// Empty unless [`plan_bindings_for`] was asked to hide spares: the driver
    /// ships a fixed pool of slots, so without this a household with four rooms
    /// leaves "Sonos Output 5" through "8" cluttering the sound picker forever.
    pub hide: Vec<String>,
    /// The endpoint reserved for the aggregate "All Sonos" output, if any.
    pub aggregate: Option<AggregateBinding>,
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
    plan_bindings_for(current, rooms, PlanOptions::default())
}

/// [`plan_bindings`], with the aggregate output and spare-hiding behaviour
/// selected by `options`.
///
/// The aggregate takes the **highest** slot rather than the first, so adding
/// it does not shift every room's position in the sound picker, and so a
/// household that outgrows the driver loses a placeholder rather than
/// "All Sonos".
#[must_use]
pub fn plan_bindings_for(
    current: &[VadEndpoint],
    rooms: &[RoomKey],
    options: PlanOptions,
) -> BindingPlan {
    let mut slots: Vec<&VadEndpoint> = current
        .iter()
        .filter(|e| slot_from_factory_name(&e.device_desc).is_some())
        .collect();
    slots.sort_by_key(|e| slot_from_factory_name(&e.device_desc).unwrap_or(u32::MAX));

    // Reserve the aggregate's slot before rooms are placed, so the two never
    // compete for it.
    let aggregate_slot = if options.aggregate && rooms.len() >= 2 {
        slots.pop()
    } else {
        None
    };

    // Desired name per room, de-duplicated the same way the rest of the project
    // derives device names. "All Sonos" is pre-claimed so a room actually
    // called that gets suffixed instead of colliding with the aggregate.
    let mut taken = BTreeSet::new();
    if aggregate_slot.is_some() {
        taken.insert(AGGREGATE_NAME.to_owned());
    }
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
            // A room cannot be selected in Windows while its endpoint is
            // disabled, so an endpoint we hid on an earlier tick has to come
            // back the moment a room needs it. Only a *disabled* endpoint is
            // revived: an unplugged or not-present one is Windows' business.
            if endpoint.disabled {
                plan.show.push(endpoint.device_id.clone());
            }
        } else {
            if endpoint.friendly_name != endpoint.device_desc {
                plan.releases.push(Release {
                    device_id: endpoint.device_id.clone(),
                    name: endpoint.device_desc.clone(),
                });
            }
            if options.hide_spares && endpoint.active {
                plan.hide.push(endpoint.device_id.clone());
            }
        }
    }

    if let Some(endpoint) = aggregate_slot {
        plan.aggregate = Some(AggregateBinding {
            device_id: endpoint.device_id.clone(),
            name: AGGREGATE_NAME.to_owned(),
            rooms: desired.iter().map(|(room, _)| room.clone()).collect(),
            rename_needed: endpoint.friendly_name != AGGREGATE_NAME,
        });
        if endpoint.disabled {
            plan.show.push(endpoint.device_id.clone());
        }
    }

    plan
}

/// The plan that undoes everything this crate ever did: factory names back on
/// every renamed slot, and every slot we disabled made visible again.
///
/// [`plan_bindings_with`] with an empty room set restores names but *not*
/// visibility, because from its point of view a nameless spare is exactly what
/// should stay hidden. Uninstall needs the opposite: leaving `DeviceState=2`
/// behind would strand disabled ghost endpoints in the registry, which outlive
/// the driver and cannot be re-enabled through any UI once its INF is gone.
#[must_use]
pub fn plan_release_all(current: &[VadEndpoint]) -> BindingPlan {
    let mut plan = plan_bindings_for(current, &[], PlanOptions::default());
    plan.show = current
        .iter()
        .filter(|e| slot_from_factory_name(&e.device_desc).is_some() && e.disabled)
        .map(|e| e.device_id.clone())
        .collect();
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
            disabled: false,
        }
    }

    /// An endpoint we previously hid, as `list()` would report it back.
    fn hidden_endpoint(slot: u32, friendly: &str) -> VadEndpoint {
        VadEndpoint {
            active: false,
            disabled: true,
            ..endpoint(slot, friendly)
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

    #[test]
    fn release_all_restores_names_and_reveals_hidden_slots() {
        let current = vec![
            endpoint(1, "Kitchen"),
            hidden_endpoint(2, "Sonos Output 2"),
            hidden_endpoint(3, "Sonos Output 3"),
        ];

        let plan = plan_release_all(&current);

        // Only slot 1 carries a non-factory name, so only it needs renaming...
        assert_eq!(
            plan.releases
                .iter()
                .map(|r| &r.device_id)
                .collect::<Vec<_>>(),
            vec!["id-1"]
        );
        // ...but every slot we disabled has to come back, or uninstalling the
        // driver would strand them disabled forever.
        assert_eq!(plan.show, vec!["id-2", "id-3"]);
        assert!(plan.hide.is_empty());
        assert!(plan.binds.is_empty());
    }

    #[test]
    fn release_all_leaves_endpoints_windows_disabled_for_other_reasons_alone() {
        // Not disabled, just absent: forcing DeviceState=ACTIVE here would put a
        // phantom output in the picker for hardware that is not present.
        let unplugged = VadEndpoint {
            active: false,
            disabled: false,
            ..endpoint(2, "Sonos Output 2")
        };

        let plan = plan_release_all(&[endpoint(1, "Sonos Output 1"), unplugged]);

        assert!(plan.show.is_empty());
    }
}
