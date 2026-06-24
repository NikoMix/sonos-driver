//! Subcommand handlers: discovery/listing, streaming with graceful Ctrl-C,
//! one-shot stop, profile listing, and config inspection.
//!
//! Discovery, control, and the bridge run loop are reached only through the
//! stable public API re-exported by [`sonos_driver_core`]; this crate never
//! depends on their internals.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context};
use serde::Serialize;
use sonos_driver_core::core::{CodecProfile, Config};
use sonos_driver_core::{control, Bridge, LatencyEstimate, StopHandle};

use crate::cli::{ConfigCommand, ListArgs, StopArgs, StreamArgs};
use crate::config;

/// Print every streaming profile with its honest end-to-end latency estimate.
pub(crate) fn run_profiles() {
    println!("Available streaming profiles (latency includes the fixed Sonos buffer):\n");
    for profile in [
        CodecProfile::L16Pcm,
        CodecProfile::Wav,
        CodecProfile::Flac,
        CodecProfile::Aac,
        CodecProfile::Mp3,
    ] {
        let est = LatencyEstimate::for_profile(profile);
        let kind = if profile.is_lossless() {
            "lossless"
        } else {
            "lossy"
        };
        println!(
            "  {:<8} {:<14} {}\n           {}",
            format!("{profile:?}"),
            est.approx_range,
            kind,
            profile.description(),
        );
    }
}

/// Discover players and print them as a table or JSON, enriched with zone-group
/// role/coordinator information when the topology is available.
pub(crate) fn run_list(args: &ListArgs) -> anyhow::Result<()> {
    let timeout = Duration::from_secs(args.timeout);
    let devices = control::discover(timeout).context("discovering Sonos devices")?;
    let groups = control::discover_zone_groups(timeout).unwrap_or_else(|err| {
        tracing::debug!(%err, "zone-group topology unavailable; listing without group info");
        Vec::new()
    });

    let rows: Vec<DeviceRow> = devices
        .iter()
        .map(|device| DeviceRow::from_device(device, &groups))
        .collect();

    if args.json {
        let json = serde_json::to_string_pretty(&rows).context("serialising device list")?;
        println!("{json}");
    } else {
        print_device_table(&rows);
    }
    Ok(())
}

/// Build the effective config (flags over file over defaults), then either print
/// it (`--dry-run`) or start the bridge and block until Ctrl-C / failure.
pub(crate) fn run_stream(config_path: Option<&Path>, args: &StreamArgs) -> anyhow::Result<()> {
    let config = config::effective_stream_config(config_path, args)?;
    if args.dry_run {
        return dry_run_report(&config);
    }
    config::validate(&config, true)?;
    start_bridge(config)
}

fn dry_run_report(config: &Config) -> anyhow::Result<()> {
    println!("# Effective configuration (dry run):\n");
    print_config_toml(config)?;
    config::validate(config, false)?;
    if !config::has_target(config) {
        eprintln!("note: no target room/uuid set — streaming will require --room or --uuid.");
    }
    Ok(())
}

fn start_bridge(config: Config) -> anyhow::Result<()> {
    let mut bridge = Bridge::new(config);
    let est = bridge.latency_estimate();
    // Acquire the stop handle BEFORE run() (which blocks), then wire Ctrl-C to it.
    install_ctrl_c(bridge.stop_handle())?;

    println!(
        "Starting bridge: profile {:?}, expected end-to-end latency {} (includes the fixed Sonos buffer).",
        est.profile, est.approx_range
    );
    println!("Press Ctrl-C to stop.");

    match bridge.run() {
        Ok(()) => {
            println!("Bridge stopped cleanly.");
            Ok(())
        }
        Err(err) => Err(err).context("running the audio bridge"),
    }
}

/// Install a Ctrl-C / console-close handler that signals `handle` to stop.
fn install_ctrl_c(handle: StopHandle) -> anyhow::Result<()> {
    ctrlc::set_handler(stop_signal_handler(handle)).context("installing the Ctrl-C handler")?;
    Ok(())
}

/// The handler body wired into the OS signal: requests a graceful stop. Split
/// out so the wiring can be unit-tested without delivering a real signal.
fn stop_signal_handler(handle: StopHandle) -> impl FnMut() + Send + 'static {
    move || handle.stop()
}

/// Stop playback on a room or a specific device UUID (one-shot control plane).
pub(crate) fn run_stop(args: &StopArgs) -> anyhow::Result<()> {
    let timeout = Duration::from_secs(args.timeout);
    let controller = resolve_stop_controller(args, timeout)?;
    let room = controller.device().room_name.clone();
    let ip = controller.device().ip;
    println!("Stopping playback on {room} ({ip})...");
    controller.stop().context("sending AVTransport Stop")?;
    println!("Stopped {room}.");
    Ok(())
}

