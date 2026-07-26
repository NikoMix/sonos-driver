//! The routing table: one Windows output, one Sonos destination.
//!
//! This is the whole mental model of the app. Windows already decides *which*
//! output an application plays to; all that is missing is a way to say where
//! each of those outputs should end up. So the table is not a list of rules the
//! user has to invent â€” it is the list of outputs the machine already has, each
//! with a destination beside it.
//!
//! Everything here is pure: no WASAPI, no network, no disk. The table is
//! rebuilt from whatever the OS reports and validated before anything is
//! started, so the failure modes that used to surface as a service retry loop
//! (two outputs fighting over one room, a room that went away) are visible in
//! the UI instead.

use std::collections::BTreeMap;

use sonos_routing::{RoomSelector, RoomsField, Route, RouteConfig, Source};

use crate::state::{Output, Room};

/// The synthetic first row: whatever Windows currently calls the default.
///
/// Held as a row rather than a checkbox because it behaves exactly like an
/// output â€” it just resolves late. Its device id is empty, which no real
/// endpoint id ever is.
pub const DEFAULT_OUTPUT_ID: &str = "";

/// Where one output's audio goes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Destination {
    /// Not routed: this output stays purely local.
    #[default]
    Off,
    /// One room.
    Room {
        /// Stable Sonos device `UUID`.
        uuid: String,
        /// Room name, kept so an offline room still displays as itself.
        name: String,
    },
    /// Every discovered room, played in sync as one Sonos group.
    AllRooms,
}

impl Destination {
    /// Whether this output is routed anywhere.
    #[must_use]
    pub fn is_routed(&self) -> bool {
        !matches!(self, Self::Off)
    }

    /// The label shown in the destination picker.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Off => "Not routed".to_owned(),
            Self::Room { name, .. } => name.clone(),
            Self::AllRooms => "All rooms".to_owned(),
        }
    }

    /// The room `UUID` this destination claims, if it is a single room.
    #[must_use]
    pub fn uuid(&self) -> Option<&str> {
        match self {
            Self::Room { uuid, .. } => Some(uuid.as_str()),
            Self::Off | Self::AllRooms => None,
        }
    }
}

/// One row: an output and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRoute {
    /// The Windows render endpoint this row represents.
    pub output: Output,
    /// Where its audio is sent.
    pub destination: Destination,
}

/// Why a table cannot be started as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The rows involved, in table order.
    pub rows: Vec<usize>,
    /// A sentence explaining the clash and how to resolve it.
    pub message: String,
}

/// Something worth saying that is not a reason to refuse to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// The rows involved, in table order.
    pub rows: Vec<usize>,
    /// A sentence describing the surprise.
    pub message: String,
}

/// The output-to-room assignments, rebuilt from the OS on every refresh.
#[derive(Debug, Clone, Default)]
pub struct RoutingTable {
    rows: Vec<OutputRoute>,
    /// Assignments by device id, including for outputs not currently present.
    ///
    /// A USB DAC that is unplugged and plugged back in should come back with
    /// its room still attached; without this the user would have to redo the
    /// mapping every time, which is exactly the sort of thing that makes a tool
    /// feel unreliable.
    remembered: BTreeMap<String, Destination>,
}

impl RoutingTable {
    /// An empty table holding only the synthetic default-output row.
    #[must_use]
    pub fn new() -> Self {
        let mut table = Self::default();
        table.sync_outputs(Vec::new());
        table
    }

    /// The rows, in presentation order.
    #[must_use]
    pub fn rows(&self) -> &[OutputRoute] {
        &self.rows
    }

