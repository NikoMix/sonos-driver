//! Loading the driverless host's configuration.
//!
//! The file is a normal `sonos-driver` config with `[[route]]` tables added, so
//! every existing setting — codec profile, jitter buffer, idle release — means
//! the same thing here as it does on the driver path.

use std::path::{Path, PathBuf};

use anyhow::Context;
use audio_core::Config;
use serde::Deserialize;
use sonos_routing::{Route, RouteConfig};

/// The default configuration a first run writes about.
///
/// System audio to every room is the configuration almost everyone wants
/// first, and it is the one that needs no knowledge of the room names.
pub const EXAMPLE: &str = r#"# Every route says: capture this, play it in these rooms.
#
# Sources (pick at most one per route; omitting them all means system audio):
#   device         = "Realtek"      # one named render endpoint
#   process        = "spotify.exe"  # only that app's audio
#   except-process = "teams.exe"    # everything but that app
#
# Rooms: "all", or a list of room names or UUIDs. The first room in a list
# coordinates the Sonos group, so put a mains-powered player first.

# Uncomment on networks where SSDP multicast does not work (NAT'd VMs,
# VLAN-segmented or guest wifi, APs without working IGMP). Any one player
# reports the whole household.
#
# [sonos]
# seed-host = "192.168.1.50"

[[route]]
rooms = "all"

# Per-application routing needs no driver at all:
#
# [[route]]
# process = "spotify.exe"
# rooms = ["Küche"]
#
# [[route]]
# except-process = "spotify.exe"
# rooms = ["Wohnzimmer", "Bad"]
"#;

/// The `[[route]]` tables, picked out of the same document as [`Config`].
///
/// Parsed separately rather than with `#[serde(flatten)]`: neither struct
/// rejects unknown fields, so each can read the same text and take only what it
/// understands, which avoids serde's flatten/TOML interaction entirely.
#[derive(Debug, Default, Deserialize)]
struct RoutesDocument {
    #[serde(default)]
    route: Vec<RouteConfig>,
}

/// The machine-wide configuration path for the driverless host.
///
/// Deliberately **not** the driver service's `config.toml`: both hosts can be
/// installed at once, and sharing a file would make one's routes look like
/// stray keys to the other.
#[must_use]
pub fn default_config_path() -> PathBuf {
    program_data().join("cast.toml")
}

/// The machine-wide log directory.
#[must_use]
pub fn log_dir() -> PathBuf {
    program_data().join("logs")
}

fn program_data() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    base.join("sonos-driver")
}

/// Read `path` into audio settings and routes.
///
/// A missing file yields defaults plus no routes, which the worker reports as a
/// configuration problem rather than silently streaming nothing.
///
/// # Errors
///
/// Fails when the file cannot be read, is not valid TOML, or contains a route
/// that does not describe a single source and a non-empty room list.
pub fn load(path: Option<&Path>) -> anyhow::Result<(Config, Vec<Route>)> {
    let Some(path) = path else {
        return Ok((Config::default(), Vec::new()));
    };
    if !path.exists() {
        tracing::debug!(path = %path.display(), "no config file");
        return Ok((Config::default(), Vec::new()));
    }

    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Parse a configuration document into audio settings and routes.
fn parse(text: &str) -> anyhow::Result<(Config, Vec<Route>)> {
    let audio: Config = toml::from_str(text)?;
    let document: RoutesDocument = toml::from_str(text)?;
    let routes = document
        .route
        .iter()
        .enumerate()
        .map(|(index, route)| route.to_route(index))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((audio, routes))
}

/// Write [`EXAMPLE`] to `path` unless something is already there.
///
/// Called by `install` so the service has something to run and the user has
/// something to edit, rather than a service that starts and immediately
/// complains about having no routes.
///
/// # Errors
///
/// Fails when the directory or file cannot be created.
pub fn write_example_if_absent(path: &Path) -> anyhow::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, EXAMPLE).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{parse, EXAMPLE};
    use sonos_routing::{RoomSelector, Source};

    #[test]
    fn the_shipped_example_is_valid() {
        // It is written to disk by `install`, so a typo here would break the
        // service for every first-time user.
        let (_, routes) = parse(EXAMPLE).expect("the example must parse");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].source, Source::Default);
        assert_eq!(routes[0].rooms, RoomSelector::All);
    }

    #[test]
    fn audio_settings_and_routes_come_from_one_document() {
        let (config, routes) = parse(
            r#"
[stream]
port = 9100

[sonos]
seed-host = "192.168.1.50"

[[route]]
process = "spotify.exe"
rooms = ["Küche"]

[[route]]
except-process = "spotify.exe"
rooms = "all"
"#,
        )
        .expect("valid document");

        assert_eq!(config.stream.port, 9100);
        assert_eq!(config.sonos.seed_host.as_deref(), Some("192.168.1.50"));
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].source, Source::Process("spotify.exe".into()));
        assert_eq!(
            routes[1].source,
            Source::ExceptProcess("spotify.exe".into())
        );
    }

    #[test]
    fn a_document_with_no_routes_parses_to_none() {
        // Not an error here; the worker reports it, with a path to fix it.
        let (_, routes) = parse("[stream]\nport = 8080\n").expect("valid document");
        assert!(routes.is_empty());
    }

    #[test]
    fn an_invalid_route_fails_at_load_rather_than_at_stream_time() {
        let err = parse("[[route]]\nrooms = []\n").expect_err("an empty route must not load");
        assert!(err.to_string().contains("route 0"), "{err}");
    }
}
