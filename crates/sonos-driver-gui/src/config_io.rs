//! Persisting the routing table and audio settings as `TOML`.
//!
//! The file is deliberately the *same* document the driverless CLI and the
//! `sonoscast` service read: audio settings in their usual tables, plus one
//! `[[route]]` per routed output. So a table built by clicking can be copied to
//! another machine and run headless with `sonos-driver cast --config <file>`,
//! and a file written by hand opens correctly in the app.
//!
//! It lives under `%APPDATA%` rather than `%ProgramData%` because this app
//! streams in its own process, for the logged-in user, and should never need
//! administrator rights to save what the user just clicked.

use std::path::{Path, PathBuf};

use audio_core::{Config, Error, Result};
use serde::{Deserialize, Serialize};
use sonos_routing::RouteConfig;

/// Written at the top of every saved file, so the file explains itself when
/// someone opens it in an editor a year from now.
const HEADER: &str = "\
# Written by the Sonos Router app. Safe to edit by hand.
#
# Each [[route]] says: capture this output, play it in these rooms.
#   device = \"<Windows output name>\"   omit it to follow the Windows default
#   rooms  = \"all\" | [\"Room name\", ...]
#
# This is the same format as cast.toml, so the same file also works headless:
#   sonos-driver cast --config <this file>
";

/// The `[[route]]` tables, read from the same document as the audio settings.
///
/// Parsed separately rather than with `#[serde(flatten)]`: neither struct
/// rejects unknown fields, so each can read the same text and take only what it
/// understands, which avoids serde's flatten/TOML interaction entirely. This
/// mirrors `sonos_cast_service::config` on purpose — one format, one behaviour.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RoutesDocument {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    route: Vec<RouteConfig>,
}

/// Serialise settings and routes into one `TOML` document.
///
/// # Errors
///
/// Returns [`audio_core::Error::Config`] if serialisation fails, which should
/// only happen for a logically inconsistent value.
pub fn to_toml(config: &Config, routes: &[RouteConfig]) -> Result<String> {
    let settings = toml::to_string_pretty(config)
        .map_err(|err| Error::config(format!("failed to serialise settings: {err}")))?;
    let routes = toml::to_string_pretty(&RoutesDocument {
        route: routes.to_vec(),
    })
    .map_err(|err| Error::config(format!("failed to serialise routes: {err}")))?;

    // Routes are arrays of tables, so they have to follow every other table in
    // the document; appending is both the simplest and the only correct order.
    Ok(format!("{HEADER}\n{settings}\n{routes}"))
}

/// Parse settings and routes from one `TOML` document.
///
/// # Errors
///
/// Returns [`audio_core::Error::Config`] if the document is not valid `TOML`,
/// does not match the settings schema, or contains a malformed route.
pub fn from_toml(text: &str) -> Result<(Config, Vec<RouteConfig>)> {
    let config: Config = toml::from_str(text)
        .map_err(|err| Error::config(format!("failed to parse settings: {err}")))?;
    let document: RoutesDocument = toml::from_str(text)
        .map_err(|err| Error::config(format!("failed to parse routes: {err}")))?;
    Ok((config, document.route))
}

/// Load settings and routes from `path`.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read, or
/// [`audio_core::Error::Config`] if its contents are not valid.
pub fn load(path: &Path) -> Result<(Config, Vec<RouteConfig>)> {
    let text = std::fs::read_to_string(path)?;
    from_toml(&text)
}

/// Save settings and routes to `path`, creating parent directories as needed.
///
/// # Errors
///
/// Returns an I/O error if the directory or file cannot be written, or
/// [`audio_core::Error::Config`] if serialisation fails.
pub fn save(path: &Path, config: &Config, routes: &[RouteConfig]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let text = to_toml(config, routes)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// The default per-user configuration path.
///
/// On Windows this is `%APPDATA%\sonos-driver\routes.toml`; if `APPDATA` is
/// unset it falls back to a relative file in the working directory.
#[must_use]
pub fn default_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("APPDATA") {
        return PathBuf::from(dir).join("sonos-driver").join("routes.toml");
    }
    PathBuf::from("routes.toml")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{from_toml, load, save, to_toml};
    use audio_core::{CodecProfile, Config};
    use sonos_routing::{RoomsField, RouteConfig};

    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("sonos-router-{}-{n}-{tag}", std::process::id()))
    }

    fn routes() -> Vec<RouteConfig> {
        vec![
            RouteConfig {
                rooms: RoomsField::Keyword("all".to_owned()),
                device: None,
                process: None,
                except_process: None,
            },
            RouteConfig {
                rooms: RoomsField::List(vec!["Küche".to_owned()]),
                device: Some("LG TV (NVIDIA HDMI)".to_owned()),
                process: None,
                except_process: None,
            },
        ]
    }

    #[test]
    fn settings_and_routes_round_trip_through_one_document() {
        let mut config = Config::default();
        config.stream.profile = CodecProfile::Wav;
        config.stream.port = 9100;
        config.sonos.seed_host = Some("192.168.1.50".to_owned());

        let text = to_toml(&config, &routes()).expect("serialise");
        let (parsed, parsed_routes) = from_toml(&text).expect("parse");
        assert_eq!(parsed, config);
        assert_eq!(parsed_routes, routes());
    }

    #[test]
    fn a_saved_file_is_readable_by_the_headless_host() {
        // The whole point of sharing the format: whatever the app writes, the
        // service's own loader must accept unchanged.
        let text = to_toml(&Config::default(), &routes()).expect("serialise");
        let path = unique_temp_dir("portable").join("routes.toml");
        save(&path, &Config::default(), &routes()).expect("save");

        let (_, loaded) =
            sonos_cast_service::config::load(Some(&path)).expect("the service must accept it");
        assert_eq!(loaded.len(), 2);
        assert!(text.contains("[[route]]"));

        std::fs::remove_dir_all(path.parent().expect("parent")).ok();
    }

    #[test]
    fn the_header_explains_the_file() {
        let text = to_toml(&Config::default(), &routes()).expect("serialise");
        assert!(text.starts_with('#'));
        assert!(text.contains("sonos-driver cast --config"));
    }

    #[test]
    fn a_document_with_no_routes_is_valid() {
        let text = to_toml(&Config::default(), &[]).expect("serialise");
        let (_, parsed) = from_toml(&text).expect("parse");
        assert!(parsed.is_empty());
        assert!(
            !text.lines().any(|line| line.trim() == "[[route]]"),
            "an empty routing must not write an empty route table"
        );
    }

    #[test]
    fn save_then_load_creates_missing_directories() {
        let dir = unique_temp_dir("roundtrip");
        let path = dir.join("nested").join("routes.toml");
        save(&path, &Config::default(), &routes()).expect("save");

        let (config, loaded) = load(&path).expect("load");
        assert_eq!(config, Config::default());
        assert_eq!(loaded, routes());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(from_toml("this is = = not toml").is_err());
    }

    #[test]
    fn a_route_with_an_unknown_key_is_rejected_rather_than_ignored() {
        // `deny_unknown_fields` on RouteConfig: a typo like `room = ` must be
        // an error, not a route that silently plays nowhere.
        assert!(from_toml("[[route]]\nroom = \"Küche\"\n").is_err());
    }

    #[test]
    fn loading_a_missing_file_is_an_error() {
        let path = unique_temp_dir("missing").join("absent.toml");
        assert!(load(&path).is_err());
    }
}
