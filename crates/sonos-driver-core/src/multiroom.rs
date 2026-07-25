//! Multi-room supervision: one [`Bridge`] per bound `sonosvad` endpoint.
//!
//! This is the piece that turns "a bridge that streams one endpoint to one
//! room" into the product the kernel driver exists for: every Sonos room shows
//! up as its own Windows audio output, and whatever Windows renders to that
//! output lands in that room.
//!
//! # How a session is derived
//!
//! For each [`Binding`] produced by
//! [`sonos_vad_endpoint::bind_rooms`](https://docs.rs/sonos-vad-endpoint) the
//! supervisor clones the template [`Config`] and overrides exactly three things:
//!
//! - `capture.device` — the endpoint's stable WASAPI id, so this bridge
//!   loopback-captures *only* that virtual output;
//! - `sonos.uuid` — the room's stable UUID, so control resolves the right
//!   group coordinator even if the room is renamed;
//! - `stream.port` — `template.stream.port + index`, because each bridge runs
//!   its own HTTP server and two servers cannot share a port.
//!
//! `capture.process` is cleared: per-application loopback and per-endpoint
//! loopback are mutually exclusive, and the endpoint *is* the selector here.
//!
//! # Failure policy
//!
//! Rooms are independent. A room whose bridge fails is logged and dropped; the
//! others keep streaming. The supervisor only returns an error when *nothing*
//! could be started, which is the case that actually needs the operator's
//! attention.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use audio_core::{Config, Error, Result};
use sonos_vad_endpoint::{AggregateBinding, Binding, PlanOptions, SystemEndpoints, VadEndpoints};

use crate::{Arbiter, Bridge, SessionId, StopHandle};

/// What a session's audio is sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    /// One Sonos room, identified by its zone-group coordinator UUID.
    Room {
        /// Stable UUID of the room's coordinator.
        uuid: String,
    },
    /// Every room at once, played as one synchronised Sonos group.
    ///
    /// Carries all room UUIDs in binding order; the first coordinates the
    /// group. See [`crate::GroupPlayer`] for why grouping is used instead of
    /// streaming to each room in parallel.
    AllRooms {
        /// Every room's coordinator UUID, coordinator of the party first.
        uuids: Vec<String>,
    },
}

/// One planned streaming session: one Windows endpoint, one Sonos target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomSession {
    /// Stable UUID of the Sonos room this session streams to.
    ///
    /// For the aggregate session this is the group coordinator's UUID.
    pub room_uuid: String,
    /// Human-facing room name, for logs and UI.
    pub room_name: String,
    /// The Windows audio output the user selects to reach this room.
    pub device_name: String,
    /// Stable WASAPI endpoint id that this session loopback-captures.
    pub device_id: String,
    /// TCP port this session's stream server binds to.
    pub port: u16,
    /// Which Sonos player(s) this session drives.
    pub target: SessionTarget,
}

impl RoomSession {
    /// Every Sonos room this session wants to own, for the arbiter.
    #[must_use]
    pub fn claimed_rooms(&self) -> Vec<String> {
        match &self.target {
            SessionTarget::Room { uuid } => vec![uuid.clone()],
            SessionTarget::AllRooms { uuids } => uuids.clone(),
        }
    }

    /// Whether this is the aggregate "All Sonos" session.
    #[must_use]
    pub fn is_aggregate(&self) -> bool {
        matches!(self.target, SessionTarget::AllRooms { .. })
    }

    /// This session's identity in the shared [`Arbiter`].
    ///
    /// Derived from the stream port, which the planner already guarantees is
    /// unique per session and stable across restarts, so no separate counter
    /// has to be kept in step with the plan.
    #[must_use]
    pub fn arbiter_id(&self) -> SessionId {
        SessionId(u64::from(self.port))
    }

    /// The fully derived [`Config`] for this session.
    #[must_use]
    pub fn config(&self, template: &Config) -> Config {
        let mut config = template.clone();
        config.capture.device = Some(self.device_id.clone());
        // The endpoint is the selector; per-application loopback would fight it.
        config.capture.process = None;
        config.capture.exclude_process = false;
        config.sonos.uuid = Some(self.room_uuid.clone());
        config.sonos.room = Some(self.room_name.clone());
        config.stream.port = self.port;
        config
    }
}

