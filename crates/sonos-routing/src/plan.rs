//! Turning routes into a concrete, conflict-free streaming plan.

use std::collections::HashMap;

use sonos_vad_control::RoomKey;

use crate::{PlanError, RoomSelector, Route, Source};

/// One route, resolved against a real household and given a stream port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlannedRoute {
    /// The audio this session captures.
    pub source: Source,
    /// The rooms it plays in. Never empty; the first coordinates the group.
    pub rooms: Vec<RoomKey>,
    /// TCP port this session's stream server binds to.
    pub port: u16,
}

impl PlannedRoute {
    /// The room that coordinates this route's Sonos group.
    ///
    /// For a single-room route this is simply the room itself.
    #[must_use]
    pub fn coordinator(&self) -> &RoomKey {
        // The planner rejects empty routes, so this cannot be reached.
        self.rooms
            .first()
            .expect("a planned route always has at least one room")
    }

    /// Whether this route has to form a Sonos group to stay in sync.
    ///
    /// One capture feeding several players independently drifts audibly, so
    /// anything past a single room is grouped rather than fanned out.
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.rooms.len() > 1
    }

    /// Every room UUID this route claims, coordinator first.
    #[must_use]
    pub fn room_uuids(&self) -> Vec<String> {
        self.rooms.iter().map(|room| room.uuid.clone()).collect()
    }

    /// A human-facing one-line description, for logs and `--dry-run`.
    #[must_use]
    pub fn label(&self) -> String {
        let rooms: Vec<&str> = self.rooms.iter().map(|room| room.name.as_str()).collect();
        format!("{} -> {}", self.source.label(), rooms.join(" + "))
    }
}

/// Resolves `routes` against the discovered `rooms` into a runnable plan.
///
/// Ports are allocated from `base_port` in route order, so a plan is
/// deterministic and a route keeps its port as long as the configuration above
/// it does not change.
///
/// # Errors
///
/// Returns a [`PlanError`] when there is nothing to do ([`PlanError::NoRoutes`],
/// [`PlanError::NoRooms`]), when a route names a room that cannot be resolved
/// ([`PlanError::UnknownRoom`], [`PlanError::AmbiguousRoom`]) or resolves to
/// none ([`PlanError::EmptyRoute`]), when two routes claim the same room
/// ([`PlanError::RoomConflict`]), or when the ports would overflow
/// ([`PlanError::PortOverflow`]).
pub fn plan(
    routes: &[Route],
    rooms: &[RoomKey],
    base_port: u16,
) -> Result<Vec<PlannedRoute>, PlanError> {
    if routes.is_empty() {
        return Err(PlanError::NoRoutes);
    }
    if rooms.is_empty() {
        return Err(PlanError::NoRooms);
    }

    let mut planned = Vec::with_capacity(routes.len());
    // Which route already owns a room, so a conflict names both of them.
    let mut claimed: HashMap<String, usize> = HashMap::new();

    for (index, route) in routes.iter().enumerate() {
        let resolved = resolve(index, &route.rooms, rooms)?;
        if resolved.is_empty() {
            return Err(PlanError::EmptyRoute { route: index });
        }

        for room in &resolved {
            if let Some(&first) = claimed.get(&room.uuid) {
                return Err(PlanError::RoomConflict {
                    room: room.name.clone(),
                    first,
                    second: index,
                });
            }
            claimed.insert(room.uuid.clone(), index);
        }

        planned.push(PlannedRoute {
            source: route.source.clone(),
            rooms: resolved,
            port: port_for(index, routes.len(), base_port)?,
        });
    }

    Ok(planned)
}

fn port_for(index: usize, count: usize, base_port: u16) -> Result<u16, PlanError> {
    u16::try_from(index)
        .ok()
        .and_then(|offset| base_port.checked_add(offset))
        .ok_or(PlanError::PortOverflow {
            count,
            base: base_port,
        })
}

