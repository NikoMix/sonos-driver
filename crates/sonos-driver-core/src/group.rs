//! The "All Sonos" aggregate player: one Windows endpoint, every room.
//!
//! # Why grouping rather than parallel streams
//!
//! The obvious implementation — point every room at the same HTTP URL — sounds
//! wrong in practice. Each player fetches, buffers and starts independently, so
//! they land tens to hundreds of milliseconds apart and stay there; in an open
//! plan flat that is an echo, not multiroom audio.
//!
//! Sonos solves this itself with *grouping*: one player coordinates and the
//! others slave to its clock, which is what the app's "Group" button does. A
//! member joins by having its transport URI set to the pseudo-scheme
//! `x-rincon:<coordinator uuid>`, and leaves via
//! `BecomeCoordinatorOfStandaloneGroup`. [`GroupPlayer`] drives exactly that,
//! then plays the bridge's stream on the coordinator alone.
//!
//! # Why whole zone groups are the unit
//!
//! The bridge's "rooms" are already zone-group coordinators, so absorbing one
//! brings its bonded satellites (a Sub, surrounds, a stereo pair) and any rooms
//! the user had grouped with it along for free. Undoing the party is then just
//! `leave_group()` on each absorbed coordinator: it becomes standalone again
//! and its own group reforms exactly as it was. That is why there is no
//! topology snapshot here — moving whole groups is inherently reversible, and a
//! snapshot could only go stale.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use audio_core::{Error, Result};
use sonos_control::controller_for_uuid;

use crate::player::{PlayerControl, MIN_DISCOVERY};

/// One player the aggregate can command, plus the room it belongs to.
///
/// Boxed as a trait object so the end-to-end harness can drive the aggregate
/// with in-process mocks, exactly as [`PlayerControl`] allows for single rooms.
pub struct GroupMember {
    /// The zone-group coordinator's UUID, as the arbiter and the topology know
    /// it.
    pub uuid: String,
    /// Human-readable room name, for logging.
    pub name: String,
    /// Control surface for this coordinator.
    pub player: Box<dyn PlayerControl>,
}

impl std::fmt::Debug for GroupMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupMember")
            .field("uuid", &self.uuid)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Plays one stream across every Sonos room as a single synchronised group.
///
/// Implements [`PlayerControl`], so the whole existing session machinery —
/// stream server, latency accounting, idle release, reconnect — drives it
/// unchanged. The aggregate is just another endpoint whose "player" happens to
/// be the whole household.
#[derive(Debug)]
pub struct GroupPlayer {
    /// The player that renders the stream and clocks everyone else.
    coordinator: GroupMember,
    /// Every other room's coordinator, which the party absorbs.
    members: Vec<GroupMember>,
    /// UUIDs currently joined to the party, so reconciling is a no-op when
    /// nothing changed. Never contains the coordinator's own UUID.
    joined: Mutex<BTreeSet<String>>,
    /// Where to journal the party so it survives a crash. `None` in tests,
    /// which must not touch the machine's state.
    journal: Option<PathBuf>,
}

impl GroupPlayer {
    /// Build an aggregate over `players`, the first of which coordinates.
    ///
    /// The first entry is used rather than, say, the loudest or largest room
    /// because the caller passes rooms in binding order — the same order the
    /// user sees in the Windows sound picker — which keeps the choice stable
    /// across restarts instead of shuffling with discovery timing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when fewer than two rooms are supplied: an
    /// "All Sonos" output over a single room is just that room, and grouping
    /// nothing would leave a confusing duplicate endpoint in the sound picker.
    pub fn new(mut players: Vec<GroupMember>) -> Result<Self> {
        if players.len() < 2 {
            return Err(Error::config(format!(
                "the All Sonos output needs at least two rooms, got {}",
                players.len()
            )));
        }
        let coordinator = players.remove(0);
        Ok(Self {
            coordinator,
            members: players,
            joined: Mutex::new(BTreeSet::new()),
            journal: None,
        })
    }

    /// Journal the party to `path`, so `vad ungroup` can undo it if this
    /// process never gets to disband it itself.
    #[must_use]
    pub fn with_journal(mut self, path: PathBuf) -> Self {
        self.journal = Some(path);
        self
    }

    /// The coordinator's room UUID.
    #[must_use]
    pub fn coordinator_uuid(&self) -> &str {
        &self.coordinator.uuid
    }