/// Derive the per-room session plan from a set of bindings.
///
/// Ports are allocated from `base_port` in binding order, so the plan is
/// deterministic and a room keeps its port as long as its slot is stable.
///
/// # Errors
///
/// Returns [`Error::Config`] when the port range would overflow `u16`.
pub fn sessions_for(bindings: &[Binding], base_port: u16) -> Result<Vec<RoomSession>> {
    sessions_for_all(bindings, None, base_port)
}

/// [`sessions_for`], plus the aggregate "All Sonos" session when one was bound.
///
/// The aggregate is placed **last** so every room keeps the port it would have
/// had without it; a stream port that moved whenever the household grew or
/// shrank would invalidate any firewall rule the user had written.
///
/// # Errors
///
/// Returns [`Error::Config`] when the port range would overflow `u16`.
pub fn sessions_for_all(
    bindings: &[Binding],
    aggregate: Option<&AggregateBinding>,
    base_port: u16,
) -> Result<Vec<RoomSession>> {
    let total = bindings.len() + usize::from(aggregate.is_some());
    let port_for = |index: usize| -> Result<u16> {
        u16::try_from(index)
            .ok()
            .and_then(|o| base_port.checked_add(o))
            .ok_or_else(|| {
                Error::config(format!(
                    "{total} sessions starting at port {base_port} exceeds the port range; lower stream.port"
                ))
            })
    };

    let mut sessions: Vec<RoomSession> = bindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            Ok(RoomSession {
                room_uuid: binding.room.uuid.clone(),
                room_name: binding.room.name.clone(),
                device_name: binding.name.clone(),
                device_id: binding.device_id.clone(),
                port: port_for(index)?,
                target: SessionTarget::Room {
                    uuid: binding.room.uuid.clone(),
                },
            })
        })
        .collect::<Result<_>>()?;

    if let Some(aggregate) = aggregate {
        let uuids: Vec<String> = aggregate.rooms.iter().map(|r| r.uuid.clone()).collect();
        // The coordinator is the first room, matching GroupPlayer::new.
        let coordinator = uuids
            .first()
            .cloned()
            .ok_or_else(|| Error::config("the All Sonos output was bound to no rooms"))?;
        let coordinator_name = aggregate
            .rooms
            .first()
            .map(|r| r.name.clone())
            .unwrap_or_default();
        sessions.push(RoomSession {
            room_uuid: coordinator,
            room_name: coordinator_name,
            device_name: aggregate.name.clone(),
            device_id: aggregate.device_id.clone(),
            port: port_for(bindings.len())?,
            target: SessionTarget::AllRooms { uuids },
        });
    }

    Ok(sessions)
}

/// Supervises one [`Bridge`] per bound `sonosvad` endpoint.
#[derive(Debug)]
pub struct MultiRoomBridge {
    config: Config,
    stop: Arc<AtomicBool>,
}

