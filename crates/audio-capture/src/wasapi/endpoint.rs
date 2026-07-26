//! Endpoint enumeration, device resolution and mix-format extraction.
//!
//! These are the Core Audio "discovery" calls: turning the `IMMDeviceEnumerator`
//! into a list of [`EndpointInfo`], resolving a chosen id back to an
//! `IMMDevice`, and reading an endpoint's mix format into the pure
//! [`WaveFormat`] model.

use core::ffi::c_void;

use audio_core::{AudioFormat, Error, Result};
use windows::core::PWSTR;
use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_DeviceInterface_FriendlyName, PKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, StructuredStorage::PropVariantClear, CLSCTX_ALL, STGM_READ,
};
use windows::Win32::System::Variant::VT_LPWSTR;

use super::com::TaskMem;
use crate::device::EndpointInfo;
use crate::format::WaveFormat;

/// `WAVE_FORMAT_IEEE_FLOAT` tag / subformat `Data1`.
const WAVE_FORMAT_IEEE_FLOAT: u32 = 0x0003;
/// `WAVE_FORMAT_EXTENSIBLE` tag.
const WAVE_FORMAT_EXTENSIBLE: u32 = 0xFFFE;

/// Create the process-wide `IMMDeviceEnumerator`.
pub(super) fn create_enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: standard COM activation of the documented enumerator CLSID; COM is
    // initialised on this thread by a `ComGuard`. Returns the interface or errors.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| super::win_err("CoCreateInstance(MMDeviceEnumerator)", &e))
}

/// Enumerate active render endpoints, flagging the current default.
pub(super) fn enumerate(enumerator: &IMMDeviceEnumerator) -> Result<Vec<EndpointInfo>> {
    let default_id = default_render_id(enumerator);

    // SAFETY: `eRender` + `DEVICE_STATE_ACTIVE` are valid arguments; the returned
    // collection is owned and released when dropped.
    let collection: IMMDeviceCollection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
            .map_err(|e| super::win_err("EnumAudioEndpoints", &e))?;

    // SAFETY: simple count read on a live collection.
    let count = unsafe { collection.GetCount() }.map_err(|e| super::win_err("GetCount", &e))?;

    let mut endpoints = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: `index` is bounded by `GetCount`; returns an owned device.
        let device = unsafe { collection.Item(index) }.map_err(|e| super::win_err("Item", &e))?;
        let id = device_id(&device)?;
        let name = friendly_name(&device).unwrap_or_else(|_| id.clone());
        let adapter = adapter_name(&device);
        let is_default = default_id.as_deref() == Some(id.as_str());
        endpoints.push(EndpointInfo::new(id, name, adapter, is_default));
    }
    Ok(endpoints)
}

/// Resolve a previously enumerated endpoint id back to an `IMMDevice`.
pub(super) fn device_by_id(enumerator: &IMMDeviceEnumerator, id: &str) -> Result<IMMDevice> {
    let wide = widen(id);
    // SAFETY: `wide` is a NUL-terminated wide string valid for the call; returns
    // an owned device or an error if the id is no longer present.
    unsafe { enumerator.GetDevice(windows::core::PCWSTR(wide.as_ptr())) }
        .map_err(|e| super::win_err("GetDevice", &e))
}

/// Read an endpoint's current mix format as canonical [`AudioFormat`].
pub(super) fn probe_format(device: &IMMDevice) -> Result<AudioFormat> {
    // SAFETY: activates the audio client interface on a live device.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| super::win_err("IMMDevice::Activate(IAudioClient)", &e))?;
    mix_format(&client)?.to_audio_format()
}

/// Read the client's mix format into the pure [`WaveFormat`] model.
pub(super) fn mix_format(client: &IAudioClient) -> Result<WaveFormat> {
    // SAFETY: `GetMixFormat` allocates a `WAVEFORMATEX` we own; `TaskMem` frees it.
    let ptr = unsafe { client.GetMixFormat() }.map_err(|e| super::win_err("GetMixFormat", &e))?;
    let _mem = TaskMem::new(ptr.cast::<c_void>());
    if ptr.is_null() {
        return Err(Error::capture("GetMixFormat returned a null format"));
    }
    Ok(wave_format_from_ptr(ptr))
}

