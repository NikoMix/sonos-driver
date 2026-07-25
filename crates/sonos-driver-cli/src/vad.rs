//! `vad` subcommand handlers: inspect, bind, release and serve the virtual
//! audio outputs exposed by the `sonosvad` kernel driver.
//!
//! The driver publishes a fixed pool of render endpoints ("Sonos Output 1..N").
//! Binding renames them to the discovered Sonos rooms so they appear in the
//! Windows sound picker under the room's own name; `serve` then runs one bridge
//! per bound endpoint, loopback-capturing it and streaming to that room.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde::Serialize;
use sonos_driver_core::capture::LoopbackCapture;
use sonos_driver_core::core::{AudioSource, CaptureConfig, Config};
use sonos_driver_core::vad::RoomKey;
use sonos_driver_core::vad_endpoint::{
    factory_name_for_slot, plan_bindings, SystemEndpoints, VadEndpoint, VadEndpoints,
    DEFAULT_ADAPTER_NAME,
};
use sonos_driver_core::{control, MultiRoomBridge, RoomSession};

use crate::cli::{VadBindArgs, VadCommand, VadProbeArgs, VadServeArgs, VadStatusArgs};
use crate::commands::install_ctrl_c;
use crate::config;

/// Dispatch a `vad` subcommand.
pub(crate) fn run_vad(command: &VadCommand, config_path: Option<&Path>) -> anyhow::Result<()> {
    match command {
        VadCommand::Status(args) => run_status(args),
        VadCommand::Bind(args) => run_bind(args),
        VadCommand::Release => run_release(),
        VadCommand::Probe(args) => run_probe(args),
        VadCommand::Serve(args) => run_serve(config_path, args),
    }
}

/// A row of `vad status` output.
#[derive(Debug, Serialize)]
struct EndpointRow {
    slot: Option<u32>,
    name: String,
    factory_name: String,
    adapter: String,
    bound: bool,
    active: bool,
    device_id: String,
}

impl From<&VadEndpoint> for EndpointRow {
    fn from(endpoint: &VadEndpoint) -> Self {
        Self {
            slot: endpoint.slot,
            name: endpoint.friendly_name.clone(),
            factory_name: endpoint.device_desc.clone(),
            adapter: endpoint.adapter.clone(),
            bound: !endpoint.is_unbound(),
            active: endpoint.active,
            device_id: endpoint.device_id.clone(),
        }
    }
}

fn run_status(args: &VadStatusArgs) -> anyhow::Result<()> {
    let adapter = args.adapter.as_deref().unwrap_or(DEFAULT_ADAPTER_NAME);
    let endpoints = SystemEndpoints::for_adapter(adapter)
        .list()
        .with_context(|| format!("listing endpoints of '{adapter}'"))?;
    let rows: Vec<EndpointRow> = endpoints.iter().map(EndpointRow::from).collect();

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).context("serialising endpoint list")?
        );
        return Ok(());
    }

    println!(
        "{:<5} {:<28} {:<18} {:<7} BOUND",
        "SLOT", "NAME", "FACTORY", "STATE"
    );
    for row in &rows {
        println!(
            "{:<5} {:<28} {:<18} {:<7} {}",
            row.slot.map_or_else(|| "-".to_owned(), |s| s.to_string()),
            row.name,
            row.factory_name,
            if row.active { "active" } else { "idle" },
            if row.bound { "yes" } else { "no" },
        );
    }
    let scope = if adapter.is_empty() {
        "every render adapter".to_owned()
    } else {
        format!("'{adapter}'")
    };
    println!("\n{} endpoint(s) from {scope}.", rows.len());
    Ok(())
}

fn run_bind(args: &VadBindArgs) -> anyhow::Result<()> {
    let rooms = discover_rooms(args.seed_host.as_deref(), Duration::from_secs(args.timeout))?;
    let endpoints = SystemEndpoints::new();

    if args.dry_run {
        let current = match args.assume_endpoints {
            Some(count) => synthetic_endpoints(count),
            None => endpoints.list().context("listing sonosvad endpoints")?,
        };
        let plan = plan_bindings(&current, &rooms);
        println!("# Bindings that would be applied:\n");
        for bind in &plan.binds {
            let mark = if bind.rename_needed { "*" } else { " " };
            println!("  {mark} {} -> {}", bind.name, bind.room.name);
        }
        for release in &plan.releases {
            println!("  * {} (released)", release.name);
        }
        if plan.unplaced > 0 {
            println!(
                "\n{} room(s) could not be placed; rebuild the driver with a larger endpoint count.",
                plan.unplaced
            );
        }
        println!("\n('*' marks endpoints that would actually be renamed.)");
        if args.assume_endpoints.is_some() {
            println!("(Planned against synthetic endpoints; nothing was read from the driver.)");
        }
        return Ok(());
    }

    let bindings = sonos_driver_core::vad_endpoint::bind_rooms(&endpoints, &rooms)
        .context("binding Sonos rooms to sonosvad endpoints")?;

    println!("Bound {} room(s):", bindings.len());
    for binding in &bindings {
        println!("  {} -> {}", binding.name, binding.room.name);
    }
    Ok(())
}