impl MultiRoomBridge {
    /// Create a supervisor from the template [`Config`] every room inherits.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The template configuration each room's session is derived from.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A cloneable handle that stops every room's bridge.
    ///
    /// Acquire it before calling [`run`](Self::run), which blocks.
    #[must_use]
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle::from_flag(Arc::clone(&self.stop))
    }

    /// Share an existing handle's stop flag instead of this supervisor's own.
    ///
    /// A long-running host -- the Windows service -- owns one stop signal but
    /// builds a fresh supervisor on every re-plan. Without this, a stop arriving
    /// mid-cycle would be delivered to a handle nobody is watching, and the
    /// service would hang until the SCM killed it.
    ///
    /// Any handle previously returned by [`stop_handle`](Self::stop_handle) is
    /// detached by this call, so adopt before handing handles out.
    pub fn adopt_stop_handle(&mut self, handle: &StopHandle) {
        self.stop = handle.flag();
    }

    /// Bind the discovered rooms to `sonosvad` endpoints and stream to all of
    /// them until stopped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] when the driver is not installed or its
    /// endpoints cannot be renamed, and [`Error::Config`] when no room could be
    /// bound.
    pub fn run(&mut self, rooms: &[sonos_vad_control::RoomKey]) -> Result<()> {
        let sessions = self.plan(&SystemEndpoints::new(), rooms)?;
        self.run_sessions(&sessions)
    }

    /// Bind `rooms` to endpoints and derive the session plan, without starting
    /// anything.
    ///
    /// Exposed so the CLI's `--dry-run` and the GUI can show the plan, and so
    /// tests can drive it against an in-memory endpoint fake.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] when binding fails, and [`Error::Config`]
    /// when no room could be bound.
    pub fn plan<E: VadEndpoints + ?Sized>(
        &self,
        endpoints: &E,
        rooms: &[sonos_vad_control::RoomKey],
    ) -> Result<Vec<RoomSession>> {
        self.plan_with(endpoints, rooms, PlanOptions::default())
    }

    /// [`MultiRoomBridge::plan`], with spare-hiding and the aggregate
    /// "All Sonos" output selected by `options`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] when binding fails, and [`Error::Config`]
    /// when no room could be bound.
    pub fn plan_with<E: VadEndpoints + ?Sized>(
        &self,
        endpoints: &E,
        rooms: &[sonos_vad_control::RoomKey],
        options: PlanOptions,
    ) -> Result<Vec<RoomSession>> {
        if rooms.is_empty() {
            return Err(Error::config(
                "no Sonos rooms were discovered; nothing to bind",
            ));
        }

        let bindings = sonos_vad_endpoint::bind_rooms_for(endpoints, rooms, options)
            .map_err(|err| Error::control(format!("binding Sonos rooms to endpoints: {err}")))?;

        let sessions = sessions_for_all(
            &bindings.rooms,
            bindings.aggregate.as_ref(),
            self.config.stream.port,
        )?;
        if sessions.is_empty() {
            return Err(Error::config(
                "the sonosvad driver exposed no usable endpoints",
            ));
        }
        Ok(sessions)
    }

    /// Run one bridge per session, blocking until every one has finished.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] only when *no* session could be started;
    /// individual room failures are logged and the rest keep running.
    pub fn run_sessions(&mut self, sessions: &[RoomSession]) -> Result<()> {
        let seed = self.config.sonos.seed_host.clone();
        let timeout = self.config.sonos.discovery_timeout_secs;
        // One shared map for every session, so "All Sonos" and the per-room
        // outputs hand speakers back and forth instead of fighting over them.
        let arbiter = Arc::new(Arbiter::new());
        self.run_sessions_with(sessions, move |session, config, stop| {
            let mut bridge = Bridge::new(config);
            bridge.adopt_stop_flag(stop);
            bridge.arbitrate(
                Arc::clone(&arbiter),
                session.arbiter_id(),
                session.claimed_rooms(),
            );
            match &session.target {
                SessionTarget::Room { .. } => bridge.run(),
                SessionTarget::AllRooms { uuids } => {
                    let group = crate::group::resolve_group(uuids, seed.as_deref(), timeout)?;
                    let source = crate::open_capture(&bridge.config().capture)?;
                    bridge.run_with(source, Box::new(group))
                }
            }
        })
    }

    /// The dependency-injection seam behind [`run_sessions`](Self::run_sessions).
    ///
    /// `start` is invoked on a dedicated thread per session and must block for
    /// the lifetime of that room's stream, observing the shared stop flag. Tests
    /// substitute a closure that records the derived config instead of touching
    /// audio hardware or the LAN.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Control`] when every session failed.
    pub fn run_sessions_with<F>(&mut self, sessions: &[RoomSession], start: F) -> Result<()>
    where
        F: Fn(&RoomSession, Config, Arc<AtomicBool>) -> Result<()> + Send + Sync + 'static,
    {
        let start = Arc::new(start);
        let failures = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut handles = Vec::with_capacity(sessions.len());

        for session in sessions {
            let config = session.config(&self.config);
            let stop = Arc::clone(&self.stop);
            let start = Arc::clone(&start);
            let failures = Arc::clone(&failures);
            let label = format!("{} -> {}", session.device_name, session.room_name);
            let owned = session.clone();

            tracing::info!(
                room = %session.room_name,
                device = %session.device_name,
                port = session.port,
                aggregate = session.is_aggregate(),
                "starting room bridge"
            );

            let handle = thread::Builder::new()
                .name(format!("sonos-room-{}", session.port))
                .spawn(move || {
                    if let Err(err) = start(&owned, config, stop) {
                        tracing::error!(session = %label, %err, "room bridge stopped with an error");
                        // A poisoned lock means another room already panicked;
                        // the failure list is best-effort telemetry either way.
                        if let Ok(mut failures) = failures.lock() {
                            failures.push(format!("{label}: {err}"));
                        }
                    }
                })
                .map_err(|err| Error::control(format!("spawning room bridge thread: {err}")))?;
            handles.push(handle);
        }

        let started = handles.len();
        for handle in handles {
            // A panicking room must not take the supervisor down; the thread's
            // own error path has already logged whatever it knew.
            if handle.join().is_err() {
                tracing::error!("a room bridge thread panicked");
            }
        }

        // A poisoned mutex means a room panicked while holding it; treat that as
        // "everything failed" rather than silently reporting success.
        let failures = failures
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |f| f.clone());

        if started > 0 && failures.len() == started {
            return Err(Error::control(format!(
                "every room bridge failed: {}",
                failures.join("; ")
            )));
        }
        Ok(())
    }

    /// Whether a stop has been requested.
    #[must_use]
    pub fn is_stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sonos_vad_control::RoomKey;
    use sonos_vad_endpoint::{factory_name_for_slot, VadEndpoint, DEFAULT_ADAPTER_NAME};

    use super::*;

    struct FakeEndpoints {
        endpoints: Mutex<Vec<VadEndpoint>>,
    }

    impl FakeEndpoints {
        fn with_slots(n: u32) -> Self {
            let endpoints = (1..=n)
                .map(|slot| {
                    let desc = factory_name_for_slot(slot);
                    VadEndpoint {
                        device_id: format!("{{0.0.0.00000000}}.{{slot-{slot}}}"),
                        friendly_name: desc.clone(),
                        device_desc: desc,
                        adapter: DEFAULT_ADAPTER_NAME.to_owned(),
                        slot: Some(slot),
                        active: true,
                        disabled: false,
                    }
                })
                .collect();
            Self {
                endpoints: Mutex::new(endpoints),
            }
        }
    }

    impl VadEndpoints for FakeEndpoints {
        fn list(&self) -> sonos_vad_endpoint::Result<Vec<VadEndpoint>> {
            Ok(self.endpoints.lock().expect("lock").clone())
        }

        fn rename(&self, device_id: &str, name: &str) -> sonos_vad_endpoint::Result<()> {
            let mut endpoints = self.endpoints.lock().expect("lock");
            for endpoint in endpoints.iter_mut() {
                if endpoint.device_id == device_id {
                    endpoint.friendly_name = name.to_owned();
                    return Ok(());
                }
            }
            Err(sonos_vad_endpoint::Error::NotFound(device_id.to_owned()))
        }

        fn set_visible(&self, device_id: &str, visible: bool) -> sonos_vad_endpoint::Result<()> {
            let mut endpoints = self.endpoints.lock().expect("lock");
            for endpoint in endpoints.iter_mut() {
                if endpoint.device_id == device_id {
                    endpoint.active = visible;
                    endpoint.disabled = !visible;
                    return Ok(());
                }
            }
            Err(sonos_vad_endpoint::Error::NotFound(device_id.to_owned()))
        }
    }

    fn rooms() -> Vec<RoomKey> {
        vec![
            RoomKey::new("RINCON_1", "Living Room"),
            RoomKey::new("RINCON_2", "Kitchen"),
        ]
    }

    #[test]
    fn plan_assigns_one_endpoint_and_one_port_per_room() {
        let supervisor = MultiRoomBridge::new(Config::default());
        let sessions = supervisor
            .plan(&FakeEndpoints::with_slots(4), &rooms())
            .expect("plan");

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].room_name, "Living Room");
        assert_eq!(sessions[0].device_name, "Sonos Living Room");
        assert_eq!(sessions[0].port, supervisor.config().stream.port);
        assert_eq!(sessions[1].port, supervisor.config().stream.port + 1);
        assert_ne!(sessions[0].device_id, sessions[1].device_id);
    }

    #[test]
    fn plan_rejects_an_empty_room_set_without_touching_the_driver() {
        let supervisor = MultiRoomBridge::new(Config::default());
        let error = supervisor
            .plan(&FakeEndpoints::with_slots(4), &[])
            .expect_err("no rooms");
        assert!(matches!(error, Error::Config(_)));
    }

    #[test]
    fn derived_config_targets_the_endpoint_and_the_room() {
        let mut template = Config::default();
        template.capture.process = Some("spotify.exe".to_owned());
        template.capture.exclude_process = true;

        let session = RoomSession {
            room_uuid: "RINCON_1".to_owned(),
            room_name: "Living Room".to_owned(),
            device_name: "Sonos Living Room".to_owned(),
            device_id: "{0.0.0.00000000}.{slot-1}".to_owned(),
            port: 9100,
            target: SessionTarget::Room {
                uuid: "RINCON_1".to_owned(),
            },
        };
        let config = session.config(&template);

        assert_eq!(
            config.capture.device.as_deref(),
            Some(session.device_id.as_str())
        );
        assert_eq!(config.sonos.uuid.as_deref(), Some("RINCON_1"));
        assert_eq!(config.stream.port, 9100);
        // Process loopback must not survive: the endpoint is the selector.
        assert!(config.capture.process.is_none());
        assert!(!config.capture.exclude_process);
        // Everything else is inherited untouched.
        assert_eq!(config.stream.profile, template.stream.profile);
        assert_eq!(config.pipeline, template.pipeline);
    }

    #[test]
    fn port_allocation_overflow_is_a_config_error() {
        let bindings = vec![
            Binding {
                device_id: "a".to_owned(),
                room: RoomKey::new("u1", "A"),
                name: "Sonos A".to_owned(),
                rename_needed: false,
            },
            Binding {
                device_id: "b".to_owned(),
                room: RoomKey::new("u2", "B"),
                name: "Sonos B".to_owned(),
                rename_needed: false,
            },
        ];
        assert!(matches!(
            sessions_for(&bindings, u16::MAX),
            Err(Error::Config(_))
        ));
    }

    #[test]
    fn every_room_gets_its_own_bridge_and_stop_is_shared() {
        let supervisor_config = Config::default();
        let mut supervisor = MultiRoomBridge::new(supervisor_config);
        let sessions = supervisor
            .plan(&FakeEndpoints::with_slots(4), &rooms())
            .expect("plan");

        let seen = Arc::new(Mutex::new(BTreeMap::<u16, String>::new()));
        let recorder = Arc::clone(&seen);

        supervisor
            .run_sessions_with(&sessions, move |_session, config, stop| {
                // Every session must observe the supervisor's shared stop flag.
                assert!(!stop.load(Ordering::SeqCst));
                recorder.lock().expect("lock").insert(
                    config.stream.port,
                    config.capture.device.clone().unwrap_or_default(),
                );
                Ok(())
            })
            .expect("all sessions ran");

        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 2);
        assert!(seen.values().all(|id| id.contains("slot-")));
    }

    #[test]
    fn a_single_room_failure_does_not_fail_the_run() {
        let mut supervisor = MultiRoomBridge::new(Config::default());
        let sessions = supervisor
            .plan(&FakeEndpoints::with_slots(4), &rooms())
            .expect("plan");
        let base = supervisor.config().stream.port;

        supervisor
            .run_sessions_with(&sessions, move |_session, config, _stop| {
                if config.stream.port == base {
                    Err(Error::control("simulated room failure"))
                } else {
                    Ok(())
                }
            })
            .expect("the surviving room keeps the run alive");
    }

    #[test]
    fn every_room_failing_is_reported() {
        let mut supervisor = MultiRoomBridge::new(Config::default());
        let sessions = supervisor
            .plan(&FakeEndpoints::with_slots(4), &rooms())
            .expect("plan");

        let error = supervisor
            .run_sessions_with(&sessions, |_session, _config, _stop| {
                Err(Error::control("simulated room failure"))
            })
            .expect_err("all rooms failed");
        assert!(matches!(error, Error::Control(_)));
    }
}
