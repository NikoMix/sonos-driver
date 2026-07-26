//! Decides which session owns which Sonos room.
//!
//! Windows lets several applications render to different endpoints at the same
//! time, so the "All Sonos" endpoint and an individual room endpoint can both
//! carry audio at once — but a Sonos player can only obey one of them. The rule
//! is **whoever started playing most recently wins**: starting audio is a
//! deliberate act, so the newest one is what the user is currently listening
//! for.
//!
//! "Started" means the silent → audible transition tracked by
//! [`crate::idle::IdleTracker`], not "is producing samples". A session that is
//! pre-empted mid-stream stays pre-empted until it falls silent and starts
//! again; otherwise two continuously-playing sessions would fight over the same
//! speakers forever, several times a second.
//!
//! A room nobody holds is free for the taking, so when the winner goes quiet
//! its rooms fall back to whichever session is still playing rather than going
//! silent until the user intervenes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Identifies one streaming session (one Windows endpoint) to the arbiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// Shared ownership map over the household's rooms.
///
/// Cheap to consult: sessions poll it on their supervise tick, so every method
/// takes the lock only briefly and none of them perform I/O.
#[derive(Debug)]
pub struct Arbiter {
    /// Room UUID to the session that currently owns it.
    ///
    /// No timestamp is kept: "most recent wins" is exactly last-write-wins on
    /// this map, so an ordering counter could only restate what the insertion
    /// already decided.
    claims: Mutex<HashMap<String, SessionId>>,
}

impl Default for Arbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Arbiter {
    /// An arbiter with no rooms claimed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            claims: Mutex::new(HashMap::new()),
        }
    }

    /// Take every listed room for `owner`, displacing any previous holder.
    ///
    /// Called when a session's audio *starts*. It always succeeds: that is what
    /// "most recent wins" means.
    pub fn claim(&self, owner: SessionId, rooms: &[String]) {
        let mut claims = self.lock();
        for room in rooms {
            claims.insert(room.clone(), owner);
        }
    }

    /// Take only the listed rooms that nobody currently holds.
    ///
    /// Lets a session that is still playing pick up rooms freed by the session
    /// that pre-empted it, without letting it barge back in on rooms that are
    /// still in use.
    ///
    /// Returns the rooms newly acquired.
    pub fn claim_free(&self, owner: SessionId, rooms: &[String]) -> Vec<String> {
        let mut claims = self.lock();
        let mut taken = Vec::new();
        for room in rooms {
            if !claims.contains_key(room) {
                claims.insert(room.clone(), owner);
                taken.push(room.clone());
            }
        }
        taken
    }

    /// Give up every room held by `owner`.
    ///
    /// Called when a session goes silent or stops, so its rooms become
    /// available again.
    pub fn release_all(&self, owner: SessionId) {
        let mut claims = self.lock();
        claims.retain(|_, holder| *holder != owner);
    }

    /// Which of `rooms` `owner` currently holds, in the order given.
    ///
    /// This is the query a session's supervise tick uses to reconcile: the
    /// answer shrinking means it has been pre-empted, growing means rooms came
    /// back.
    #[must_use]
    pub fn owned(&self, owner: SessionId, rooms: &[String]) -> Vec<String> {
        let claims = self.lock();
        rooms
            .iter()
            .filter(|room| claims.get(*room).is_some_and(|holder| *holder == owner))
            .cloned()
            .collect()
    }

    /// Whether `owner` holds `room`.
    #[must_use]
    pub fn holds(&self, owner: SessionId, room: &str) -> bool {
        self.lock().get(room).is_some_and(|holder| *holder == owner)
    }

    /// Recover the map even if a session panicked while holding the lock: a
    /// poisoned ownership table is still a usable ownership table, and refusing
    /// to arbitrate would silence every room.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionId>> {
        self.claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One session's stake in the [`Arbiter`].
///
/// Bundles the three things every arbitration decision needs — who is asking,
/// which rooms they want, and the shared map — so the session loop takes one
/// parameter instead of three that could drift out of step.
#[derive(Debug, Clone)]
pub struct Arbitration {
    arbiter: Arc<Arbiter>,
    session: SessionId,
    rooms: Vec<String>,
}