fn resolve_stop_controller(
    args: &StopArgs,
    timeout: Duration,
) -> anyhow::Result<control::SonosController> {
    if let Some(room) = args.room.as_deref() {
        return control::controller_for_room(timeout, room)
            .with_context(|| format!("resolving room '{room}'"));
    }
    let uuid = args
        .uuid
        .as_deref()
        .ok_or_else(|| anyhow!("a target room or uuid is required"))?;
    controller_for_uuid(timeout, uuid)
}

/// Resolve a device UUID to a controller for the coordinator that drives its
/// group, falling back to a direct controller if the device isn't grouped.
fn controller_for_uuid(timeout: Duration, uuid: &str) -> anyhow::Result<control::SonosController> {
    let groups =
        control::discover_zone_groups(timeout).context("discovering Sonos zone topology")?;
    if let Some(group) = groups
        .iter()
        .find(|g| g.members.iter().any(|m| m.uuid.eq_ignore_ascii_case(uuid)))
    {
        if let Some(coord) = group.coordinator().or_else(|| group.members.first()) {
            let ip = coord.ip().ok_or_else(|| {
                anyhow!("group coordinator for UUID '{uuid}' has no usable IPv4 address")
            })?;
            let device = control::fetch_device(&coord.location, timeout).unwrap_or_else(|_| {
                control::SonosDevice {
                    uuid: coord.uuid.clone(),
                    room_name: coord.zone_name.clone(),
                    model_name: "Sonos".to_owned(),
                    ip,
                }
            });
            return Ok(control::SonosController::new(device));
        }
    }

    let device = control::discover(timeout)
        .context("discovering Sonos devices")?
        .into_iter()
        .find(|d| d.uuid.eq_ignore_ascii_case(uuid))
        .ok_or_else(|| anyhow!("no Sonos device with UUID '{uuid}' was found"))?;
    Ok(control::SonosController::new(device))
}

/// Handle the `config` subcommand family.
pub(crate) fn run_config(
    command: &ConfigCommand,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    match command {
        ConfigCommand::Example => {
            print!("{}", config::example_toml());
            Ok(())
        }
        ConfigCommand::Check => check_config(config_path),
    }
}

fn check_config(config_path: Option<&Path>) -> anyhow::Result<()> {
    let config = if let Some(path) = config_path {
        println!("# Loaded configuration from {}", path.display());
        config::load_config(path)?
    } else {
        println!("# No --config given; showing built-in defaults.");
        Config::default()
    };
    print_config_toml(&config)?;
    config::validate(&config, false)?;
    if !config::has_target(&config) {
        eprintln!("note: no target room/uuid set — `stream` will require --room or --uuid.");
    }
    if config.sonos.discovery_timeout_secs == 0 {
        eprintln!(
            "note: sonos.discovery-timeout-secs is 0; the bridge falls back to its built-in discovery timeout."
        );
    }
    println!("\nConfiguration is valid.");
    Ok(())
}

fn print_config_toml(config: &Config) -> anyhow::Result<()> {
    let rendered = toml::to_string_pretty(config).context("serialising configuration")?;
    print!("{rendered}");
    Ok(())
}

/// The role a device plays within its zone group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DeviceRole {
    /// Drives a multi-member group.
    Coordinator,
    /// A non-coordinator member of a group.
    Member,
    /// A single-player group (its own coordinator).
    Standalone,
    /// Topology unknown (no group info available).
    Unknown,
}

impl DeviceRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Member => "member",
            Self::Standalone => "standalone",
            Self::Unknown => "unknown",
        }
    }
}

/// A single row of the `list` output, serialisable for `--json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
struct DeviceRow {
    room: String,
    model: String,
    ip: String,
    uuid: String,
    role: DeviceRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_coordinator_uuid: Option<String>,
}

impl DeviceRow {
    fn from_device(device: &control::SonosDevice, groups: &[control::ZoneGroup]) -> Self {
        let membership = groups
            .iter()
            .find(|g| g.members.iter().any(|m| m.uuid == device.uuid));

        let (role, group, coordinator_uuid) = match membership {
            Some(group) => {
                let role = if group.coordinator_uuid == device.uuid {
                    if group.members.len() <= 1 {
                        DeviceRole::Standalone
                    } else {
                        DeviceRole::Coordinator
                    }
                } else {
                    DeviceRole::Member
                };
                let label = group.coordinator().map(|c| c.zone_name.clone());
                (role, label, Some(group.coordinator_uuid.clone()))
            }
            None => (DeviceRole::Unknown, None, None),
        };

        Self {
            room: device.room_name.clone(),
            model: device.model_name.clone(),
            ip: device.ip.to_string(),
            uuid: device.uuid.clone(),
            role,
            group,
            group_coordinator_uuid: coordinator_uuid,
        }
    }
}