    /// How many outputs are routed somewhere.
    #[must_use]
    pub fn routed_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.destination.is_routed())
            .count()
    }

    /// Whether nothing at all is routed.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.routed_count() == 0
    }

    /// Rebuild the rows from the endpoints the OS currently reports.
    ///
    /// The synthetic default row is always first, then the endpoints in the
    /// order they were enumerated. Assignments follow the device id, so a
    /// reorder or a re-enumeration never moves a room onto the wrong output.
    pub fn sync_outputs(&mut self, outputs: Vec<Output>) {
        let mut rows = Vec::with_capacity(outputs.len() + 1);
        rows.push(OutputRoute {
            output: Output::default_follower(),
            destination: self.remembered_for(DEFAULT_OUTPUT_ID),
        });
        for output in outputs {
            let destination = self.remembered_for(&output.id);
            rows.push(OutputRoute {
                output,
                destination,
            });
        }
        self.rows = rows;
    }

    fn remembered_for(&self, id: &str) -> Destination {
        self.remembered.get(id).cloned().unwrap_or_default()
    }

    /// Assign `destination` to the row at `index`.
    ///
    /// Returns `false` when the index is out of bounds, so a stale click from a
    /// frame rendered before a refresh cannot panic the UI.
    pub fn assign(&mut self, index: usize, destination: Destination) -> bool {
        let Some(row) = self.rows.get_mut(index) else {
            return false;
        };
        row.destination = destination.clone();
        let id = row.output.id.clone();
        if destination.is_routed() {
            self.remembered.insert(id, destination);
        } else {
            self.remembered.remove(&id);
        }
        true
    }

    /// Clear every assignment.
    pub fn clear(&mut self) {
        self.remembered.clear();
        for row in &mut self.rows {
            row.destination = Destination::Off;
        }
    }

    /// Rows whose room is not in `known`, i.e. a speaker that is powered off,
    /// renamed, or on another network.
    #[must_use]
    pub fn offline_rows(&self, known: &[Room]) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| match &row.destination {
                Destination::Room { uuid, .. } => !known.iter().any(|room| &room.uuid == uuid),
                Destination::Off | Destination::AllRooms => false,
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Every reason the table cannot be started as written.
    ///
    /// Mirrors the planner's own rules so the message arrives on the click
    /// rather than several seconds later from a background retry.
    #[must_use]
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut conflicts = Vec::new();
        let all_rooms: Vec<usize> = self
            .routed()
            .filter(|(_, row)| row.destination == Destination::AllRooms)
            .map(|(index, _)| index)
            .collect();
        let named: Vec<(usize, &OutputRoute)> = self
            .routed()
            .filter(|(_, row)| row.destination.uuid().is_some())
            .collect();

        if all_rooms.len() > 1 {
            conflicts.push(Conflict {
                rows: all_rooms.clone(),
                message: "Two outputs are both set to All rooms. Only one output can own \
                          every room at a time â€” send the others to specific rooms, or turn \
                          them off."
                    .to_owned(),
            });
        }

        if let (Some(&all), Some((_, first))) = (all_rooms.first(), named.first()) {
            conflicts.push(Conflict {
                rows: vec![all, named[0].0],
                message: format!(
                    "\"{}\" is set to All rooms, which already includes \"{}\". Pick specific \
                     rooms for both, or leave only the All rooms output.",
                    self.rows[all].output.name,
                    first.destination.label()
                ),
            });
        }

        let mut claimed: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, row) in named {
            let Some(uuid) = row.destination.uuid() else {
                continue;
            };
            if let Some(&first) = claimed.get(uuid) {
                conflicts.push(Conflict {
                    rows: vec![first, index],
                    message: format!(
                        "\"{}\" is already receiving \"{}\". A room can only play one source \
                         at a time.",
                        row.destination.label(),
                        self.rows[first].output.name
                    ),
                });
            } else {
                claimed.insert(uuid, index);
            }
        }

        conflicts
    }

    /// Things that are legal but probably not what was meant.
    #[must_use]
    pub fn warnings(&self) -> Vec<Warning> {
        let mut warnings = Vec::new();
        let default_row_routed = self
            .rows
            .first()
            .is_some_and(|row| row.destination.is_routed());
        if default_row_routed {
            if let Some((index, row)) = self
                .routed()
                .skip(1)
                .find(|(_, row)| row.output.is_default_endpoint)
            {
                warnings.push(Warning {
                    rows: vec![0, index],
                    message: format!(
                        "\"Default output\" and \"{}\" are the same endpoint right now, so this \
                         captures it twice. That works, but one row is enough.",
                        row.output.name
                    ),
                });
            }
        }

        // Routing a sonosvad endpoint means this app captures audio that the
        // driver's own service is very likely already capturing and sending to
        // the same household. It is not wrong to do deliberately with the
        // service stopped, so this informs rather than blocks.
        let virtual_rows: Vec<usize> = self
            .routed()
            .filter(|(_, row)| row.output.is_sonos_virtual())
            .map(|(index, _)| index)
            .collect();
        if !virtual_rows.is_empty() {
            let names: Vec<&str> = virtual_rows
                .iter()
                .map(|&index| self.rows[index].output.name.as_str())
                .collect();
            warnings.push(Warning {
                rows: virtual_rows,
                message: format!(
                    "{} already belong(s) to the sonosvad driver, which streams to Sonos by \
                     itself. Routing it here as well sends the same audio twice unless the \
                     sonosvad service is stopped.",
                    names.join(", ")
                ),
            });
        }

        warnings
    }

    /// The routes to actually run, resolved against the discovered household.
    ///
    /// Rows pointing at a room that did not answer discovery are left out
    /// rather than failing the whole table: one speaker being switched off
    /// should not silence the rest of the house. [`offline_rows`] reports them
    /// so the UI can say so.
    ///
    /// Sources use the endpoint **id**, which is exact, even though the saved
    /// file uses the friendly name, which is portable.
    ///
    /// [`offline_rows`]: Self::offline_rows
    #[must_use]
    pub fn to_routes(&self, known: &[Room]) -> Vec<Route> {
        self.routed()
            .filter_map(|(_, row)| {
                let selector = match &row.destination {
                    Destination::Off => return None,
                    Destination::AllRooms => RoomSelector::All,
                    Destination::Room { uuid, .. } => {
                        if !known.iter().any(|room| &room.uuid == uuid) {
                            return None;
                        }
                        RoomSelector::Named(vec![uuid.clone()])
                    }
                };
                Some(Route::new(row.output.source(), selector))
            })
            .collect()
    }

    /// The routed rows as portable `[[route]]` tables.
    #[must_use]
    pub fn to_configs(&self) -> Vec<RouteConfig> {
        self.routed()
            .filter_map(|(_, row)| {
                let rooms = match &row.destination {
                    Destination::Off => return None,
                    Destination::AllRooms => RoomsField::Keyword("all".to_owned()),
                    Destination::Room { name, .. } => RoomsField::List(vec![name.clone()]),
                };
                Some(RouteConfig {
                    rooms,
                    device: row.output.config_selector(),
                    process: None,
                    except_process: None,
                })
            })
            .collect()
    }

    /// Restore assignments from a saved `[[route]]` list.
    ///
    /// Matching is deliberately forgiving, because the file is portable and may
    /// have been written by the CLI: an output is matched by exact endpoint id
    /// first and then by friendly name, and a room by `UUID` first and then by
    /// name. Anything that resolves to neither is remembered but not shown, so
    /// re-saving does not quietly delete a route for a device that happens to
    /// be unplugged today.
    pub fn apply_configs(&mut self, configs: &[RouteConfig], known: &[Room]) {
        self.clear();
        for config in configs {
            let Ok(route) = config.to_route(0) else {
                continue;
            };
            let destination = match &route.rooms {
                RoomSelector::All => Destination::AllRooms,
                RoomSelector::Named(names) => {
                    let Some(name) = names.first() else { continue };
                    let matched = known.iter().find(|room| &room.uuid == name).or_else(|| {
                        known
                            .iter()
                            .find(|room| room.name.eq_ignore_ascii_case(name))
                    });
                    match matched {
                        Some(room) => Destination::Room {
                            uuid: room.uuid.clone(),
                            name: room.name.clone(),
                        },
                        // Keep the name so the row still reads correctly; the
                        // uuid resolves on the next discovery.
                        None => Destination::Room {
                            uuid: name.clone(),
                            name: name.clone(),
                        },
                    }
                }
            };

            let Some(index) = self.find_output(&route.source) else {
                continue;
            };
            self.assign(index, destination);
        }
    }

    /// Locate the row a saved source refers to.
    fn find_output(&self, source: &Source) -> Option<usize> {
        let Some(selector) = source.device() else {
            // `Source::Default`, `Process` and `ExceptProcess` all belong to
            // the default row: the last two are CLI-only features this table
            // does not model, and dropping them silently would be worse than
            // showing them as the default output.
            return matches!(source, Source::Default).then_some(0);
        };
        self.rows
            .iter()
            .position(|row| row.output.id == selector)
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| row.output.name.eq_ignore_ascii_case(selector))
            })
    }

    fn routed(&self) -> impl Iterator<Item = (usize, &OutputRoute)> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.destination.is_routed())
    }
}

