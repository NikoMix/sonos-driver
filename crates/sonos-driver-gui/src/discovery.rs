//! Background Sonos discovery.
//!
//! Discovery performs blocking `SSDP` network I/O, so it runs on a dedicated
//! thread and reports its result back to the UI as an [`Action`] over a channel.
//! The UI thread drains the channel each frame and feeds the action through the
//! reducer, so discovery never blocks rendering.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use sonos_driver_core::control::discover;

use crate::action::Action;
use crate::state::Room;

/// Spawn a thread that discovers Sonos rooms and posts the result to `sender`.
///
/// On success it sends [`Action::RoomsDiscovered`]; on failure,
/// [`Action::DiscoveryFailed`]. If the receiver has already been dropped (the
/// app is shutting down) the send is silently discarded.
pub fn spawn(timeout: Duration, sender: Sender<Action>) {
    // Cloned so the fallback branch can still report if spawning fails (the
    // closure consumes its own copy).
    let fallback = sender.clone();
    let spawned = thread::Builder::new()
        .name("sonos-discovery".to_owned())
        .spawn(move || {
            let action = match discover(timeout) {
                Ok(devices) => {
                    tracing::info!(count = devices.len(), "sonos discovery completed");
                    Action::RoomsDiscovered(devices.iter().map(Room::from_device).collect())
                }
                Err(err) => {
                    tracing::warn!(%err, "sonos discovery failed");
                    Action::DiscoveryFailed(err.to_string())
                }
            };
            let _ = sender.send(action);
        });

    if let Err(err) = spawned {
        // Spawning a thread fails only under severe OS resource pressure; report
        // it as a discovery failure rather than panicking the UI.
        tracing::error!(%err, "failed to spawn the discovery thread");
        let _ = fallback.send(Action::DiscoveryFailed(
            "could not start the discovery thread".to_owned(),
        ));
    }
}
