//! The `unsafe` island: `MMDevice` / `IPropertyStore` COM calls.
//!
//! Everything here is a thin, linear wrapper over the Core Audio endpoint APIs.
//! All decision-making lives in [`crate::naming`], which is FFI-free and
//! therefore unit-testable without a driver installed.

use core::ffi::c_void;
use core::mem::ManuallyDrop;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_DeviceInterface_FriendlyName, PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, E_ACCESSDENIED, E_OUTOFMEMORY, PROPERTYKEY, RPC_E_CHANGED_MODE,
    WIN32_ERROR,
};
use windows::Win32::Media::Audio::{
    eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE,
    DEVICE_STATEMASK_ALL, DEVICE_STATE_ACTIVE, DEVICE_STATE_DISABLED,
};
use windows::Win32::System::Com::StructuredStorage::{
    PropVariantToStringAlloc, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemAlloc, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ, STGM_READWRITE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_DWORD,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use crate::{naming, Error, Result, VadEndpoint};

/// Wrap a `windows` error with the call that produced it.
///
/// `E_ACCESSDENIED` is promoted to [`Error::AccessDenied`] wherever it appears,
/// because "you are not elevated" is the one failure users actually hit and it
/// deserves an actionable message rather than an HRESULT.
fn win(call: &'static str, source: windows::core::Error) -> Error {
    if source.code() == E_ACCESSDENIED {
        return Error::AccessDenied;
    }
    Error::Windows {
        call,
        source: Box::new(source),
    }
}

/// Initialises COM (multithreaded apartment) for the calling thread and
/// uninitialises it on drop.
///
/// `RPC_E_CHANGED_MODE` means some other component already initialised COM on
/// this thread with a different apartment model; we happily reuse it but must
/// **not** balance it.
struct ComGuard {
    owns_init: bool,
}

impl ComGuard {
    fn new() -> Result<Self> {
        // SAFETY: the documented COM bootstrap. The reserved pointer is null and
        // every owned success is balanced by exactly one `CoUninitialize` in
        // `Drop`, on this same thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self { owns_init: false });
        }
        hr.ok().map_err(|e| win("CoInitializeEx", e))?;
        Ok(Self { owns_init: true })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owns_init {
            // SAFETY: balances the single successful `CoInitializeEx` in `new`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Owns a block allocated by COM and frees it with `CoTaskMemFree` on drop.
struct TaskMem(*const c_void);

impl Drop for TaskMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer was returned by a COM allocator and is freed
            // exactly once here; nothing retains it past this scope.
            unsafe { CoTaskMemFree(Some(self.0)) };
        }
    }
}

/// Copy a COM-allocated wide string into an owned `String` and free it.
fn take_wide(pwstr: PWSTR) -> String {
    let _mem = TaskMem(pwstr.0.cast::<c_void>());
    if pwstr.is_null() {
        return String::new();
    }
    // SAFETY: `pwstr` is a NUL-terminated wide string from a COM call, still
    // owned by `_mem` for the duration of this read.
    unsafe { pwstr.to_string() }.unwrap_or_default()
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for Win32 calls.
fn widen(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

fn create_enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: standard COM activation of the documented MMDeviceEnumerator
    // CLSID; COM is initialised on this thread by a live `ComGuard`.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| win("CoCreateInstance(MMDeviceEnumerator)", e))
}

/// Read an endpoint's stable id string.
fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: `GetId` allocates a NUL-terminated wide string we take ownership of.
    let pwstr = unsafe { device.GetId() }.map_err(|e| win("IMMDevice::GetId", e))?;
    Ok(take_wide(pwstr))
}

/// Read a string-valued property, or `""` when absent or of another type.
///
/// `PropVariantToStringAlloc` coerces every reasonable variant type to a string,
/// which keeps this tolerant of endpoints that publish `VT_BSTR` where the docs
/// promise `VT_LPWSTR`.
fn read_string(store: &IPropertyStore, key: &PROPERTYKEY) -> String {
    // SAFETY: `key` is a valid property key; the returned PROPVARIANT is owned
    // and released by its `Drop` impl (which calls `PropVariantClear`).
    let Ok(prop) = (unsafe { store.GetValue(key) }) else {
        return String::new();
    };
    // SAFETY: `prop` is a live, well-formed PROPVARIANT; on success the call
    // allocates a wide string we take ownership of.
    match unsafe { PropVariantToStringAlloc(&raw const prop) } {
        Ok(pwstr) => take_wide(pwstr),
        Err(_) => String::new(),
    }
}