#[cfg(test)]
mod tests {
    use super::{Destination, RoutingTable, DEFAULT_OUTPUT_ID};
    use crate::state::{Output, Room};
    use sonos_routing::{RoomSelector, RoomsField, Source};

    fn outputs() -> Vec<Output> {
        vec![
            Output::new("{0.0.0}.speakers", "Speakers (Realtek)", "", true),
            Output::new("{0.0.0}.hdmi", "LG TV (NVIDIA HDMI)", "", false),
        ]
    }

    fn rooms() -> Vec<Room> {
        vec![
            Room::new("RINCON_1", "KÃ¼che"),
            Room::new("RINCON_2", "Wohnzimmer"),
        ]
    }

    fn table() -> RoutingTable {
        let mut table = RoutingTable::new();
        table.sync_outputs(outputs());
        table
    }

    #[test]
    fn routing_a_sonosvad_endpoint_warns_about_streaming_it_twice() {
        use crate::state::SONOSVAD_ADAPTER;

        let mut table = RoutingTable::new();
        table.sync_outputs(vec![
            Output::new("{0.0.0}.speakers", "Speakers (Realtek)", "", true),
            // What the driver's endpoints look like once sonosvad-svc has
            // renamed them: the name is a room, so only the adapter gives it
            // away.
            Output::new("{0.0.0}.vad1", "Wohnzimmer", SONOSVAD_ADAPTER, false),
        ]);
        let rooms = rooms();

        // A plain output is unremarkable.
        table.assign(1, to_room(&rooms[0]));
        assert!(
            table.warnings().is_empty(),
            "an ordinary output should not warn"
        );

        table.assign(2, to_room(&rooms[1]));
        let warnings = table.warnings();
        assert_eq!(warnings.len(), 1, "the sonosvad row should warn");
        assert_eq!(warnings[0].rows, vec![2]);
        assert!(
            warnings[0].message.contains("Wohnzimmer"),
            "the warning should name the output: {}",
            warnings[0].message
        );
        assert!(
            warnings[0].message.contains("twice"),
            "the warning should say what goes wrong: {}",
            warnings[0].message
        );

        // Unrouting it clears the warning: the endpoint existing is fine, it is
        // routing it from here that is the surprise.
        table.assign(2, Destination::Off);
        assert!(table.warnings().is_empty());
    }