/// A pristine, factory-named endpoint set as the driver would expose it right
/// after installation.
///
/// Used by `--assume-endpoints` so the room-to-endpoint plan can be reviewed
/// where the driver is absent. Every endpoint is reported inactive because none
/// of them exist.
fn synthetic_endpoints(count: u32) -> Vec<VadEndpoint> {
    (1..=count)
        .map(|slot| {
            let name = factory_name_for_slot(slot);
            VadEndpoint {
                device_id: format!("synthetic-slot-{slot}"),
                friendly_name: name.clone(),
                device_desc: name,
                adapter: DEFAULT_ADAPTER_NAME.to_owned(),
                slot: Some(slot),
                active: false,
            }
        })
        .collect()
}

fn run_release() -> anyhow::Result<()> {
    let endpoints = SystemEndpoints::new();
    let current = endpoints.list().context("listing sonosvad endpoints")?;
    // Binding against an empty room set releases every slot.
    let plan = plan_bindings(&current, &[]);
    let count = plan.releases.len();
    sonos_driver_core::vad_endpoint::apply_plan(&endpoints, &plan)
        .context("restoring factory endpoint names")?;
    println!("Released {count} endpoint(s) back to their factory names.");
    Ok(())
}

/// The outcome of a `vad probe` run.
#[derive(Debug, Serialize)]
struct ProbeReport {
    device: String,
    device_id: String,
    sample_rate: u32,
    channels: u16,
    elapsed_secs: f64,
    captured_secs: f64,
    /// Captured audio time minus wall-clock time, as a percentage.
    ///
    /// A virtual device that advances its position faster than real time
    /// produces a positive drift and will eventually outrun any downstream
    /// buffer; slower than real time starves it.
    drift_percent: f64,
    tolerance_percent: f64,
    frames: u64,
    reads: u64,
    /// Largest gap between two consecutive reads, in milliseconds. A large
    /// value means the driver stalled its notification.
    max_read_gap_ms: f64,
    peak_amplitude: f32,
    silent: bool,
    overruns: u64,
    reconnects: u64,
    ok: bool,
}