fn print_device_table(rows: &[DeviceRow]) {
    if rows.is_empty() {
        println!("No Sonos devices found.");
        return;
    }
    let room_w = col_width(rows.iter().map(|r| r.room.as_str()), "ROOM");
    let model_w = col_width(rows.iter().map(|r| r.model.as_str()), "MODEL");
    let ip_w = col_width(rows.iter().map(|r| r.ip.as_str()), "IP");

    println!(
        "{}  {}  {}  {}  GROUP",
        pad("ROOM", room_w),
        pad("MODEL", model_w),
        pad("IP", ip_w),
        pad("ROLE", ROLE_W),
    );
    for row in rows {
        let group = row.group.as_deref().unwrap_or("-");
        println!(
            "{}  {}  {}  {}  {}",
            pad(&row.room, room_w),
            pad(&row.model, model_w),
            pad(&row.ip, ip_w),
            pad(row.role.as_str(), ROLE_W),
            group,
        );
    }
}

/// Width of the role column: the longest role name ("coordinator").
const ROLE_W: usize = 11;

fn pad(value: &str, width: usize) -> String {
    format!("{value:<width$}")
}

fn col_width<'a>(values: impl Iterator<Item = &'a str>, header: &str) -> usize {
    values
        .map(str::len)
        .chain(std::iter::once(header.len()))
        .max()
        .unwrap_or(header.len())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use sonos_driver_core::core::Config;
    use sonos_driver_core::Bridge;

    use super::*;

    fn member(uuid: &str, room: &str, ip: &str) -> control::ZoneGroupMember {
        control::ZoneGroupMember {
            uuid: uuid.to_owned(),
            zone_name: room.to_owned(),
            location: format!("http://{ip}:1400/xml/device_description.xml"),
        }
    }

    fn device(uuid: &str, room: &str, ip: Ipv4Addr) -> control::SonosDevice {
        control::SonosDevice {
            uuid: uuid.to_owned(),
            room_name: room.to_owned(),
            model_name: "Sonos One".to_owned(),
            ip,
        }
    }

    fn topology() -> Vec<control::ZoneGroup> {
        vec![
            control::ZoneGroup {
                id: "G1".to_owned(),
                coordinator_uuid: "AAA".to_owned(),
                members: vec![
                    member("AAA", "Living Room", "192.168.1.50"),
                    member("BBB", "Living Room", "192.168.1.51"),
                ],
            },
            control::ZoneGroup {
                id: "G2".to_owned(),
                coordinator_uuid: "CCC".to_owned(),
                members: vec![member("CCC", "Kitchen", "192.168.1.60")],
            },
        ]
    }

    #[test]
    fn device_role_reflects_topology() {
        let groups = topology();

        let coord = DeviceRow::from_device(
            &device("AAA", "Living Room", Ipv4Addr::new(192, 168, 1, 50)),
            &groups,
        );
        assert_eq!(coord.role, DeviceRole::Coordinator);
        assert_eq!(coord.group.as_deref(), Some("Living Room"));
        assert_eq!(coord.group_coordinator_uuid.as_deref(), Some("AAA"));

        let mem = DeviceRow::from_device(
            &device("BBB", "Living Room", Ipv4Addr::new(192, 168, 1, 51)),
            &groups,
        );
        assert_eq!(mem.role, DeviceRole::Member);

        let standalone = DeviceRow::from_device(
            &device("CCC", "Kitchen", Ipv4Addr::new(192, 168, 1, 60)),
            &groups,
        );
        assert_eq!(standalone.role, DeviceRole::Standalone);

        let unknown = DeviceRow::from_device(
            &device("ZZZ", "Garage", Ipv4Addr::new(192, 168, 1, 70)),
            &groups,
        );
        assert_eq!(unknown.role, DeviceRole::Unknown);
        assert!(unknown.group.is_none());
    }

    #[test]
    fn device_row_serialises_to_json() {
        let groups = topology();
        let row = DeviceRow::from_device(
            &device("AAA", "Living Room", Ipv4Addr::new(192, 168, 1, 50)),
            &groups,
        );
        let json = serde_json::to_string(&row).expect("json");
        assert!(json.contains("\"room\":\"Living Room\""));
        assert!(json.contains("\"role\":\"coordinator\""));
        assert!(json.contains("\"group-coordinator-uuid\":\"AAA\""));
    }

    #[test]
    fn unknown_role_omits_group_fields_in_json() {
        let row = DeviceRow::from_device(&device("ZZZ", "Garage", Ipv4Addr::new(10, 0, 0, 9)), &[]);
        let json = serde_json::to_string(&row).expect("json");
        assert!(!json.contains("group"));
    }

    #[test]
    fn ctrl_c_handler_requests_stop() {
        // Validates the Ctrl-C wiring without delivering a real OS signal.
        let bridge = Bridge::new(Config::default());
        let handle = bridge.stop_handle();
        let mut fire = stop_signal_handler(handle.clone());
        assert!(!handle.is_stop_requested());
        fire();
        assert!(handle.is_stop_requested());
    }
}
