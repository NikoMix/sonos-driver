//! The reconcile planner that converges live endpoints to the discovered rooms.

use std::collections::{BTreeMap, BTreeSet};

use crate::protocol::{ControlCommand, DriverEvent, EndpointId};
use crate::room::{device_name_for, RoomKey};

/// Lifecycle phase of a single room-to-endpoint mapping (read model for UIs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointPhase {
    /// A create command was issued; awaiting the driver's confirmation.
    PendingCreate,
    /// The endpoint is live.
    Active,
    /// A destroy command was issued; awaiting the driver's confirmation.
    PendingDestroy,
}

/// Read-only snapshot of one room-to-endpoint mapping, for UIs and telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointStatus {
    /// The room this mapping streams to.
    pub room: RoomKey,
    /// The unique Windows friendly name assigned to the device.
    pub device_name: String,
    /// The current lifecycle phase.
    pub phase: EndpointPhase,
    /// The driver-assigned id, once the endpoint is (or was) live.
    pub id: Option<EndpointId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    PendingCreate,
    Active(EndpointId),
    PendingDestroy(EndpointId),
}

impl State {
    fn id(&self) -> Option<EndpointId> {
        match self {
            State::PendingCreate => None,
            State::Active(id) | State::PendingDestroy(id) => Some(*id),
        }
    }