fn run_probe(args: &VadProbeArgs) -> anyhow::Result<()> {
    if args.seconds == 0 {
        bail!("--seconds must be at least 1");
    }

    let (device_name, device_id) = resolve_probe_target(args.device.as_deref())?;

    let mut capture = LoopbackCapture::open(&CaptureConfig {
        device: Some(device_id.clone()),
        process: None,
        exclude_process: false,
    })
    .with_context(|| format!("opening a loopback capture on '{device_name}'"))?;

    let format = capture.format();
    let duration = Duration::from_secs(args.seconds);

    // Wall-clock time is the reference. The keep-alive renderer inside
    // `audio-capture` guarantees frames keep arriving even with nothing
    // playing, so a shortfall here is the driver's clock, not an idle system.
    let started = Instant::now();
    let mut last_read = started;
    let mut frames: u64 = 0;
    let mut reads: u64 = 0;
    let mut max_gap = Duration::ZERO;
    let mut peak = 0.0_f32;

    while started.elapsed() < duration {
        match capture.read().context("reading captured audio")? {
            Some(frame) => {
                let now = Instant::now();
                max_gap = max_gap.max(now.duration_since(last_read));
                last_read = now;
                reads += 1;
                frames += frame.frame_count() as u64;
                for sample in frame.samples() {
                    peak = peak.max(sample.abs());
                }
            }
            None => bail!("the capture ended early; the endpoint disappeared mid-probe"),
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    let captured =
        f64::from(u32::try_from(frames).unwrap_or(u32::MAX)) / f64::from(format.sample_rate);
    let drift = (captured - elapsed) / elapsed * 100.0;

    let report = ProbeReport {
        device: device_name,
        device_id,
        sample_rate: format.sample_rate,
        channels: format.channels,
        elapsed_secs: elapsed,
        captured_secs: captured,
        drift_percent: drift,
        tolerance_percent: args.tolerance,
        frames,
        reads,
        max_read_gap_ms: max_gap.as_secs_f64() * 1000.0,
        // `f32::EPSILON` would be too tight: a dithered silent stream is not
        // exactly zero. -120 dBFS is comfortably below anything audible.
        silent: peak < 1.0e-6,
        peak_amplitude: peak,
        overruns: capture.overrun_count(),
        reconnects: capture.reconnect_count(),
        ok: drift.abs() <= args.tolerance && frames > 0,
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialising the probe report")?
        );
    } else {
        print_probe_report(&report);
    }

    if !report.ok {
        bail!(
            "clock drift {:+.3}% exceeds the {:.3}% tolerance; the driver is not advancing its stream position at real time",
            report.drift_percent,
            report.tolerance_percent
        );
    }
    Ok(())
}

fn print_probe_report(report: &ProbeReport) {
    println!("Endpoint     : {} ({})", report.device, report.device_id);
    println!(
        "Format       : {} Hz, {} ch",
        report.sample_rate, report.channels
    );
    println!(
        "Elapsed      : {:.3} s wall clock, {:.3} s captured",
        report.elapsed_secs, report.captured_secs
    );
    println!(
        "Clock drift  : {:+.3} % (tolerance +/-{:.3} %)",
        report.drift_percent, report.tolerance_percent
    );
    println!(
        "Reads        : {} ({} frames), longest gap {:.1} ms",
        report.reads, report.frames, report.max_read_gap_ms
    );
    println!(
        "Signal       : peak {:.6}{}",
        report.peak_amplitude,
        if report.silent { " (silence)" } else { "" }
    );
    println!(
        "Health       : {} overrun(s), {} reconnect(s)",
        report.overruns, report.reconnects
    );
    println!("Result       : {}", if report.ok { "PASS" } else { "FAIL" });
}

/// Resolve `--device` to a `(name, id)` pair, defaulting to the first sonosvad
/// endpoint.
///
/// The id is always passed to the capture layer rather than the name: names are
/// mutable (that is the whole point of binding) and ambiguous, ids are not.
fn resolve_probe_target(selector: Option<&str>) -> anyhow::Result<(String, String)> {
    let endpoints = SystemEndpoints::new()
        .list()
        .context("listing sonosvad endpoints")?;

    let Some(selector) = selector else {
        let first = endpoints
            .first()
            .context("the sonosvad driver exposes no endpoints")?;
        return Ok((first.friendly_name.clone(), first.device_id.clone()));
    };

    let matched = endpoints
        .iter()
        .find(|e| e.device_id == selector)
        .or_else(|| {
            endpoints.iter().find(|e| {
                e.friendly_name
                    .to_lowercase()
                    .contains(&selector.to_lowercase())
            })
        })
        .with_context(|| {
            format!(
                "no sonosvad endpoint matches {selector:?}; available: {:?}",
                endpoints
                    .iter()
                    .map(|e| &e.friendly_name)
                    .collect::<Vec<_>>()
            )
        })?;

    Ok((matched.friendly_name.clone(), matched.device_id.clone()))
}

fn run_serve(config_path: Option<&Path>, args: &VadServeArgs) -> anyhow::Result<()> {
    let config = effective_serve_config(config_path, args)?;
    config::validate(&config, false)?;

    let rooms = discover_rooms(args.seed_host.as_deref(), Duration::from_secs(args.timeout))?;
    let mut supervisor = MultiRoomBridge::new(config);
    let sessions = supervisor
        .plan(&SystemEndpoints::new(), &rooms)
        .context("planning per-room sessions")?;

    print_plan(&sessions);
    if args.dry_run {
        return Ok(());
    }

    install_ctrl_c(supervisor.stop_handle())?;
    println!(
        "\nStreaming {} room(s). Press Ctrl-C to stop.",
        sessions.len()
    );

    supervisor
        .run_sessions(&sessions)
        .context("running the multi-room bridge")?;
    println!("All room bridges stopped cleanly.");
    Ok(())
}

fn print_plan(sessions: &[RoomSession]) {
    println!("{:<28} {:<20} PORT", "WINDOWS OUTPUT", "SONOS ROOM");
    for session in sessions {
        println!(
            "{:<28} {:<20} {}",
            session.device_name, session.room_name, session.port
        );
    }
}

/// Build the effective `vad serve` config: file (if any) then explicit flags.
fn effective_serve_config(
    config_path: Option<&Path>,
    args: &VadServeArgs,
) -> anyhow::Result<Config> {
    let mut config = match config_path {
        Some(path) => config::load_config(path)?,
        None => Config::default(),
    };
    if let Some(profile) = args.profile {
        config.stream.profile = profile.into();
    }
    if let Some(port) = args.port {
        config.stream.port = port;
    }
    if let Some(bind) = &args.bind_address {
        config.stream.bind_address.clone_from(bind);
    }
    if let Some(rate) = args.target_sample_rate {
        config.pipeline.target_sample_rate = (rate != 0).then_some(rate);
    }
    if let Some(buffer_ms) = args.buffer_ms {
        config.pipeline.buffer_ms = buffer_ms;
    }
    if let Some(seed) = &args.seed_host {
        config.sonos.seed_host = Some(seed.clone());
    }
    config.sonos.discovery_timeout_secs = u32::try_from(args.timeout).unwrap_or(u32::MAX);
    Ok(config)
}

/// Discover Sonos rooms, de-duplicated to one entry per zone-group coordinator
/// where the topology is available.
///
/// Grouped players share one transport, so binding every member would create
/// several Windows outputs that all fight over the same speakers. A four-piece
/// home-theatre set (soundbar + sub + two surrounds) is a single room, not four.
fn discover_rooms(seed: Option<&str>, timeout: Duration) -> anyhow::Result<Vec<RoomKey>> {
    let devices =
        control::discover_household(seed, timeout).context("discovering Sonos devices")?;
    if devices.is_empty() {
        bail!("no Sonos devices were found on the local network");
    }

    let groups = control::household_zone_groups(seed, timeout).unwrap_or_else(|err| {
        tracing::debug!(%err, "zone-group topology unavailable; binding every player");
        Vec::new()
    });

    let mut rooms: Vec<RoomKey> = Vec::new();
    for device in &devices {
        if is_non_coordinator(&groups, &device.uuid) {
            tracing::debug!(room = %device.room_name, "skipping grouped non-coordinator");
            continue;
        }
        rooms.push(RoomKey::new(device.uuid.clone(), device.room_name.clone()));
    }

    // Deterministic order keeps slot assignment stable between runs.
    rooms.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.uuid.cmp(&b.uuid)));
    Ok(rooms)
}