    /// Every room UUID this player can drive, coordinator first.
    #[must_use]
    pub fn room_uuids(&self) -> Vec<String> {
        std::iter::once(self.coordinator.uuid.clone())
            .chain(self.members.iter().map(|m| m.uuid.clone()))
            .collect()
    }

    /// Pull every member into the party group.
    fn join_all(&self) -> Result<()> {
        let wanted: Vec<String> = self.members.iter().map(|m| m.uuid.clone()).collect();
        self.reconcile(&wanted)
    }

    /// Make the party consist of exactly `wanted` members (coordinator implied).
    ///
    /// Every member is attempted even after one fails, so a single unreachable
    /// speaker cannot leave the rest of the household half-grouped; the first
    /// error is reported once the pass is complete.
    fn reconcile(&self, wanted: &[String]) -> Result<()> {
        let wanted: BTreeSet<&str> = wanted.iter().map(String::as_str).collect();
        let mut joined = self.lock_joined();
        let mut first_error = None;
        let mut changed = false;

        for member in &self.members {
            let should_join = wanted.contains(member.uuid.as_str());
            let is_joined = joined.contains(&member.uuid);
            if should_join == is_joined {
                continue;
            }

            let outcome = if should_join {
                tracing::info!(room = %member.name, "adding room to the All Sonos group");
                member.player.join_group(&self.coordinator.uuid)
            } else {
                tracing::info!(room = %member.name, "removing room from the All Sonos group");
                member.player.leave_group()
            };

            match outcome {
                Ok(()) => {
                    if should_join {
                        joined.insert(member.uuid.clone());
                    } else {
                        joined.remove(&member.uuid);
                    }
                    changed = true;
                }
                Err(err) => {
                    tracing::warn!(room = %member.name, error = %err, "regrouping failed");
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        if changed {
            self.journal(&joined);
        }

        first_error.map_or(Ok(()), Err)
    }

    /// Persist the current membership, so a crash cannot strand the household
    /// in a party nothing remembers forming.
    ///
    /// Called with the lock held so the journal can never describe a party
    /// older than the one in force.
    fn journal(&self, joined: &BTreeSet<String>) {
        let Some(path) = &self.journal else {
            return;
        };
        let members: Vec<String> = joined.iter().cloned().collect();
        crate::party::record(path, &members);
    }

    /// Recover a poisoned membership set rather than silencing the household:
    /// the worst case is a redundant regroup call, which Sonos tolerates.
    fn lock_joined(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.joined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PlayerControl for GroupPlayer {
    fn set_stream_uri(&self, stream_url: &str, content_type: &str) -> Result<()> {
        // Group first, then hand the coordinator the content: a player that
        // joins *after* the URI is set would start mid-buffer and audibly lag.
        self.join_all()?;
        self.coordinator
            .player
            .set_stream_uri(stream_url, content_type)
    }

    fn play(&self) -> Result<()> {
        self.coordinator.player.play()
    }

    fn stop(&self) -> Result<()> {
        // Stop the audio before dissolving the group, so members do not each
        // briefly resume whatever they were playing before the party started.
        let stopped = self.coordinator.player.stop();
        let ungrouped = self.reconcile(&[]);
        stopped.and(ungrouped)
    }

    fn set_active_rooms(&self, room_uuids: &[String]) -> Result<()> {
        // The coordinator is not a member of its own group, and losing it is
        // handled a level up by stopping the session.
        let members: Vec<String> = room_uuids
            .iter()
            .filter(|uuid| *uuid != &self.coordinator.uuid)
            .cloned()
            .collect();
        self.reconcile(&members)
    }

    fn peer_ip(&self) -> Option<IpAddr> {
        // The stream is fetched by the coordinator alone.
        self.coordinator.player.peer_ip()
    }
}

/// Resolve `room_uuids` into a live [`GroupPlayer`].
///
/// Each UUID is resolved to the controller that accepts transport commands for
/// it, exactly as a single-room session resolves its target, so a room named
/// by a bonded satellite still ends up addressing its coordinator.
///
/// # Errors
///
/// Returns [`Error::Control`] when fewer than two rooms could be reached: the
/// aggregate output only means something across multiple rooms, and silently
/// degrading it to a single room would send the whole household's audio to one
/// speaker with no indication why.
pub(crate) fn resolve_group(
    room_uuids: &[String],
    seed: Option<&str>,
    discovery_timeout_secs: u32,
) -> Result<GroupPlayer> {
    let timeout = Duration::from_secs(u64::from(discovery_timeout_secs)).max(MIN_DISCOVERY);
    let mut members = Vec::with_capacity(room_uuids.len());
    let mut failures = Vec::new();

    for uuid in room_uuids {
        match controller_for_uuid(seed, timeout, uuid) {
            Ok(controller) => members.push(GroupMember {
                uuid: uuid.clone(),
                name: controller.device().room_name.clone(),
                player: Box::new(controller),
            }),
            Err(err) => {
                // One unreachable room must not cost the user the whole
                // aggregate output; the rest of the household still groups.
                tracing::warn!(%uuid, error = %err, "room unavailable for the All Sonos output");
                failures.push(format!("{uuid}: {err}"));
            }
        }
    }

    if members.len() < 2 {
        return Err(Error::control(format!(
            "the All Sonos output needs at least two reachable rooms, resolved {}: {}",
            members.len(),
            failures.join("; ")
        )));
    }

    GroupPlayer::new(members).map(|group| group.with_journal(crate::party::default_journal_path()))
}

/// Split every room a previous run left in the "All Sonos" party.
///
/// The escape hatch for a service that was killed mid-party: the speakers
/// remember the grouping, nothing in the Windows UI does, and the driver
/// renders no interface of its own to undo it from.
///
/// Only rooms named in the journal are touched, so a group the user made
/// themselves survives. Unreachable rooms are reported but do not stop the
/// rest, and the journal is cleared regardless — a room that cannot be reached
/// now will not be reachable by a retry either, and keeping the record would
/// make every later run try to split it again.
///
/// Returns the number of rooms released.
///
/// # Errors
///
/// Returns [`Error::Control`] if no room could be released at all, which means
/// the household is probably still grouped.
pub fn disband(
    journal: &std::path::Path,
    seed: Option<&str>,
    discovery_timeout_secs: u32,
) -> Result<usize> {
    let recorded = crate::party::recorded(journal);
    if recorded.is_empty() {
        return Ok(0);
    }

    let timeout = Duration::from_secs(u64::from(discovery_timeout_secs)).max(MIN_DISCOVERY);
    let mut released = 0;
    let mut failures = Vec::new();

    for uuid in &recorded {
        let outcome =
            controller_for_uuid(seed, timeout, uuid).and_then(|player| player.leave_group());
        match outcome {
            Ok(()) => {
                tracing::info!(%uuid, "released room from the All Sonos group");
                released += 1;
            }
            Err(err) => {
                tracing::warn!(%uuid, error = %err, "could not release room from the All Sonos group");
                failures.push(format!("{uuid}: {err}"));
            }
        }
    }

    crate::party::clear(journal);

    if released == 0 {
        return Err(Error::control(format!(
            "could not release any of the {} recorded room(s): {}",
            recorded.len(),
            failures.join("; ")
        )));
    }
    Ok(released)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records what the aggregate did to one player.
    #[derive(Debug, Default)]
    struct Log {
        events: Mutex<Vec<String>>,
    }

    impl Log {
        fn push(&self, event: impl Into<String>) {
            self.events.lock().expect("log lock").push(event.into());
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().expect("log lock").clone()
        }
    }

    struct FakePlayer {
        name: &'static str,
        log: Arc<Log>,
        fail: bool,
    }

    impl PlayerControl for FakePlayer {
        fn set_stream_uri(&self, stream_url: &str, _content_type: &str) -> Result<()> {
            if self.fail {
                return Err(Error::control("unreachable"));
            }
            self.log.push(format!("{}:uri:{stream_url}", self.name));
            Ok(())
        }

        fn play(&self) -> Result<()> {
            self.log.push(format!("{}:play", self.name));
            Ok(())
        }

        fn stop(&self) -> Result<()> {
            if self.fail {
                return Err(Error::control("unreachable"));
            }
            self.log.push(format!("{}:stop", self.name));
            Ok(())
        }

        fn join_group(&self, coordinator_uuid: &str) -> Result<()> {
            if self.fail {
                return Err(Error::control("unreachable"));
            }
            self.log
                .push(format!("{}:join:{coordinator_uuid}", self.name));
            Ok(())
        }

        fn leave_group(&self) -> Result<()> {
            if self.fail {
                return Err(Error::control("unreachable"));
            }
            self.log.push(format!("{}:leave", self.name));
            Ok(())
        }
    }

    fn member(name: &'static str, log: &Arc<Log>, fail: bool) -> GroupMember {
        GroupMember {
            uuid: format!("RINCON_{}", name.to_uppercase()),
            name: name.to_owned(),
            player: Box::new(FakePlayer {
                name,
                log: Arc::clone(log),
                fail,
            }),
        }
    }

    fn player(log: &Arc<Log>) -> GroupPlayer {
        GroupPlayer::new(vec![
            member("living", log, false),
            member("kitchen", log, false),
            member("bath", log, false),
        ])
        .expect("three rooms is enough")
    }

    #[test]
    fn refuses_to_aggregate_fewer_than_two_rooms() {
        let log = Arc::new(Log::default());
        assert!(GroupPlayer::new(vec![]).is_err());
        assert!(GroupPlayer::new(vec![member("living", &log, false)]).is_err());
    }

    #[test]
    fn starting_groups_every_member_before_handing_the_coordinator_content() {
        let log = Arc::new(Log::default());
        let group = player(&log);

        group
            .set_stream_uri("http://host:8080/s.flac", "audio/flac")
            .expect("start");

        assert_eq!(
            log.events(),
            vec![
                "kitchen:join:RINCON_LIVING",
                "bath:join:RINCON_LIVING",
                "living:uri:http://host:8080/s.flac",
            ]
        );
    }

    #[test]
    fn stopping_silences_the_coordinator_then_dissolves_the_group() {
        let log = Arc::new(Log::default());
        let group = player(&log);
        group
            .set_stream_uri("http://host:8080/s.flac", "audio/flac")
            .expect("start");

        group.stop().expect("stop");

        assert_eq!(
            log.events()[3..],
            ["living:stop", "kitchen:leave", "bath:leave"]
        );
    }

    #[test]
    fn losing_one_room_ungroups_only_that_room() {
        let log = Arc::new(Log::default());
        let group = player(&log);
        group
            .set_stream_uri("http://host:8080/s.flac", "audio/flac")
            .expect("start");

        // The arbiter says the kitchen was taken by its own endpoint.
        group
            .set_active_rooms(&["RINCON_LIVING".to_owned(), "RINCON_BATH".to_owned()])
            .expect("regroup");

        assert_eq!(log.events()[3..], ["kitchen:leave"]);
    }

    #[test]
    fn a_room_coming_back_rejoins_the_party() {
        let log = Arc::new(Log::default());
        let group = player(&log);
        group
            .set_stream_uri("http://host:8080/s.flac", "audio/flac")
            .expect("start");
        group
            .set_active_rooms(&["RINCON_LIVING".to_owned(), "RINCON_BATH".to_owned()])
            .expect("regroup");

        group
            .set_active_rooms(&group.room_uuids())
            .expect("regroup");

        assert_eq!(log.events()[4..], ["kitchen:join:RINCON_LIVING"]);
    }

    #[test]
    fn reconciling_an_unchanged_membership_issues_no_calls() {
        let log = Arc::new(Log::default());
        let group = player(&log);
        group
            .set_stream_uri("http://host:8080/s.flac", "audio/flac")
            .expect("start");
        let before = log.events().len();

        // Runs several times a second in production.
        for _ in 0..10 {
            group
                .set_active_rooms(&group.room_uuids())
                .expect("regroup");
        }

        assert_eq!(log.events().len(), before);
    }

    #[test]
    fn one_unreachable_speaker_does_not_strand_the_others() {
        let log = Arc::new(Log::default());
        let group = GroupPlayer::new(vec![
            member("living", &log, false),
            member("dead", &log, true),
            member("bath", &log, false),
        ])
        .expect("three rooms");

        let result = group.set_stream_uri("http://host:8080/s.flac", "audio/flac");

        assert!(result.is_err(), "the failure must be reported");
        // ...but the reachable room was still grouped.
        assert!(log.events().contains(&"bath:join:RINCON_LIVING".to_owned()));
    }

    #[test]
    fn a_member_that_failed_to_join_is_retried_next_tick() {
        let log = Arc::new(Log::default());
        let group = GroupPlayer::new(vec![
            member("living", &log, false),
            member("dead", &log, true),
        ])
        .expect("two rooms");

        assert!(group.set_stream_uri("http://h/s", "audio/flac").is_err());
        // Not recorded as joined, so the next reconcile tries again rather than
        // assuming a speaker that never answered is in the group.
        assert!(!group.lock_joined().contains("RINCON_DEAD"));
    }
}