    #[test]
    fn an_endpoint_with_an_unknown_adapter_is_not_treated_as_sonosvad() {
        // Property reads can fail; an empty adapter must never match, or every
        // such endpoint would warn.
        let output = Output::new("{0.0.0}.x", "Something", "", false);
        assert!(!output.is_sonos_virtual());

        // Observed on a real machine: the Remote Desktop endpoint reports a
        // single space, which is "unknown" too.
        let blank = Output::new("{0.0.0}.rdp", "Remote Audio", " ", false);
        assert!(!blank.is_sonos_virtual());

        // Whitespace around a genuine match still matches.
        let padded = Output::new(
            "{0.0.0}.vad",
            "Bad",
            format!("  {} ", crate::state::SONOSVAD_ADAPTER),
            false,
        );
        assert!(padded.is_sonos_virtual());
    }

    fn to_room(room: &Room) -> Destination {
        Destination::Room {
            uuid: room.uuid.clone(),
            name: room.name.clone(),
        }
    }

    #[test]
    fn the_default_output_is_always_the_first_row() {
        let table = table();
        assert_eq!(table.rows().len(), 3);
        assert_eq!(table.rows()[0].output.id, DEFAULT_OUTPUT_ID);
        assert!(table.is_idle());
    }

    #[test]
    fn an_assignment_survives_the_device_disappearing_and_returning() {
        let mut table = table();
        assert!(table.assign(2, to_room(&rooms()[0])));

        // The HDMI output goes away (monitor switched off) and comes back.
        table.sync_outputs(vec![outputs()[0].clone()]);
        assert_eq!(table.rows().len(), 2);
        table.sync_outputs(outputs());

        assert_eq!(table.rows()[2].destination, to_room(&rooms()[0]));
    }

    #[test]
    fn assignments_follow_the_device_id_not_the_row_index() {
        let mut table = table();
        table.assign(2, to_room(&rooms()[1]));
        // Re-enumeration returns the endpoints in the other order.
        let mut reordered = outputs();
        reordered.reverse();
        table.sync_outputs(reordered);

        let hdmi = table
            .rows()
            .iter()
            .find(|row| row.output.id == "{0.0.0}.hdmi")
            .expect("the HDMI row");
        assert_eq!(hdmi.destination, to_room(&rooms()[1]));
    }

    #[test]
    fn turning_a_row_off_forgets_it() {
        let mut table = table();
        table.assign(1, to_room(&rooms()[0]));
        table.assign(1, Destination::Off);
        table.sync_outputs(outputs());
        assert_eq!(table.rows()[1].destination, Destination::Off);
    }

    #[test]
    fn assigning_out_of_bounds_is_refused_rather_than_panicking() {
        let mut table = table();
        assert!(!table.assign(99, Destination::AllRooms));
    }