    fn phase(&self) -> EndpointPhase {
        match self {
            State::PendingCreate => EndpointPhase::PendingCreate,
            State::Active(_) => EndpointPhase::Active,
            State::PendingDestroy(_) => EndpointPhase::PendingDestroy,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    room: RoomKey,
    device_name: String,
    state: State,
}

/// Tracks the desired-versus-actual mapping of Sonos rooms to virtual render
/// endpoints and plans the [`ControlCommand`]s that converge them.
///
/// The registry is the user-mode service's authoritative view. Each discovery
/// tick the service calls [`reconcile`](Self::reconcile) with the currently
/// discovered rooms to obtain the create/destroy commands to send the driver,
/// and feeds every [`DriverEvent`] reply back through [`apply`](Self::apply).
/// Both operations are deterministic and side-effect-free beyond the registry
/// itself, so the entire control loop is unit-testable without a driver.
#[derive(Debug, Clone, Default)]
pub struct EndpointRegistry {
    /// Entries keyed by [`RoomKey::uuid`] (the stable room identity).
    entries: BTreeMap<String, Entry>,
}

impl EndpointRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of tracked room-to-endpoint mappings, in any phase.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no mappings are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the live endpoint id for the room with `uuid`, if it is active.
    #[must_use]
    pub fn active_endpoint(&self, uuid: &str) -> Option<EndpointId> {
        match self.entries.get(uuid).map(|entry| &entry.state) {
            Some(State::Active(id)) => Some(*id),
            _ => None,
        }
    }

    /// Returns a stable, name-sorted snapshot of every mapping, for UIs and
    /// telemetry.
    #[must_use]
    pub fn statuses(&self) -> Vec<EndpointStatus> {
        let mut out: Vec<EndpointStatus> = self
            .entries
            .values()
            .map(|entry| EndpointStatus {
                room: entry.room.clone(),
                device_name: entry.device_name.clone(),
                phase: entry.state.phase(),
                id: entry.state.id(),
            })
            .collect();
        out.sort_by(|a, b| a.device_name.cmp(&b.device_name));
        out
    }

    /// Plans the commands that converge the live endpoints to `desired`.
    ///
    /// For every desired room with no tracked entry a
    /// [`ControlCommand::CreateEndpoint`] is emitted (and the entry recorded as
    /// pending creation); for every active entry whose room is absent from
    /// `desired` a [`ControlCommand::DestroyEndpoint`] is emitted (and the entry
    /// moved to pending destruction). Entries already in a pending phase are
    /// left untouched, so calling `reconcile` repeatedly with an unchanged
    /// `desired` set is idempotent — it returns no commands until a
    /// [`DriverEvent`] is applied. The returned commands must be delivered to
    /// the driver.
    #[must_use = "the planned commands must be sent to the driver"]
    pub fn reconcile(&mut self, desired: &[RoomKey]) -> Vec<ControlCommand> {
        let desired_uuids: BTreeSet<&str> = desired.iter().map(|room| room.uuid.as_str()).collect();
        let mut commands = Vec::new();

        // Destroy: active endpoints whose room is no longer discovered.
        for entry in self.entries.values_mut() {
            if let State::Active(id) = entry.state {
                if !desired_uuids.contains(entry.room.uuid.as_str()) {
                    commands.push(ControlCommand::DestroyEndpoint { id });
                    entry.state = State::PendingDestroy(id);
                }
            }
        }

        // Create: discovered rooms with no entry at all. Names are de-duplicated
        // against the names already taken by other entries and within this batch.
        let mut taken = self.taken_names();
        for room in desired {
            if self.entries.contains_key(&room.uuid) {
                continue;
            }
            let device_name = device_name_for(room, &taken);
            taken.insert(device_name.clone());
            commands.push(ControlCommand::CreateEndpoint {
                room: room.clone(),
                device_name: device_name.clone(),
            });
            self.entries.insert(
                room.uuid.clone(),
                Entry {
                    room: room.clone(),
                    device_name,
                    state: State::PendingCreate,
                },
            );
        }

        commands
    }

    /// Applies a [`DriverEvent`] reply, advancing the affected entry's state.
    ///
    /// Confirmed creations become active; confirmed destructions drop the entry;
    /// a `CreateFailed` drops the pending-create entry so the next `reconcile`
    /// retries it; a `DestroyFailed` reverts the entry to active so the next
    /// `reconcile` retries the destroy. Events for rooms the registry is not
    /// tracking are ignored.
    pub fn apply(&mut self, event: &DriverEvent) {
        match event {
            DriverEvent::EndpointCreated { room_uuid, id } => {
                if let Some(entry) = self.entries.get_mut(room_uuid) {
                    if matches!(entry.state, State::PendingCreate) {
                        entry.state = State::Active(*id);
                    }
                }
            }
            DriverEvent::EndpointDestroyed { id } => {
                self.entries
                    .retain(|_, entry| entry.state.id() != Some(*id));
            }
            DriverEvent::CreateFailed { room_uuid, .. } => {
                let pending = matches!(
                    self.entries.get(room_uuid).map(|entry| &entry.state),
                    Some(State::PendingCreate)
                );
                if pending {
                    self.entries.remove(room_uuid);
                }
            }
            DriverEvent::DestroyFailed { id, .. } => {
                for entry in self.entries.values_mut() {
                    if entry.state == State::PendingDestroy(*id) {
                        entry.state = State::Active(*id);
                    }
                }
            }
        }
    }

    fn taken_names(&self) -> BTreeSet<String> {
        self.entries
            .values()
            .map(|entry| entry.device_name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(uuid: &str, name: &str) -> RoomKey {
        RoomKey::new(uuid, name)
    }

    #[test]
    fn empty_reconcile_emits_nothing() {
        let mut registry = EndpointRegistry::new();
        assert!(registry.reconcile(&[]).is_empty());
        assert!(registry.is_empty());
    }

    #[test]
    fn new_rooms_emit_create_commands_and_are_pending() {
        let mut registry = EndpointRegistry::new();
        let desired = vec![room("u1", "Living Room"), room("u2", "Kitchen")];
        let commands = registry.reconcile(&desired);

        assert_eq!(commands.len(), 2);
        assert_eq!(registry.len(), 2);
        assert!(registry.active_endpoint("u1").is_none());

        let names: Vec<String> = registry
            .statuses()
            .into_iter()
            .map(|status| status.device_name)
            .collect();
        assert_eq!(
            names,
            vec!["Sonos Kitchen".to_owned(), "Sonos Living Room".to_owned()],
            "statuses are sorted by device name"
        );
    }

    #[test]
    fn reconcile_is_idempotent_until_events_are_applied() {
        let mut registry = EndpointRegistry::new();
        let desired = vec![room("u1", "Living Room")];
        assert_eq!(registry.reconcile(&desired).len(), 1);
        assert!(
            registry.reconcile(&desired).is_empty(),
            "a pending create must not be re-issued"
        );
    }

    #[test]
    fn created_event_activates_the_endpoint() {
        let mut registry = EndpointRegistry::new();
        let _ = registry.reconcile(&[room("u1", "Living Room")]);
        registry.apply(&DriverEvent::EndpointCreated {
            room_uuid: "u1".to_owned(),
            id: EndpointId(7),
        });
        assert_eq!(registry.active_endpoint("u1"), Some(EndpointId(7)));
        assert_eq!(registry.statuses()[0].phase, EndpointPhase::Active);
    }

    #[test]
    fn removed_room_emits_destroy_then_is_idempotent() {
        let mut registry = EndpointRegistry::new();
        let _ = registry.reconcile(&[room("u1", "Living Room")]);
        registry.apply(&DriverEvent::EndpointCreated {
            room_uuid: "u1".to_owned(),
            id: EndpointId(1),
        });

        let commands = registry.reconcile(&[]);
        assert_eq!(
            commands,
            vec![ControlCommand::DestroyEndpoint { id: EndpointId(1) }]
        );
        assert_eq!(registry.statuses()[0].phase, EndpointPhase::PendingDestroy);
        assert!(
            registry.reconcile(&[]).is_empty(),
            "a pending destroy must not be re-issued"
        );
    }

    #[test]
    fn destroyed_event_drops_entry_and_allows_recreate() {
        let mut registry = EndpointRegistry::new();
        let _ = registry.reconcile(&[room("u1", "Living Room")]);
        registry.apply(&DriverEvent::EndpointCreated {
            room_uuid: "u1".to_owned(),
            id: EndpointId(1),
        });
        let _ = registry.reconcile(&[]);
        registry.apply(&DriverEvent::EndpointDestroyed { id: EndpointId(1) });

        assert!(registry.is_empty());
        assert_eq!(
            registry.reconcile(&[room("u1", "Living Room")]).len(),
            1,
            "a re-appearing room is created again"
        );
    }

    #[test]
    fn create_failure_allows_retry() {
        let mut registry = EndpointRegistry::new();
        let _ = registry.reconcile(&[room("u1", "Living Room")]);
        registry.apply(&DriverEvent::CreateFailed {
            room_uuid: "u1".to_owned(),
            reason: "device busy".to_owned(),
        });

        assert!(registry.is_empty());
        assert_eq!(
            registry.reconcile(&[room("u1", "Living Room")]).len(),
            1,
            "a failed create is retried on the next tick"
        );
    }

    #[test]
    fn destroy_failure_reverts_to_active_for_retry() {
        let mut registry = EndpointRegistry::new();
        let _ = registry.reconcile(&[room("u1", "Living Room")]);
        registry.apply(&DriverEvent::EndpointCreated {
            room_uuid: "u1".to_owned(),
            id: EndpointId(1),
        });
        let _ = registry.reconcile(&[]);
        registry.apply(&DriverEvent::DestroyFailed {
            id: EndpointId(1),
            reason: "in use".to_owned(),
        });

        assert_eq!(registry.active_endpoint("u1"), Some(EndpointId(1)));
        assert_eq!(
            registry.reconcile(&[]),
            vec![ControlCommand::DestroyEndpoint { id: EndpointId(1) }],
            "a failed destroy is retried on the next tick"
        );
    }

    #[test]
    fn duplicate_room_names_are_disambiguated() {
        let mut registry = EndpointRegistry::new();
        let commands = registry.reconcile(&[room("u1", "Office"), room("u2", "Office")]);
        let names: BTreeSet<String> = commands
            .iter()
            .filter_map(|command| match command {
                ControlCommand::CreateEndpoint { device_name, .. } => Some(device_name.clone()),
                ControlCommand::DestroyEndpoint { .. } => None,
            })
            .collect();
        assert!(names.contains("Sonos Office"));
        assert!(names.contains("Sonos Office (2)"));
    }

    #[test]
    fn unknown_room_events_are_ignored() {
        let mut registry = EndpointRegistry::new();
        registry.apply(&DriverEvent::EndpointCreated {
            room_uuid: "ghost".to_owned(),
            id: EndpointId(99),
        });
        assert!(registry.is_empty());
    }
}