fn resolve(
    index: usize,
    selector: &RoomSelector,
    rooms: &[RoomKey],
) -> Result<Vec<RoomKey>, PlanError> {
    match selector {
        RoomSelector::All => Ok(rooms.to_vec()),
        RoomSelector::Named(names) => {
            let mut resolved: Vec<RoomKey> = Vec::with_capacity(names.len());
            for name in names {
                let room = resolve_one(index, name, rooms)?;
                // Naming the same room twice is a typo, not an instruction to
                // stream to it twice; the second mention is simply redundant.
                if !resolved.iter().any(|seen| seen.uuid == room.uuid) {
                    resolved.push(room);
                }
            }
            Ok(resolved)
        }
    }
}

fn resolve_one(index: usize, needle: &str, rooms: &[RoomKey]) -> Result<RoomKey, PlanError> {
    // A UUID is unambiguous by construction, so it wins over any name match and
    // is the documented escape hatch for two rooms sharing a name.
    if let Some(room) = rooms.iter().find(|room| room.uuid == needle) {
        return Ok(room.clone());
    }

    let folded = needle.to_lowercase();
    let matches: Vec<&RoomKey> = rooms
        .iter()
        .filter(|room| room.name.to_lowercase() == folded)
        .collect();

    match matches.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err(PlanError::UnknownRoom {
            route: index,
            name: needle.to_owned(),
            known: rooms.iter().map(|room| room.name.clone()).collect(),
        }),
        many => Err(PlanError::AmbiguousRoom {
            route: index,
            name: needle.to_owned(),
            matches: many.iter().map(|room| room.uuid.clone()).collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{plan, PlannedRoute};
    use crate::{PlanError, RoomSelector, Route, Source};
    use sonos_vad_control::RoomKey;

    fn household() -> Vec<RoomKey> {
        vec![
            RoomKey::new("RINCON_BAD", "Bad"),
            RoomKey::new("RINCON_KUECHE", "Küche"),
            RoomKey::new("RINCON_WOHN", "Wohnzimmer"),
        ]
    }

    fn named(names: &[&str]) -> RoomSelector {
        RoomSelector::Named(names.iter().map(|n| (*n).to_string()).collect())
    }

    #[test]
    fn all_expands_to_every_room_in_discovery_order() {
        let routes = [Route::everything_everywhere()];
        let plan = plan(&routes, &household(), 8080).expect("valid plan");

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].rooms, household());
        assert_eq!(plan[0].port, 8080);
        // Discovery order is alphabetical, so the coordinator matches the
        // driver's All Sonos output and is stable across restarts.
        assert_eq!(plan[0].coordinator().name, "Bad");
        assert!(plan[0].is_group());
    }

    #[test]
    fn ports_are_allocated_in_route_order() {
        let routes = [
            Route::new(Source::Process("spotify.exe".into()), named(&["Küche"])),
            Route::new(Source::Default, named(&["Bad"])),
        ];
        let plan = plan(&routes, &household(), 9000).expect("valid plan");
        assert_eq!(plan[0].port, 9000);
        assert_eq!(plan[1].port, 9001);
    }

    #[test]
    fn concurrent_per_app_routes_to_different_rooms_are_the_point() {
        let routes = [
            Route::new(Source::Process("spotify.exe".into()), named(&["Küche"])),
            Route::new(Source::ExceptProcess("spotify.exe".into()), named(&["Bad"])),
        ];
        let plan = plan(&routes, &household(), 8080).expect("valid plan");

        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].coordinator().name, "Küche");
        assert_eq!(plan[1].coordinator().name, "Bad");
        assert!(!plan[0].is_group() && !plan[1].is_group());
    }

    #[test]
    fn named_rooms_keep_config_order_so_the_coordinator_is_chosen() {
        let routes = [Route::new(Source::Default, named(&["Wohnzimmer", "Bad"]))];
        let plan = plan(&routes, &household(), 8080).expect("valid plan");
        // Config order, not discovery order: listing a mains-powered player
        // first has to actually make it the coordinator.
        assert_eq!(plan[0].coordinator().name, "Wohnzimmer");
        assert_eq!(plan[0].room_uuids(), vec!["RINCON_WOHN", "RINCON_BAD"]);
    }

    #[test]
    fn two_routes_claiming_one_room_is_rejected_at_plan_time() {
        let routes = [
            Route::new(Source::Default, named(&["Küche"])),
            Route::new(Source::Process("spotify.exe".into()), named(&["Küche"])),
        ];
        let err = plan(&routes, &household(), 8080).expect_err("a conflict must not plan");
        // Failing here beats failing halfway through starting the second
        // session, with the first already streaming.
        assert!(matches!(
            err,
            PlanError::RoomConflict {
                first: 0,
                second: 1,
                ..
            }
        ));
    }

    #[test]
    fn all_conflicts_with_any_other_route() {
        let routes = [
            Route::everything_everywhere(),
            Route::new(Source::Process("spotify.exe".into()), named(&["Bad"])),
        ];
        let err = plan(&routes, &household(), 8080).expect_err("a conflict must not plan");
        assert!(matches!(err, PlanError::RoomConflict { .. }));
    }

    #[test]
    fn room_names_match_case_insensitively_including_non_ascii() {
        let routes = [Route::new(Source::Default, named(&["KÜCHE"]))];
        let plan = plan(&routes, &household(), 8080).expect("valid plan");
        assert_eq!(plan[0].coordinator().uuid, "RINCON_KUECHE");
    }

    #[test]
    fn a_uuid_resolves_directly() {
        let routes = [Route::new(Source::Default, named(&["RINCON_WOHN"]))];
        let plan = plan(&routes, &household(), 8080).expect("valid plan");
        assert_eq!(plan[0].coordinator().name, "Wohnzimmer");
    }

    #[test]
    fn an_unknown_room_reports_what_was_discovered() {
        let routes = [Route::new(Source::Default, named(&["Kitchen"]))];
        let err = plan(&routes, &household(), 8080).expect_err("unknown rooms must not plan");
        match err {
            PlanError::UnknownRoom { name, known, .. } => {
                assert_eq!(name, "Kitchen");
                assert_eq!(known, vec!["Bad", "Küche", "Wohnzimmer"]);
            }
            other => panic!("expected UnknownRoom, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_rooms_in_one_route_are_a_typo_not_a_conflict() {
        let routes = [Route::new(Source::Default, named(&["Bad", "Bad", "Küche"]))];
        let plan = plan(&routes, &household(), 8080).expect("valid plan");
        assert_eq!(plan[0].rooms.len(), 2);
    }

    #[test]
    fn two_rooms_sharing_a_name_demand_a_uuid() {
        let twins = vec![
            RoomKey::new("RINCON_A", "Office"),
            RoomKey::new("RINCON_B", "Office"),
        ];
        let routes = [Route::new(Source::Default, named(&["Office"]))];
        let err = plan(&routes, &twins, 8080).expect_err("an ambiguous name must not plan");
        match err {
            PlanError::AmbiguousRoom { matches, .. } => {
                assert_eq!(matches, vec!["RINCON_A", "RINCON_B"]);
            }
            other => panic!("expected AmbiguousRoom, got {other:?}"),
        }

        // Naming the UUID resolves it, which is what the error tells you to do.
        let routes = [Route::new(Source::Default, named(&["RINCON_B"]))];
        assert!(plan(&routes, &twins, 8080).is_ok());
    }

    #[test]
    fn nothing_to_do_is_an_error_rather_than_a_silent_success() {
        assert!(matches!(
            plan(&[], &household(), 8080),
            Err(PlanError::NoRoutes)
        ));
        assert!(matches!(
            plan(&[Route::everything_everywhere()], &[], 8080),
            Err(PlanError::NoRooms)
        ));
    }

    #[test]
    fn ports_running_off_the_end_are_caught() {
        let routes = [
            Route::new(Source::Default, named(&["Bad"])),
            Route::new(Source::Device("x".into()), named(&["Küche"])),
        ];
        let err = plan(&routes, &household(), u16::MAX).expect_err("overflow must not plan");
        assert!(matches!(err, PlanError::PortOverflow { count: 2, .. }));
    }

    #[test]
    fn labels_read_like_the_instruction_they_encode() {
        let route = PlannedRoute {
            source: Source::Process("spotify.exe".into()),
            rooms: vec![
                RoomKey::new("RINCON_A", "Bad"),
                RoomKey::new("R_B", "Küche"),
            ],
            port: 8080,
        };
        assert_eq!(route.label(), "App \"spotify.exe\" -> Bad + Küche");
    }
}