/// Decode a `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` into [`WaveFormat`].
///
/// # Caller contract
///
/// `ptr` must be a valid, non-null pointer to a `WAVEFORMATEX` whose `cbSize`
/// honestly describes any extensible trailer — exactly what WASAPI returns from
/// `GetMixFormat`. The pointer only needs to be valid for the duration of the
/// call.
pub(super) fn wave_format_from_ptr(ptr: *const WAVEFORMATEX) -> WaveFormat {
    // SAFETY: per the caller contract `ptr` references a valid `WAVEFORMATEX`.
    let base = unsafe { &*ptr };
    let tag = u32::from(base.wFormatTag);

    let (valid_bits, is_float) = if tag == WAVE_FORMAT_EXTENSIBLE {
        // SAFETY: an extensible tag guarantees the struct is actually a
        // `WAVEFORMATEXTENSIBLE`, so the wider read is in bounds.
        let ext = unsafe { &*ptr.cast::<WAVEFORMATEXTENSIBLE>() };
        // SAFETY: union field read; for a mix format `wValidBitsPerSample` is the
        // active member.
        let valid = unsafe { ext.Samples.wValidBitsPerSample };
        (valid, ext.SubFormat.data1 == WAVE_FORMAT_IEEE_FLOAT)
    } else {
        (base.wBitsPerSample, tag == WAVE_FORMAT_IEEE_FLOAT)
    };

    WaveFormat {
        sample_rate: base.nSamplesPerSec,
        channels: base.nChannels,
        container_bits: base.wBitsPerSample,
        valid_bits,
        is_float,
    }
}

/// The default render endpoint's id, if any.
fn default_render_id(enumerator: &IMMDeviceEnumerator) -> Option<String> {
    // SAFETY: valid data-flow/role arguments; returns an owned device or errors
    // when no default exists (e.g. no audio hardware).
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.ok()?;
    device_id(&device).ok()
}

/// Read an endpoint's stable id string.
fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: `GetId` allocates a NUL-terminated wide string we own and free.
    let pwstr: PWSTR = unsafe { device.GetId() }.map_err(|e| super::win_err("GetId", &e))?;
    let _mem = TaskMem::new(pwstr.0.cast::<c_void>());
    // SAFETY: `pwstr` is a valid NUL-terminated wide string from `GetId`.
    unsafe { pwstr.to_string() }
        .map_err(|e| Error::capture(format!("device id was not UTF-16: {e}")))
}

/// Read an endpoint's friendly name via its property store.
fn friendly_name(device: &IMMDevice) -> Result<String> {
    string_property(device, &PKEY_Device_FriendlyName, "FriendlyName")
        .ok_or_else(|| Error::capture("endpoint has no friendly name"))
}

/// Read the adapter (device interface) name this endpoint belongs to.
///
/// Returns an empty string when unavailable. A missing adapter name is not worth
/// failing enumeration over — it is used to recognise our own driver's endpoints,
/// and "unknown" is correctly handled as "not ours".
///
/// The value is trimmed because Windows really does report blank-but-present
/// names: the Remote Desktop "Remote Audio" endpoint reports a single space.
/// Left untrimmed that reads as a known adapter called `" "`, which would defeat
/// the emptiness checks downstream and show as `[ ]` in any UI.
fn adapter_name(device: &IMMDevice) -> String {
    string_property(
        device,
        &PKEY_DeviceInterface_FriendlyName,
        "DeviceInterface_FriendlyName",
    )
    .map(|name| name.trim().to_owned())
    .unwrap_or_default()
}

/// Read one string property from a device's property store.
fn string_property(device: &IMMDevice, key: &PROPERTYKEY, label: &str) -> Option<String> {
    // SAFETY: opens a read-only property store on a live device.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    // SAFETY: `key` is a valid property key; returns an owned PROPVARIANT we
    // clear below.
    let mut prop = match unsafe { store.GetValue(key) } {
        Ok(prop) => prop,
        Err(err) => {
            tracing::debug!(%err, property = label, "endpoint property is unreadable");
            return None;
        }
    };

    // SAFETY: reading the PROPVARIANT discriminant and, when it is `VT_LPWSTR`,
    // its wide-string member; both are the active union fields for this key.
    let value = unsafe {
        if prop.Anonymous.Anonymous.vt == VT_LPWSTR {
            let pwsz = prop.Anonymous.Anonymous.Anonymous.pwszVal;
            if pwsz.is_null() {
                None
            } else {
                pwsz.to_string().ok()
            }
        } else {
            None
        }
    };

    // SAFETY: clears the owned PROPVARIANT exactly once, freeing its string.
    unsafe {
        let _ = PropVariantClear(&raw mut prop);
    }

    value
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for Win32 calls.
fn widen(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
