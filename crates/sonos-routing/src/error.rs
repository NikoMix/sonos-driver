//! Errors the route planner can report.
//!
//! Every variant carries enough context to fix the configuration without
//! re-reading the docs: which route failed, and — for an unresolvable room —
//! what the household actually offered.

use std::fmt;

/// Why a set of routes could not be turned into a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// No routes were configured, so there is nothing to stream.
    NoRoutes,
    /// Discovery produced no rooms to route to.
    NoRooms,
    /// A route named a room that does not exist in this household.
    UnknownRoom {
        /// Zero-based index of the offending route.
        route: usize,
        /// The name or UUID as written in the configuration.
        name: String,
        /// Every room that *was* discovered, so the fix is obvious.
        known: Vec<String>,
    },
    /// A route named a room that matches more than one discovered room.
    AmbiguousRoom {
        /// Zero-based index of the offending route.
        route: usize,
        /// The ambiguous name.
        name: String,
        /// The UUIDs it matched; naming one of these resolves the ambiguity.
        matches: Vec<String>,
    },
    /// A route resolved to no rooms at all.
    EmptyRoute {
        /// Zero-based index of the offending route.
        route: usize,
    },
    /// Two routes claim the same room.
    RoomConflict {
        /// The contested room's name.
        room: String,
        /// Zero-based index of the route that claimed it first.
        first: usize,
        /// Zero-based index of the route that claimed it again.
        second: usize,
    },
    /// The planned ports would run past the end of the port range.
    PortOverflow {
        /// How many routes were being planned.
        count: usize,
        /// The configured base port.
        base: u16,
    },
    /// A route named more than one audio source.
    MultipleSources {
        /// Zero-based index of the offending route.
        route: usize,
        /// The source fields that were set.
        fields: Vec<String>,
    },
    /// A route's `rooms` was a word other than `all`.
    BadRoomsKeyword {
        /// Zero-based index of the offending route.
        route: usize,
        /// What was written instead.
        value: String,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoutes => f.write_str(
                "no routes are configured; add at least one [[route]] with a source and rooms",
            ),
            Self::NoRooms => f.write_str("no Sonos rooms were discovered to route audio to"),
            Self::UnknownRoom { route, name, known } => write!(
                f,
                "route {route} names room \"{name}\", which is not in this household; discovered: {}",
                known.join(", ")
            ),
            Self::AmbiguousRoom {
                route,
                name,
                matches,
            } => write!(
                f,
                "route {route} names room \"{name}\", which matches {} rooms; name one of these UUIDs instead: {}",
                matches.len(),
                matches.join(", ")
            ),
            Self::EmptyRoute { route } => write!(
                f,
                "route {route} resolved to no rooms; give it a room list or \"all\""
            ),
            Self::RoomConflict {
                room,
                first,
                second,
            } => write!(
                f,
                "routes {first} and {second} both claim \"{room}\"; a room can only play one stream, so give each route its own rooms"
            ),
            Self::PortOverflow { count, base } => write!(
                f,
                "{count} routes starting at port {base} exceeds the port range; lower stream.port"
            ),
            Self::MultipleSources { route, fields } => write!(
                f,
                "route {route} names {} sources ({}); a route captures exactly one",
                fields.len(),
                fields.join(", ")
            ),
            Self::BadRoomsKeyword { route, value } => write!(
                f,
                "route {route} has rooms = \"{value}\"; expected \"all\" or a list of room names"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use super::PlanError;

    #[test]
    fn unknown_room_lists_what_was_actually_found() {
        let err = PlanError::UnknownRoom {
            route: 0,
            name: "Kitchen".into(),
            known: vec!["Küche".into(), "Bad".into()],
        };
        let text = err.to_string();
        // The whole point of carrying `known` is that the fix is in the message.
        assert!(text.contains("Kitchen"), "{text}");
        assert!(text.contains("Küche, Bad"), "{text}");
    }

    #[test]
    fn conflict_names_both_routes() {
        let text = PlanError::RoomConflict {
            room: "Bad".into(),
            first: 0,
            second: 2,
        }
        .to_string();
        assert!(text.contains('0') && text.contains('2'), "{text}");
    }
}
