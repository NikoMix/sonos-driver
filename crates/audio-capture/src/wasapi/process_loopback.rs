//! Per-application WASAPI capture via `ActivateAudioInterfaceAsync`.
//!
//! This brings up an `IAudioClient` bound to the *process-loopback* virtual
//! device, so audio is captured from (or excluded for) a single target process
//! tree rather than the whole endpoint. Activation is asynchronous: a COM
//! completion handler signals an event, the activating thread waits for it, then
//! reads the activated interface. Once initialised, the session is drained by the
//! shared [`pump`](super::pump) exactly like the default-device path.
//!
//! Process loopback requires Windows 11 / Windows Server 2022 (build 20348+); the
//! caller gates on [`crate::process::meets_min_build`] before reaching here.

use core::mem::ManuallyDrop;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use audio_core::{AudioFormat, Error, Result};
use windows::core::{IUnknown, Interface, HRESULT};
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IAudioCaptureClient, IAudioClient,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::System::Variant::VT_BLOB;

use super::pump::{self, ClientSession};
use super::{com, Outcome};
use crate::capture::{CaptureItem, SharedState};
use crate::format::WaveFormat;
use crate::process::ProcessLoopbackMode;

/// `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`: let the audio engine resample/convert to
/// our requested PCM format. Defined locally so the value is independent of which
/// `windows` feature happens to surface the constant.
const AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM: u32 = 0x8000_0000;

/// `WAVE_FORMAT_PCM`. Defined locally to avoid namespace/feature ambiguity.
const WAVE_FORMAT_PCM_TAG: u16 = 1;

/// How long to wait for `ActivateCompleted` before giving up. Activation is
/// effectively immediate; this only bounds a pathological hang.
const ACTIVATION_TIMEOUT_MS: u32 = 5_000;

/// COM completion handler that signals an event when activation completes.
///
/// The `#[implement]` macro emits helper impls (`From`, `as_interface_ref`,
/// `as_impl_ptr`, …) that trip the pedantic `clippy::ref_as_ptr` and
/// `clippy::inline_always` lints. Wrapping the type in this private module keeps
/// those `#[allow]`s scoped to the generated code, so the rest of the FFI island
/// retains the crate's strict lint profile.
mod handler {
    #![allow(clippy::ref_as_ptr, clippy::inline_always)]

    use windows::core::{implement, Ref};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Media::Audio::{
        IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
        IActivateAudioInterfaceCompletionHandler_Impl,
    };
    use windows::Win32::System::Threading::SetEvent;

    /// COM completion handler: signals `event` when activation completes.
    #[implement(IActivateAudioInterfaceCompletionHandler)]
    pub(super) struct ActivationHandler {
        /// The auto-reset event the activating thread waits on.
        pub(super) event: HANDLE,
    }

    impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
        fn ActivateCompleted(
            &self,
            _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            // SAFETY: `self.event` is a valid auto-reset event owned by the
            // activating thread for the lifetime of this handler; signalling it
            // hands control back to that thread.
            unsafe {
                let _ = SetEvent(self.event);
            }
            Ok(())
        }
    }
}

use handler::ActivationHandler;

/// Build the activation params describing the target process and capture mode.
fn build_activation_params(pid: u32, mode: ProcessLoopbackMode) -> AUDIOCLIENT_ACTIVATION_PARAMS {
    let loopback_mode = match mode {
        ProcessLoopbackMode::IncludeTree => PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        ProcessLoopbackMode::ExcludeTree => PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    };
    AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: loopback_mode,
            },
        },
    }
}

/// The explicit capture format (process loopback forbids `GetMixFormat`): 16-bit
/// PCM, 48 kHz, stereo. Returns both the Win32 and decoded representations.
fn capture_wave_format() -> (WAVEFORMATEX, WaveFormat) {
    let channels: u16 = 2;
    let sample_rate: u32 = 48_000;
    let bits: u16 = 16;
    let block_align: u16 = channels * (bits / 8);
    let avg_bytes: u32 = sample_rate * u32::from(block_align);
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM_TAG,
        nChannels: channels,
        nSamplesPerSec: sample_rate,
        nAvgBytesPerSec: avg_bytes,
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    };
    let wave = WaveFormat {
        sample_rate,
        channels,
        container_bits: bits,
        valid_bits: bits,
        is_float: false,
    };
    (format, wave)
}

