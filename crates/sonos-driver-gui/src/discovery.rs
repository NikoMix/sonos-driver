//! Background Sonos discovery.
//!
//! Discovery performs blocking `SSDP` (or, with a seed host, unicast `HTTP`)
//! network I/O, so it runs on a dedicated thread and reports its result back to
//! the UI as an [`Action`] over a channel. The UI thread drains the channel
//! each frame and feeds the action through the reducer, so discovery never
//! blocks rendering.
//!
//! It resolves *rooms*, not players: `discover_rooms` folds the zone-group
//! topology in, so a stereo pair or a bonded surround set is one destination
//! rather than two or five identically named ones.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use sonos_driver_core::discover_rooms;

use crate::action::Action;
use crate::state::Room;

/// Spawn a thread that discovers Sonos rooms and posts the result to `sender`.
///
/// `seed` is the address of any one known player, used instead of multicast on
/// networks where SSDP does not work. On success it sends
/// [`Action::RoomsDiscovered`]; on failure, [`Action::DiscoveryFailed`]. If the
/// receiver has already been dropped (the app is shutting down) the send is
/// silently discarded.
pub fn spawn(seed: Option<String>, timeout: Duration, sender: Sender<Action>) {
    // Cloned so the fallback branch can still report if spawning fails (the
    // closure consumes its own copy).
    let fallback = sender.clone();
    let spawned = thread::Builder::new()
        .name("sonos-discovery".to_owned())
        .spawn(move || {
            let action = match discover_rooms(seed.as_deref(), timeout) {
                Ok(rooms) => {
                    tracing::info!(count = rooms.len(), "sonos discovery completed");
                    Action::RoomsDiscovered(rooms.iter().map(Room::from_key).collect())
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