impl Arbitration {
    /// Register `rooms` as the rooms `session` competes for.
    #[must_use]
    pub fn new(arbiter: Arc<Arbiter>, session: SessionId, rooms: Vec<String>) -> Self {
        Self {
            arbiter,
            session,
            rooms,
        }
    }

    /// Take every room this session wants, displacing whoever held them.
    ///
    /// Called when the session's audio starts.
    pub fn claim(&self) {
        self.arbiter.claim(self.session, &self.rooms);
    }

    /// Give up every room this session holds.
    pub fn release(&self) {
        self.arbiter.release_all(self.session);
    }

    /// Pick up any of this session's rooms that nobody else holds, then report
    /// everything it now owns.
    #[must_use]
    pub fn reconcile(&self) -> Vec<String> {
        self.arbiter.claim_free(self.session, &self.rooms);
        self.arbiter.owned(self.session, &self.rooms)
    }

    /// Every room this session competes for.
    #[must_use]
    pub fn rooms(&self) -> &[String] {
        &self.rooms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: SessionId = SessionId(0);
    const KITCHEN: SessionId = SessionId(1);
    const BEDROOM: SessionId = SessionId(2);

    fn rooms(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_most_recent_claim_takes_the_room() {
        let arbiter = Arbiter::new();
        let kitchen = rooms(&["Kitchen"]);

        arbiter.claim(KITCHEN, &kitchen);
        assert!(arbiter.holds(KITCHEN, "Kitchen"));

        // "All Sonos" starts afterwards, so it wins.
        arbiter.claim(ALL, &rooms(&["Kitchen", "Bedroom"]));
        assert!(arbiter.holds(ALL, "Kitchen"));
        assert!(!arbiter.holds(KITCHEN, "Kitchen"));
    }

    #[test]
    fn a_single_room_starting_later_carves_itself_out_of_the_party() {
        let arbiter = Arbiter::new();
        let all = rooms(&["Kitchen", "Bedroom", "Bath"]);
        arbiter.claim(ALL, &all);

        arbiter.claim(KITCHEN, &rooms(&["Kitchen"]));

        // Only the kitchen leaves; the rest of the party plays on. This is what
        // makes the aggregate ungroup one room instead of collapsing.
        assert_eq!(arbiter.owned(ALL, &all), rooms(&["Bedroom", "Bath"]));
        assert!(arbiter.holds(KITCHEN, "Kitchen"));
    }

    #[test]
    fn releasing_frees_only_that_sessions_rooms() {
        let arbiter = Arbiter::new();
        arbiter.claim(ALL, &rooms(&["Kitchen", "Bedroom"]));
        arbiter.claim(KITCHEN, &rooms(&["Kitchen"]));

        arbiter.release_all(ALL);

        assert!(arbiter.holds(KITCHEN, "Kitchen"));
        assert!(arbiter.owned(ALL, &rooms(&["Bedroom"])).is_empty());
    }

    #[test]
    fn a_freed_room_falls_back_to_a_session_that_is_still_playing() {
        let arbiter = Arbiter::new();
        let all = rooms(&["Kitchen", "Bedroom"]);

        // The kitchen pre-empts the party, then goes quiet.
        arbiter.claim(ALL, &all);
        arbiter.claim(KITCHEN, &rooms(&["Kitchen"]));
        arbiter.release_all(KITCHEN);

        // The party is still playing, so it picks the kitchen back up without
        // the user having to restart anything.
        assert_eq!(arbiter.claim_free(ALL, &all), rooms(&["Kitchen"]));
        assert_eq!(arbiter.owned(ALL, &all), all);
    }

    #[test]
    fn claiming_free_rooms_never_steals_one_in_use() {
        let arbiter = Arbiter::new();
        arbiter.claim(BEDROOM, &rooms(&["Bedroom"]));

        let taken = arbiter.claim_free(ALL, &rooms(&["Kitchen", "Bedroom"]));

        assert_eq!(taken, rooms(&["Kitchen"]));
        assert!(arbiter.holds(BEDROOM, "Bedroom"));
    }

    #[test]
    fn owned_reports_nothing_before_anyone_claims() {
        let arbiter = Arbiter::new();
        assert!(arbiter.owned(ALL, &rooms(&["Kitchen"])).is_empty());
        assert!(!arbiter.holds(ALL, "Kitchen"));
    }
}