/// Lists the render endpoints belonging to `adapter`.
///
/// An empty `adapter` disables the filter and lists every render endpoint.
pub(crate) fn list(adapter: &str) -> Result<Vec<VadEndpoint>> {
    let _com = ComGuard::new()?;
    let enumerator = create_enumerator()?;

    // Every state, not just active: a disabled or unplugged sonosvad endpoint
    // must still be reported so the caller can explain *why* a room is missing
    // instead of silently dropping it.
    // SAFETY: documented enumeration call with valid arguments; returns an owned
    // collection.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL)) }
            .map_err(|e| win("IMMDeviceEnumerator::EnumAudioEndpoints", e))?;

    // SAFETY: simple count read on a live collection.
    let count =
        unsafe { collection.GetCount() }.map_err(|e| win("IMMDeviceCollection::GetCount", e))?;

    let mut endpoints = Vec::new();
    for index in 0..count {
        // SAFETY: `index` is bounded by `GetCount`; returns an owned device.
        let device =
            unsafe { collection.Item(index) }.map_err(|e| win("IMMDeviceCollection::Item", e))?;

        // SAFETY: opens a read-only property store on a live device.
        let Ok(store) = (unsafe { device.OpenPropertyStore(STGM_READ) }) else {
            continue;
        };

        // The adapter filter is the safety belt that keeps this from ever
        // renaming the user's real sound card.
        let interface_name = read_string(&store, &PKEY_DeviceInterface_FriendlyName);
        if !adapter.is_empty() && !interface_name.eq_ignore_ascii_case(adapter) {
            continue;
        }

        let device_desc = read_string(&store, &PKEY_Device_DeviceDesc);
        let friendly_name = read_string(&store, &PKEY_Device_FriendlyName);

        // SAFETY: state read on a live device.
        let state = unsafe { device.GetState() }.unwrap_or(DEVICE_STATE(0));

        endpoints.push(VadEndpoint {
            device_id: device_id(&device)?,
            slot: naming::slot_from_factory_name(&device_desc),
            device_desc,
            adapter: interface_name,
            friendly_name,
            active: state == DEVICE_STATE_ACTIVE,
            disabled: state == DEVICE_STATE_DISABLED,
        });
    }

    // With the filter disabled there is nothing to diagnose: an empty machine
    // simply has no render endpoints, which is not a driver problem.
    if endpoints.is_empty() && !adapter.is_empty() {
        return Err(Error::DriverNotPresent {
            adapter: adapter.to_owned(),
        });
    }

    // Unknown slots sort last, so a malformed description never displaces a
    // well-formed one.
    endpoints.sort_by(|a, b| {
        a.slot
            .unwrap_or(u32::MAX)
            .cmp(&b.slot.unwrap_or(u32::MAX))
            .then_with(|| a.device_id.cmp(&b.device_id))
    });
    Ok(endpoints)
}

/// Build a `VT_LPWSTR` PROPVARIANT owning a COM-allocated copy of `value`.
///
/// The returned variant frees the string through its own `Drop`
/// (`PropVariantClear`), so there is exactly one owner and no manual cleanup on
/// any error path.
fn lpwstr_propvariant(value: &str) -> Result<PROPVARIANT> {
    let wide = widen(value);
    let bytes = core::mem::size_of_val(wide.as_slice());

    // SAFETY: allocates `bytes` from the COM task allocator; ownership is
    // transferred to the PROPVARIANT below, which frees it via PropVariantClear.
    let raw = unsafe { CoTaskMemAlloc(bytes) }.cast::<u16>();
    if raw.is_null() {
        return Err(Error::Windows {
            call: "CoTaskMemAlloc",
            source: Box::new(windows::core::Error::from(E_OUTOFMEMORY)),
        });
    }
    // SAFETY: `raw` points at `bytes` freshly allocated, non-overlapping bytes,
    // and `wide` is exactly that long.
    unsafe { core::ptr::copy_nonoverlapping(wide.as_ptr(), raw, wide.len()) };

    Ok(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    pwszVal: PWSTR(raw),
                },
            }),
        },
    })
}

