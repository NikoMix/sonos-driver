# `sonos-control` — Manual Live-Device Test Checklist

The automated suite (`cargo test -p sonos-control`) covers discovery, SOAP
envelopes, headers, fault mapping, and topology parsing against loopback mocks —
**no hardware required**. This checklist covers the parts that can only be
validated against a real Sonos player on the LAN.

## Prerequisites

- A Windows host on the **same L2 subnet** as the Sonos system (SSDP multicast
  `239.255.255.250:1900` is not routed across subnets/VLANs).
- One or more Sonos players powered on. Note at least one **room name** and, if
  present, a **stereo pair** or a **grouped** set of rooms.
- A reachable plain-HTTP audio URL for playback (e.g. a file served by
  `python -m http.server` or any LAN web server). TLS is intentionally
  unsupported; use `http://`.
- Allow the test process through Windows Defender Firewall for UDP (discovery)
  and outbound TCP 1400 (control) when prompted.

A tiny scratch binary is the easiest harness:

```rust
// examples/live_smoke.rs  (cargo run -p sonos-control --example live_smoke -- "Living Room")
use std::time::Duration;
use sonos_control::{controller_for_room, discover, discover_zone_groups};

fn main() -> audio_core::Result<()> {
    let room = std::env::args().nth(1).unwrap_or_else(|| "Living Room".into());
    let url = std::env::args().nth(2)
        .unwrap_or_else(|| "http://192.168.1.10:8000/test.wav".into());

    for d in discover(Duration::from_secs(3))? {
        println!("device: {:<16} {:<14} {} ({})", d.room_name, d.model_name, d.ip, d.uuid);
    }
    for g in discover_zone_groups(Duration::from_secs(3))? {
        let rooms: Vec<_> = g.members.iter().map(|m| m.zone_name.as_str()).collect();
        println!("group coordinator={} members={rooms:?}", g.coordinator_uuid);
    }

    let ctl = controller_for_room(Duration::from_secs(3), &room)?;
    ctl.set_volume(25)?;
    ctl.set_stream_uri(&url, "audio/L16")?;
    ctl.play()?;
    std::thread::sleep(Duration::from_secs(10));
    ctl.stop()?;
    Ok(())
}
```

> The example file is not committed (it would pull `audio_core` into the crate's
> dev-deps and bind tests to hardware). Paste it locally when verifying.

## Checklist

### 1. Discovery
- [ ] `discover(3s)` lists **every** powered-on room exactly once.
- [ ] `room_name`, `model_name`, `ip`, and `uuid` are correct for each.
- [ ] A second run is stable (no duplicates, no missing rooms).
- [ ] Discovery completes at/under the requested timeout even if a player is
      unplugged mid-search (unreachable devices are skipped, not fatal).

### 2. Topology / groups
- [ ] `discover_zone_groups(3s)` reports the same grouping shown in the Sonos app.
- [ ] For a **stereo pair**, both speakers appear under one coordinator.
- [ ] For a **grouped** set of rooms, the coordinator matches the app's group head.
- [ ] `controller_for_room("<satellite/member room>")` resolves to the
      **coordinator** (commands sent to a member must drive the whole group).

### 3. Playback control (`SetAVTransportURI` + `Play`)
- [ ] `set_stream_uri(http_url, "audio/L16")` returns `Ok` (no SOAP fault).
- [ ] `play()` starts audible playback of the URL on the target room/group.
- [ ] The Sonos app shows the stream as the current track.

### 4. Stop
- [ ] `stop()` halts playback immediately and returns `Ok`.

### 5. Volume (`SetVolume`)
- [ ] `set_volume(25)` then `set_volume(40)` audibly changes the level and is
      reflected in the app.
- [ ] `set_volume(101)` returns an `Err` **before** any network I/O.

### 6. Error / fault handling
- [ ] Pointing `set_stream_uri` at an **unreachable** URL then `play()` surfaces a
      typed `audio_core::Error` whose message includes the UPnP error code
      (commonly `714`/`716`/`701`), not a panic.
- [ ] Targeting a powered-off room (`controller_for_room`) returns a clear
      `Error::Control` ("no Sonos room named …").

### 7. Invariants
- [ ] No hangs: every call returns within the configured timeout.
- [ ] Re-running the smoke test leaves the system in a clean state (stopped).

## Notes

- Sonos control is **plain HTTP on TCP 1400**; there is no TLS to configure.
- If discovery finds nothing, verify the host NIC is on the Sonos subnet and that
  no "AP/client isolation" or multicast-filtering setting is enabled on the
  access point.