/// Whether `uuid` belongs to a zone group but is not its coordinator.
fn is_non_coordinator(groups: &[control::ZoneGroup], uuid: &str) -> bool {
    groups.iter().any(|group| {
        group.members.len() > 1
            && group.contains_uuid(uuid)
            && !group.coordinator_uuid.eq_ignore_ascii_case(uuid)
    })
}

#[cfg(test)]
mod tests {
    use sonos_driver_core::control::{ZoneGroup, ZoneGroupMember};

    use super::*;

    fn member(uuid: &str) -> ZoneGroupMember {
        ZoneGroupMember {
            uuid: uuid.to_owned(),
            zone_name: uuid.to_owned(),
            location: "http://192.168.1.10:1400/xml/device_description.xml".to_owned(),
        }
    }

    fn group(coordinator: &str, members: &[&str]) -> ZoneGroup {
        ZoneGroup {
            id: format!("{coordinator}:1"),
            coordinator_uuid: coordinator.to_owned(),
            members: members.iter().copied().map(member).collect(),
        }
    }

    #[test]
    fn a_lone_player_is_never_treated_as_a_non_coordinator() {
        assert!(!is_non_coordinator(&[group("A", &["A"])], "A"));
    }

    #[test]
    fn grouped_members_other_than_the_coordinator_are_skipped() {
        let groups = [group("A", &["A", "B"])];
        assert!(!is_non_coordinator(&groups, "A"));
        assert!(is_non_coordinator(&groups, "B"));
    }

    #[test]
    fn unknown_uuids_are_left_alone() {
        assert!(!is_non_coordinator(&[group("A", &["A", "B"])], "Z"));
    }

    #[test]
    fn serve_flags_override_the_config_file() {
        let args = VadServeArgs {
            timeout: 7,
            seed_host: None,
            profile: None,
            port: Some(9100),
            bind_address: Some("127.0.0.1".to_owned()),
            target_sample_rate: Some(0),
            buffer_ms: Some(150),
            dry_run: false,
        };
        let config = effective_serve_config(None, &args).expect("config");
        assert_eq!(config.stream.port, 9100);
        assert_eq!(config.stream.bind_address, "127.0.0.1");
        // `0` is the "keep the native capture rate" sentinel.
        assert!(config.pipeline.target_sample_rate.is_none());
        assert_eq!(config.pipeline.buffer_ms, 150);
        assert_eq!(config.sonos.discovery_timeout_secs, 7);
    }
}