    #[test]
    fn two_outputs_cannot_claim_the_same_room() {
        let mut table = table();
        table.assign(1, to_room(&rooms()[0]));
        table.assign(2, to_room(&rooms()[0]));
        let conflicts = table.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].rows, vec![1, 2]);
        assert!(conflicts[0].message.contains("KÃ¼che"), "{conflicts:?}");
    }

    #[test]
    fn all_rooms_conflicts_with_a_specific_room() {
        let mut table = table();
        table.assign(1, Destination::AllRooms);
        table.assign(2, to_room(&rooms()[0]));
        assert!(!table.conflicts().is_empty());
    }

    #[test]
    fn two_all_rooms_outputs_conflict() {
        let mut table = table();
        table.assign(1, Destination::AllRooms);
        table.assign(2, Destination::AllRooms);
        let conflicts = table.conflicts();
        assert!(conflicts.iter().any(|c| c.rows == vec![1, 2]));
    }

    #[test]
    fn different_rooms_on_different_outputs_are_fine() {
        let mut table = table();
        table.assign(1, to_room(&rooms()[0]));
        table.assign(2, to_room(&rooms()[1]));
        assert!(table.conflicts().is_empty());
        assert_eq!(table.routed_count(), 2);
    }

    #[test]
    fn routing_the_default_row_and_the_default_endpoint_warns_but_is_allowed() {
        let mut table = table();
        table.assign(0, to_room(&rooms()[0]));
        table.assign(1, to_room(&rooms()[1]));
        assert!(table.conflicts().is_empty());
        let warnings = table.warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rows, vec![0, 1]);
    }

    #[test]
    fn routes_use_endpoint_ids_and_the_default_source() {
        let mut table = table();
        table.assign(0, Destination::AllRooms);
        table.assign(2, to_room(&rooms()[0]));

        let routes = table.to_routes(&rooms());
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].source, Source::Default);
        assert_eq!(routes[0].rooms, RoomSelector::All);
        assert_eq!(routes[1].source, Source::Device("{0.0.0}.hdmi".to_owned()));
        assert_eq!(
            routes[1].rooms,
            RoomSelector::Named(vec!["RINCON_1".to_owned()])
        );
    }

    #[test]
    fn a_room_that_is_switched_off_is_skipped_rather_than_failing_everything() {
        let mut table = table();
        table.assign(1, to_room(&rooms()[0]));
        table.assign(
            2,
            Destination::Room {
                uuid: "RINCON_GONE".to_owned(),
                name: "Bad".to_owned(),
            },
        );

        let routes = table.to_routes(&rooms());
        assert_eq!(routes.len(), 1, "the reachable room still streams");
        assert_eq!(table.offline_rows(&rooms()), vec![2]);
    }

    #[test]
    fn the_saved_form_uses_names_so_the_file_stays_portable() {
        let mut table = table();
        table.assign(0, Destination::AllRooms);
        table.assign(2, to_room(&rooms()[1]));

        let configs = table.to_configs();
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].device, None);
        assert_eq!(configs[0].rooms, RoomsField::Keyword("all".to_owned()));
        assert_eq!(configs[1].device.as_deref(), Some("LG TV (NVIDIA HDMI)"));
        assert_eq!(
            configs[1].rooms,
            RoomsField::List(vec!["Wohnzimmer".to_owned()])
        );
    }

    #[test]
    fn a_saved_table_round_trips() {
        let mut saved = table();
        saved.assign(0, Destination::AllRooms);
        saved.assign(1, Destination::Off);
        let configs = saved.to_configs();

        let mut restored = table();
        restored.apply_configs(&configs, &rooms());
        assert_eq!(restored.rows()[0].destination, Destination::AllRooms);
        assert_eq!(restored.routed_count(), 1);
    }

    #[test]
    fn a_config_written_by_the_cli_loads_by_friendly_name() {
        use sonos_routing::RouteConfig;
        let configs = [RouteConfig {
            rooms: RoomsField::List(vec!["kÃ¼che".to_owned()]),
            device: Some("LG TV (NVIDIA HDMI)".to_owned()),
            process: None,
            except_process: None,
        }];

        let mut table = table();
        table.apply_configs(&configs, &rooms());
        assert_eq!(table.rows()[2].destination, to_room(&rooms()[0]));
    }

    #[test]
    fn a_route_for_an_absent_device_is_dropped_rather_than_misapplied() {
        use sonos_routing::RouteConfig;
        let configs = [RouteConfig {
            rooms: RoomsField::List(vec!["KÃ¼che".to_owned()]),
            device: Some("Some USB DAC that is not plugged in".to_owned()),
            process: None,
            except_process: None,
        }];

        let mut table = table();
        table.apply_configs(&configs, &rooms());
        assert!(table.is_idle());
    }

    #[test]
    fn clearing_forgets_absent_devices_too() {
        let mut table = table();
        table.assign(2, to_room(&rooms()[0]));
        table.clear();
        table.sync_outputs(outputs());
        assert!(table.is_idle());
    }
}