/// Sets the sound-picker name of `device_id`.
pub(crate) fn rename(device_id: &str, name: &str) -> Result<()> {
    let _com = ComGuard::new()?;
    let enumerator = create_enumerator()?;

    let wide_id = widen(device_id);
    // SAFETY: `wide_id` is a NUL-terminated wide string alive for the call.
    let device = unsafe { enumerator.GetDevice(PCWSTR(wide_id.as_ptr())) }
        .map_err(|_| Error::NotFound(device_id.to_owned()))?;

    // Opening the store read/write is the step that requires elevation.
    // SAFETY: live device; `STGM_READWRITE` is the documented mode for writing
    // endpoint properties.
    let store = unsafe { device.OpenPropertyStore(STGM_READWRITE) }
        .map_err(|e| win("IMMDevice::OpenPropertyStore(STGM_READWRITE)", e))?;

    let prop = lpwstr_propvariant(name)?;
    let key = PKEY_Device_FriendlyName;

    // SAFETY: `key` is a valid property key and `prop` is a well-formed
    // `VT_LPWSTR` variant; both outlive the call. `SetValue` copies the value,
    // so ownership of our allocation stays with `prop`.
    unsafe { store.SetValue(&raw const key, &raw const prop) }
        .map_err(|e| win("IPropertyStore::SetValue(PKEY_Device_FriendlyName)", e))?;

    // SAFETY: live store; flushes the pending write to the MMDevices registry key.
    unsafe { store.Commit() }.map_err(|e| win("IPropertyStore::Commit", e))?;

    Ok(())
}

/// The `MMDevices` key holding per-endpoint state, relative to `HKEY_LOCAL_MACHINE`.
const MMDEVICES_RENDER: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render";

/// Registry value holding an endpoint's `DEVICE_STATE_XXX`.
const DEVICE_STATE_VALUE: &str = "DeviceState";

/// The registry subkey name for `device_id`.
///
/// An endpoint id is `{0.0.0.00000000}.{guid}`: a container-class GUID, a dot,
/// then the endpoint GUID. Only the second half names the `MMDevices` subkey.
pub(crate) fn endpoint_subkey(device_id: &str) -> Option<&str> {
    let rest = device_id.split_once("}.").map(|(_, rest)| rest)?;
    // Reject anything that is not a brace-delimited GUID, so a malformed id can
    // never be concatenated into a registry path.
    (rest.starts_with('{') && rest.ends_with('}') && !rest[1..].contains('{')).then_some(rest)
}

/// Show or hide `device_id` in the Windows sound UI.
///
/// Writes `DeviceState` under the endpoint's `MMDevices` key: `1`
/// (`DEVICE_STATE_ACTIVE`) or `2` (`DEVICE_STATE_DISABLED`). This is the same
/// state the Sound control panel's Enable/Disable toggles, and the same one
/// `IMMDeviceEnumerator` filters on, so a disabled endpoint disappears from
/// every picker and from app enumeration.
///
/// Done through the registry rather than `IPropertyStore` because endpoint
/// *state* is not an endpoint property: the only API that sets it is the
/// undocumented `IPolicyConfig`, whose vtable has changed between Windows
/// releases. The registry value is stable, and audiosrv reads it when it builds
/// its endpoint list.
///
/// Needs elevation, and takes effect when the audio service next reloads the
/// endpoint -- immediately for a device that is about to be re-enumerated,
/// otherwise at the next audiosrv restart.
pub(crate) fn set_visible(device_id: &str, visible: bool) -> Result<()> {
    let subkey = endpoint_subkey(device_id).ok_or_else(|| Error::NotFound(device_id.to_owned()))?;
    let path = widen(&format!(r"{MMDEVICES_RENDER}\{subkey}"));
    let value_name = widen(DEVICE_STATE_VALUE);
    let state: u32 = if visible {
        DEVICE_STATE_ACTIVE.0
    } else {
        DEVICE_STATE_DISABLED.0
    };

    let mut key = HKEY::default();
    // SAFETY: `path` is a NUL-terminated wide string alive for the call, and
    // `key` is a valid out-pointer. The handle is closed below on every path.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            None,
            KEY_SET_VALUE,
            &raw mut key,
        )
    };
    if status.is_err() {
        return Err(registry_error("RegOpenKeyExW(MMDevices endpoint)", status));
    }

    let bytes = state.to_ne_bytes();
    // SAFETY: `key` is open with KEY_SET_VALUE, `value_name` is NUL-terminated
    // and alive for the call, and `bytes` is exactly the four bytes a REG_DWORD
    // requires.
    let status = unsafe {
        RegSetValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            REG_DWORD,
            Some(&bytes),
        )
    };
    // SAFETY: `key` came from a successful `RegOpenKeyExW` and is not used again.
    let _ = unsafe { RegCloseKey(key) };

    if status.is_err() {
        return Err(registry_error("RegSetValueExW(DeviceState)", status));
    }
    Ok(())
}

