//! Integration tests that touch the real Windows audio endpoint APIs.
//!
//! These are `#[ignore]`d: they need a live audio endpoint, and the rename test
//! needs an elevated process. Run them explicitly:
//!
//! ```text
//! cargo test -p sonos-vad-endpoint -- --ignored --test-threads=1
//! ```
//!
//! `SONOSVAD_TEST_ADAPTER` selects which adapter to exercise. It defaults to
//! the empty string, which disables the adapter filter and picks whatever
//! render endpoint the machine has - correct for a bare CI/sandbox image that
//! has no sonosvad driver installed. Point it at
//! `"Sonos Virtual Audio Device"` to test the real thing.

#![cfg(windows)]

use sonos_vad_endpoint::{Error, SystemEndpoints, VadEndpoints};

fn adapter() -> String {
    std::env::var("SONOSVAD_TEST_ADAPTER").unwrap_or_default()
}

/// Enumeration must return well-formed endpoints: a non-empty WASAPI id and a
/// name. This is the half of `sys.rs` that needs no privileges.
#[test]
#[ignore = "requires a real audio endpoint"]
fn lists_real_render_endpoints() {
    let endpoints = SystemEndpoints::for_adapter(adapter())
        .list()
        .expect("enumerating render endpoints");

    assert!(
        !endpoints.is_empty(),
        "no render endpoints found; this machine has no audio output"
    );

    for endpoint in &endpoints {
        // WASAPI ids are of the form `{x.0.0.00000000}.{guid}`.
        assert!(
            endpoint.device_id.starts_with('{') && endpoint.device_id.contains("}.{"),
            "malformed endpoint id: {:?}",
            endpoint.device_id
        );
        assert!(
            !endpoint.friendly_name.is_empty(),
            "endpoint {} has no friendly name",
            endpoint.device_id
        );
    }
}

/// The rename path end-to-end: write `PKEY_Device_FriendlyName`, observe it
/// through a fresh enumeration, then restore the original name.
///
/// This is the only way to prove `IPropertyStore::SetValue` accepts the
/// hand-built `VT_LPWSTR` PROPVARIANT in `sys::lpwstr_propvariant` *and* that
/// the write actually persists, since `windows-rs` only offers a `VT_BSTR`
/// conversion.
///
/// # Environments where this cannot pass
///
/// Remote Desktop / Windows Sandbox sessions expose only *remote* endpoints,
/// whose ids start with a non-zero data-flow prefix (`{3.0.0.…}`) and which are
/// stored under `MMDevices\Audio\RemoteRender`. That key grants
/// `BUILTIN\Administrators` only `ReadKey` - `TrustedInstaller` alone has
/// `FullControl` - so `OpenPropertyStore(STGM_READWRITE)` returns
/// `E_ACCESSDENIED` no matter how elevated the caller is. Local endpoints under
/// `MMDevices\Audio\Render` *do* grant administrators `SetValue`, which is how
/// the Sound control panel renames devices.
#[test]
#[ignore = "requires elevation, a local (non-RDP) endpoint, and mutates its name"]
fn renames_an_endpoint_and_restores_it() {
    let endpoints = SystemEndpoints::for_adapter(adapter());
    let before = endpoints.list().expect("enumerating render endpoints");
    let target = before
        .iter()
        .find(|e| is_local_endpoint(&e.device_id))
        .unwrap_or_else(|| {
            panic!(
                "no local render endpoint to rename; found only {:?}. \
                 Remote Desktop / Sandbox endpoints live under MMDevices\\Audio\\RemoteRender, \
                 which is writable by TrustedInstaller only.",
                before.iter().map(|e| &e.device_id).collect::<Vec<_>>()
            )
        })
        .clone();

    let probe = format!("{} (sonos-driver test)", target.friendly_name);

    match endpoints.rename(&target.device_id, &probe) {
        Ok(()) => {}
        Err(Error::AccessDenied) => {
            panic!("run this test from an elevated shell: renaming an endpoint needs admin")
        }
        Err(other) => panic!("rename failed: {other}"),
    }

    // Read back first, restore second, so a failed assertion cannot leave the
    // machine renamed.
    let observed = endpoints
        .list()
        .expect("re-enumerating after rename")
        .into_iter()
        .find(|e| e.device_id == target.device_id)
        .map(|e| e.friendly_name);

    endpoints
        .rename(&target.device_id, &target.friendly_name)
        .expect("restoring the original endpoint name");

    assert_eq!(
        observed.as_deref(),
        Some(probe.as_str()),
        "the rename did not become visible through a fresh enumeration"
    );

    let after = endpoints
        .list()
        .expect("final enumeration")
        .into_iter()
        .find(|e| e.device_id == target.device_id)
        .expect("the endpoint survived the round-trip");
    assert_eq!(
        after.friendly_name, target.friendly_name,
        "the original name was not restored"
    );
}

/// WASAPI ids are `{<flow>.<role>.<index>.<n>}.{<guid>}`; the leading field is
/// `0` for local endpoints and non-zero for Remote Desktop ones.
fn is_local_endpoint(device_id: &str) -> bool {
    device_id.starts_with("{0.")
}