/// The canonical [`AudioFormat`] a process-loopback source reports.
///
/// # Errors
///
/// Returns [`Error::InvalidFormat`] only if the fixed 16-bit PCM layout cannot be
/// represented (it always can; this mirrors the device path's fallibility).
pub(super) fn reported_format() -> Result<AudioFormat> {
    capture_wave_format().1.to_audio_format()
}

/// Asynchronously activate the process-loopback client and wait for the result.
fn activate_process_client(pid: u32, mode: ProcessLoopbackMode) -> Result<IAudioClient> {
    let params = build_activation_params(pid, mode);
    let blob = BLOB {
        cbSize: u32::try_from(core::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>())
            .map_err(|_| Error::capture("activation params size does not fit in u32"))?,
        pBlobData: (&raw const params).cast::<u8>().cast_mut(),
    };

    // `ManuallyDrop` keeps WASAPI from ever running `PropVariantClear` on this
    // variant, whose blob borrows the stack-owned `params` (there is nothing to
    // free, and freeing a stack pointer would be undefined behaviour).
    let mut prop = ManuallyDrop::new(PROPVARIANT::default());
    // SAFETY: initialise the variant as a `VT_BLOB` pointing at `params`; the
    // union fields are written, never read uninitialised.
    unsafe {
        let inner = &mut *prop.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = blob;
    }

    let event = com::create_event()?;
    let handler: IActivateAudioInterfaceCompletionHandler =
        ActivationHandler { event: event.raw() }.into();

    // SAFETY: standard async activation of the process-loopback virtual device.
    // `prop` (and thus `params`) stays alive until after the wait below, so the
    // borrowed blob is valid until `ActivateCompleted` has fired.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *prop),
            &handler,
        )
    }
    .map_err(|e| super::win_err("ActivateAudioInterfaceAsync", &e))?;

    // SAFETY: waits on our valid event with a finite timeout.
    let wait = unsafe { WaitForSingleObject(event.raw(), ACTIVATION_TIMEOUT_MS) };
    if wait != WAIT_OBJECT_0 {
        return Err(Error::capture(
            "timed out waiting for process-loopback activation to complete",
        ));
    }

    get_activate_result(&operation)
}

/// Read the activated interface from a completed activation operation.
fn get_activate_result(operation: &IActivateAudioInterfaceAsyncOperation) -> Result<IAudioClient> {
    let mut activate_hr = HRESULT::default();
    let mut activated: Option<IUnknown> = None;
    // SAFETY: both out-pointers are valid locals; the call populates them.
    unsafe { operation.GetActivateResult(&raw mut activate_hr, &raw mut activated) }
        .map_err(|e| super::win_err("GetActivateResult", &e))?;

    // On pre-20348 builds this is where `E_NOTIMPL` surfaces; the build gate
    // upstream should prevent reaching here, but report it cleanly regardless.
    activate_hr
        .ok()
        .map_err(|e| super::win_err("process-loopback activation result", &e))?;

    let activated =
        activated.ok_or_else(|| Error::capture("activation returned a null interface"))?;
    activated
        .cast::<IAudioClient>()
        .map_err(|e| super::win_err("cast activated interface to IAudioClient", &e))
}

/// Activate, initialise and start a process-loopback capture session.
fn start_process_session(
    pid: u32,
    mode: ProcessLoopbackMode,
    shared: &Arc<SharedState>,
) -> Result<ClientSession> {
    let com = com::ComGuard::new()?;
    let client = activate_process_client(pid, mode)?;
    let (format, wave) = capture_wave_format();

    // SAFETY: shared-mode, event-driven loopback init with an explicit PCM format;
    // `AUTOCONVERTPCM` lets the engine convert from the target's native format.
    // Buffer duration and periodicity are 0 per the process-loopback contract
    // (the virtual device has no queryable device period).
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            0,
            0,
            &raw const format,
            None,
        )
    }
    .map_err(|e| super::win_err("IAudioClient::Initialize(process loopback)", &e))?;

    shared.set_format(wave.to_audio_format()?);
    tracing::debug!(
        pid,
        sample_rate = wave.sample_rate,
        channels = wave.channels,
        "process-loopback session started"
    );

    let event = com::create_event()?;
    // SAFETY: associates our auto-reset event with the initialised client.
    unsafe { client.SetEventHandle(event.raw()) }
        .map_err(|e| super::win_err("SetEventHandle", &e))?;

    // SAFETY: retrieves the capture service from the initialised loopback client.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }
        .map_err(|e| super::win_err("GetService(IAudioCaptureClient)", &e))?;

    // SAFETY: starts the stream; buffer events begin once the target emits audio.
    unsafe { client.Start() }.map_err(|e| super::win_err("Start", &e))?;

    Ok(ClientSession {
        client,
        capture,
        event,
        wave,
        _com: com,
    })
}