/// Wrap a registry `WIN32_ERROR`, promoting access denied like [`win`] does.
fn registry_error(call: &'static str, status: WIN32_ERROR) -> Error {
    if status == ERROR_ACCESS_DENIED {
        return Error::AccessDenied;
    }
    Error::Windows {
        call,
        source: Box::new(windows::core::Error::from(status.to_hresult())),
    }
}
#[cfg(test)]
mod tests {
    use super::{endpoint_subkey, lpwstr_propvariant, take_wide, widen};
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Variant::VT_LPWSTR;

    /// `IPropertyStore::SetValue` rejects a malformed variant with a bare
    /// `E_INVALIDARG`, and a wrongly-laid-out union corrupts memory instead of
    /// failing. `windows-rs` only offers `From<&str>` producing `VT_BSTR`, so
    /// the `VT_LPWSTR` variant is hand-built - which makes this round-trip the
    /// only cheap guard against the union layout drifting under us.
    #[test]
    fn builds_a_readable_vt_lpwstr_variant() {
        for value in ["Sonos Living Room", "", "Ünïcödé Küche \u{1F50A}"] {
            let prop = lpwstr_propvariant(value).expect("allocating the variant");

            // SAFETY: `prop` is a live variant built above and outlives the call.
            let vt = unsafe { prop.Anonymous.Anonymous.vt };
            assert_eq!(vt, VT_LPWSTR, "wrong variant type for {value:?}");

            // SAFETY: `prop` is well-formed; the call allocates a wide string we
            // take ownership of via `take_wide`.
            let read_back = unsafe { PropVariantToStringAlloc(&raw const prop) }
                .map(take_wide)
                .expect("coercing the variant back to a string");
            assert_eq!(read_back, value);
            // `prop` drops here: `PropVariantClear` frees the CoTaskMemAlloc'd
            // buffer exactly once. A double free would fault the test process.
        }
    }

    #[test]
    fn widen_is_nul_terminated() {
        assert_eq!(widen(""), vec![0]);
        assert_eq!(widen("ab"), vec![u16::from(b'a'), u16::from(b'b'), 0]);
    }

    #[test]
    fn endpoint_subkey_takes_the_endpoint_guid_only() {
        assert_eq!(
            endpoint_subkey("{0.0.0.00000000}.{a1b2c3d4-0000-1111-2222-333344445555}"),
            Some("{a1b2c3d4-0000-1111-2222-333344445555}")
        );
    }

    #[test]
    fn endpoint_subkey_rejects_ids_that_could_escape_the_key() {
        // Nothing here may ever be pasted into a registry path.
        for bad in [
            "",
            "not-an-endpoint-id",
            "{0.0.0.00000000}",
            r"{0.0.0.00000000}.{guid}\..\..\Capture",
            "{0.0.0.00000000}.{guid}{extra}",
            "{0.0.0.00000000}.guid",
        ] {
            assert_eq!(endpoint_subkey(bad), None, "accepted {bad:?}");
        }
    }
}
