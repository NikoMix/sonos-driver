//! The route model, and the TOML shape it is written in.
//!
//! A route is one sentence: *"send this audio to these rooms"*. Keeping the
//! configuration that literal is deliberate — it is the whole user interface of
//! the driverless mode, so it has to read like the thing it does.

use serde::{Deserialize, Serialize};

use crate::{PlanError, Source};

/// Which rooms a route sends to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomSelector {
    /// Every discovered room, in discovery order.
    ///
    /// Discovery order is alphabetical, so this picks the same coordinator as
    /// the driver's "All Sonos" output and stays stable across restarts.
    All,
    /// Specific rooms, by name or UUID, in the order written.
    ///
    /// The order matters: the first room coordinates the Sonos group, so
    /// listing a mains-powered player first is worth doing.
    Named(Vec<String>),
}

/// One resolved instruction: capture this source, play it in these rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The audio to capture.
    pub source: Source,
    /// Where to play it.
    pub rooms: RoomSelector,
}

impl Route {
    /// Builds a route from its parts.
    #[must_use]
    pub const fn new(source: Source, rooms: RoomSelector) -> Self {
        Self { source, rooms }
    }

    /// System audio to every room, the configuration most people want first.
    #[must_use]
    pub const fn everything_everywhere() -> Self {
        Self::new(Source::Default, RoomSelector::All)
    }
}

/// How `rooms` may be written in TOML: the word `all`, or a list of names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoomsField {
    /// A bare word, which must be `all`.
    Keyword(String),
    /// An explicit list of room names or UUIDs.
    List(Vec<String>),
}

impl Default for RoomsField {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

/// The TOML form of a [`Route`].
///
/// ```toml
/// [[route]]
/// rooms = "all"
///
/// [[route]]
/// process = "spotify.exe"
/// rooms = ["Küche", "Bad"]
/// ```
///
/// Exactly one source field may be set; omitting all of them means
/// [`Source::Default`], because "stream what I'm hearing" is the common case
/// and should not need a keyword.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct RouteConfig {
    /// Rooms to play in: `"all"`, or a list of names/UUIDs.
    #[serde(default)]
    pub rooms: RoomsField,
    /// Capture one named render endpoint instead of the default device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Capture only this application's audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Capture everything except this application's audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub except_process: Option<String>,
}

impl RouteConfig {
    /// Validates the configured route and resolves it to a [`Route`].
    ///
    /// `index` is only used to make errors point at the offending `[[route]]`.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::MultipleSources`] when more than one source field
    /// is set, [`PlanError::BadRoomsKeyword`] when `rooms` is a word other than
    /// `all`, and [`PlanError::EmptyRoute`] when the room list is empty.
    pub fn to_route(&self, index: usize) -> Result<Route, PlanError> {
        Ok(Route::new(self.source(index)?, self.selector(index)?))
    }

    fn source(&self, index: usize) -> Result<Source, PlanError> {
        let candidates = [
            ("device", self.device.as_ref()),
            ("process", self.process.as_ref()),
            ("except-process", self.except_process.as_ref()),
        ];
        let set: Vec<(&str, &String)> = candidates
            .into_iter()
            .filter_map(|(field, value)| value.map(|v| (field, v)))
            .collect();

        match set.as_slice() {
            [] => Ok(Source::Default),
            [("device", value)] => Ok(Source::Device((*value).clone())),
            [("process", value)] => Ok(Source::Process((*value).clone())),
            [("except-process", value)] => Ok(Source::ExceptProcess((*value).clone())),
            _ => Err(PlanError::MultipleSources {
                route: index,
                fields: set.iter().map(|(field, _)| (*field).to_owned()).collect(),
            }),
        }
    }

    fn selector(&self, index: usize) -> Result<RoomSelector, PlanError> {
        match &self.rooms {
            RoomsField::Keyword(word) if word.eq_ignore_ascii_case("all") => Ok(RoomSelector::All),
            RoomsField::Keyword(word) => Err(PlanError::BadRoomsKeyword {
                route: index,
                value: word.clone(),
            }),
            RoomsField::List(names) if names.is_empty() => {
                Err(PlanError::EmptyRoute { route: index })
            }
            RoomsField::List(names) => Ok(RoomSelector::Named(names.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RoomSelector, RoomsField, RouteConfig};
    use crate::{PlanError, Source};

    fn config(toml: &str) -> RouteConfig {
        toml::from_str(toml).expect("valid TOML")
    }

    #[test]
    fn an_empty_route_means_system_audio() {
        // Omitting the source entirely is the common case and must not need a
        // keyword; only `rooms` is mandatory.
        let route = config("rooms = \"all\"").to_route(0).expect("valid route");
        assert_eq!(route.source, Source::Default);
        assert_eq!(route.rooms, RoomSelector::All);
    }

    #[test]
    fn each_source_field_maps_to_its_variant() {
        let device = config("rooms = [\"Bad\"]\ndevice = \"Realtek\"")
            .to_route(0)
            .expect("valid route");
        assert_eq!(device.source, Source::Device("Realtek".into()));

        let process = config("rooms = [\"Bad\"]\nprocess = \"spotify.exe\"")
            .to_route(0)
            .expect("valid route");
        assert_eq!(process.source, Source::Process("spotify.exe".into()));

        let except = config("rooms = [\"Bad\"]\nexcept-process = \"teams.exe\"")
            .to_route(0)
            .expect("valid route");
        assert_eq!(except.source, Source::ExceptProcess("teams.exe".into()));
    }

    #[test]
    fn naming_two_sources_is_rejected_with_both_named() {
        let err = config("rooms = [\"Bad\"]\nprocess = \"a.exe\"\ndevice = \"Realtek\"")
            .to_route(3)
            .expect_err("two sources must not plan");
        match err {
            PlanError::MultipleSources { route, fields } => {
                assert_eq!(route, 3);
                assert_eq!(fields, vec!["device", "process"]);
            }
            other => panic!("expected MultipleSources, got {other:?}"),
        }
    }

    #[test]
    fn rooms_accepts_only_the_word_all() {
        let err = config("rooms = \"everything\"")
            .to_route(1)
            .expect_err("a bare word other than all must not plan");
        assert!(matches!(err, PlanError::BadRoomsKeyword { route: 1, .. }));

        // Case is not worth being strict about.
        assert_eq!(
            config("rooms = \"ALL\"").to_route(0).expect("valid").rooms,
            RoomSelector::All
        );
    }

    #[test]
    fn an_empty_room_list_is_an_error_not_a_silent_no_op() {
        let err = config("rooms = []")
            .to_route(2)
            .expect_err("an empty list must not plan");
        assert!(matches!(err, PlanError::EmptyRoute { route: 2 }));
    }

    #[test]
    fn a_misspelled_field_is_rejected_rather_than_ignored() {
        // deny_unknown_fields matters here: silently ignoring `proces` would
        // route the whole system's audio somewhere the user did not intend.
        let err = toml::from_str::<RouteConfig>("rooms = [\"Bad\"]\nproces = \"spotify.exe\"")
            .expect_err("unknown fields must not deserialize");
        assert!(err.to_string().contains("proces"), "{err}");
    }

    #[test]
    fn named_rooms_keep_the_order_they_were_written_in() {
        let route = config("rooms = [\"Bad\", \"Küche\"]")
            .to_route(0)
            .expect("valid route");
        assert_eq!(
            route.rooms,
            RoomSelector::Named(vec!["Bad".into(), "Küche".into()])
        );
    }

    #[test]
    fn the_default_rooms_field_is_an_empty_list() {
        assert_eq!(RoomsField::default(), RoomsField::List(Vec::new()));
    }
}
