//! The message contract between the user-mode service and the kernel driver.

use serde::{Deserialize, Serialize};

use crate::room::RoomKey;

/// Opaque identifier the kernel driver assigns to a live render endpoint.
///
/// The user-mode service treats it as a handle: it is produced by the driver in
/// an [`DriverEvent::EndpointCreated`] reply and quoted back in a
/// [`ControlCommand::DestroyEndpoint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EndpointId(pub u32);

/// A command issued by the user-mode service to the kernel driver.
///
/// Commands are produced by [`EndpointRegistry::reconcile`] and are intended to
/// be serialised (for example over a custom IOCTL or a named pipe) in a later
/// phase; the types derive `serde` so that wire format is not re-invented.
///
/// [`EndpointRegistry::reconcile`]: crate::EndpointRegistry::reconcile
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlCommand {
    /// Create a virtual render endpoint for `room`, surfaced to Windows under
    /// the unique friendly name `device_name`.
    CreateEndpoint {
        /// The Sonos room the new endpoint streams to.
        room: RoomKey,
        /// The unique Windows friendly name for the device.
        device_name: String,
    },
    /// Destroy the endpoint previously created and identified by `id`.
    DestroyEndpoint {
        /// The driver-assigned id of the endpoint to remove.
        id: EndpointId,
    },
}

/// An asynchronous reply reported by the kernel driver to the user-mode service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverEvent {
    /// The endpoint for the room with UUID `room_uuid` is live as `id`.
    EndpointCreated {
        /// UUID of the room whose endpoint was created (matches [`RoomKey::uuid`]).
        room_uuid: String,
        /// The id the driver assigned to the new endpoint.
        id: EndpointId,
    },
    /// The endpoint `id` has been removed.
    EndpointDestroyed {
        /// The id of the endpoint that was destroyed.
        id: EndpointId,
    },
    /// The driver could not create an endpoint for room `room_uuid`.
    CreateFailed {
        /// UUID of the room whose endpoint creation failed.
        room_uuid: String,
        /// Human-readable failure reason for logs and telemetry.
        reason: String,
    },
    /// The driver could not destroy endpoint `id`.
    DestroyFailed {
        /// The id of the endpoint whose destruction failed.
        id: EndpointId,
        /// Human-readable failure reason for logs and telemetry.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_command_round_trips_through_json() {
        let command = ControlCommand::CreateEndpoint {
            room: RoomKey::new("RINCON_1", "Living Room"),
            device_name: "Sonos Living Room".to_owned(),
        };
        let json = serde_json::to_string(&command).expect("serialise");
        let back: ControlCommand = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(command, back);
    }

    #[test]
    fn driver_event_round_trips_through_json() {
        let event = DriverEvent::EndpointCreated {
            room_uuid: "RINCON_1".to_owned(),
            id: EndpointId(3),
        };
        let json = serde_json::to_string(&event).expect("serialise");
        let back: DriverEvent = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(event, back);
    }
}