/// Process-loopback capture worker entry point.
///
/// Unlike the default-device path there is no reconnect loop: a single target app
/// has exactly one session, so when it exits (the stream is invalidated) the
/// stream ends gracefully rather than retrying.
pub(super) fn run_process_capture(
    pid: u32,
    mode: ProcessLoopbackMode,
    shared: &Arc<SharedState>,
    tx: &SyncSender<CaptureItem>,
) {
    pump::set_time_critical_priority();
    match start_process_session(pid, mode, shared) {
        Ok(session) => match pump::pump(&session, shared, tx) {
            Ok(Outcome::Stop) => {}
            Ok(Outcome::Reconnect) => {
                tracing::info!(pid, "process-loopback target ended; stopping capture");
            }
            Err(err) => {
                tracing::error!(pid, error = %err, "process-loopback capture failed");
                let _ = tx.try_send(Err(err));
            }
        },
        Err(err) => {
            tracing::error!(pid, error = %err, "failed to start process-loopback capture");
            let _ = tx.try_send(Err(err));
        }
    }
    tracing::debug!(pid, "process-loopback worker exiting");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_params_carry_pid_and_include_mode() {
        let params = build_activation_params(1234, ProcessLoopbackMode::IncludeTree);
        assert_eq!(
            params.ActivationType,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
        );
        // SAFETY: we just initialised the `ProcessLoopbackParams` union variant.
        let lp = unsafe { params.Anonymous.ProcessLoopbackParams };
        assert_eq!(lp.TargetProcessId, 1234);
        assert_eq!(
            lp.ProcessLoopbackMode,
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
        );
    }

    #[test]
    fn activation_params_exclude_mode() {
        let params = build_activation_params(7, ProcessLoopbackMode::ExcludeTree);
        // SAFETY: we just initialised the `ProcessLoopbackParams` union variant.
        let lp = unsafe { params.Anonymous.ProcessLoopbackParams };
        assert_eq!(lp.TargetProcessId, 7);
        assert_eq!(
            lp.ProcessLoopbackMode,
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
        );
    }

    #[test]
    fn capture_format_is_16bit_pcm_stereo_48k() {
        let (fmt, wave) = capture_wave_format();
        // `WAVEFORMATEX` is `#[repr(packed)]`; copy each field into an aligned
        // local before comparing (taking a reference to a packed field is UB).
        let format_tag = fmt.wFormatTag;
        let channels = fmt.nChannels;
        let samples_per_sec = fmt.nSamplesPerSec;
        let bits_per_sample = fmt.wBitsPerSample;
        let block_align = fmt.nBlockAlign;
        let avg_bytes_per_sec = fmt.nAvgBytesPerSec;
        assert_eq!(format_tag, WAVE_FORMAT_PCM_TAG);
        assert_eq!(channels, 2);
        assert_eq!(samples_per_sec, 48_000);
        assert_eq!(bits_per_sample, 16);
        assert_eq!(block_align, 4);
        assert_eq!(avg_bytes_per_sec, 192_000);
        assert!(!wave.is_float);
        assert_eq!(wave.container_bits, 16);
        assert_eq!(wave.channels, 2);
    }

    #[test]
    fn blob_describes_the_activation_params_struct() {
        let size = u32::try_from(core::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>())
            .expect("params fit in u32");
        let blob = BLOB {
            cbSize: size,
            pBlobData: core::ptr::null_mut(),
        };
        assert_eq!(blob.cbSize, size);
        assert!(size >= 8); // at minimum the activation type plus the pid.
    }

    #[test]
    fn reported_format_is_canonical_f32_48k_stereo() {
        let fmt = reported_format().expect("16-bit PCM is representable");
        assert_eq!(fmt.sample_rate, 48_000);
        assert_eq!(fmt.channels, 2);
    }
}
