//! Whether the kernel-driver mode is present on this machine, and what that
//! means for this app.
//!
//! The two modes can be installed side by side, and when they are, they overlap
//! in a way that is invisible from the UI: `sonosvad.sys` publishes render
//! endpoints, `sonosvad-svc` renames them to room names and streams whatever
//! Windows plays into them, and this app sees those endpoints as ordinary
//! outputs it could route. Route one and the same audio reaches the same room
//! twice, from two processes that each believe they own it.
//!
//! Telling them apart needs the **adapter** behind the endpoint
//! ([`Output::is_sonos_virtual`]), because the endpoint *name* is whatever the
//! service or the user last set it to. Knowing whether that overlap actually
//! matters needs one more fact — whether the service is running — which is what
//! this module goes and asks the Service Control Manager.
//!
//! Everything except [`probe_service`] is pure and testable; the probe is the
//! single Windows call, and it degrades to [`VadService::Unknown`] rather than
//! failing, because not knowing is never a reason to stop routing audio.

use crate::state::Output;

/// The name the driver's companion service registers under.
///
/// Duplicated from `sonos-vad-service`'s `SERVICE_NAME` rather than imported:
/// ADR-0019 keeps the driverless app free of the kernel-driver crates, and this
/// is one short string on the far side of that boundary. Note that it is
/// `sonosvad`, not `sonosvad-svc` — the latter is the name of the executable.
pub const VAD_SERVICE_NAME: &str = "sonosvad";

/// What the `sonosvad` service is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VadService {
    /// Installed and running: it owns the driver's endpoints and is streaming
    /// them to Sonos itself.
    Running,
    /// Installed but not running, so the driver's endpoints go nowhere.
    Stopped,
    /// No such service: the kernel-driver mode is not installed. The ordinary
    /// case, and not a problem — this app never needed it.
    NotInstalled,
    /// The service could not be queried. Assume the cautious interpretation.
    #[default]
    Unknown,
}

impl VadService {
    /// Whether the service might currently be streaming the driver's endpoints.
    ///
    /// [`Unknown`](Self::Unknown) counts as "might": a warning that turns out to
    /// be unnecessary costs a line of text, while a missing one costs the user
    /// a doubled stream they have to diagnose themselves.
    #[must_use]
    pub const fn may_be_streaming(self) -> bool {
        matches!(self, Self::Running | Self::Unknown)
    }

    /// Whether the kernel-driver mode's user-mode half is installed at all.
    #[must_use]
    pub const fn is_installed(self) -> bool {
        matches!(self, Self::Running | Self::Stopped)
    }
}

/// What this app can see of the kernel-driver mode.
///
/// Assembled from two independent signals, which is what makes it useful: the
/// endpoints say whether the *driver* is loaded, and the service says whether
/// the *user-mode half* is running. Either one alone can mislead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DriverStatus {
    /// How many enumerated outputs came from the `sonosvad` adapter.
    pub endpoints: usize,
    /// What the companion service is doing.
    pub service: VadService,
}

impl DriverStatus {
    /// Read the driver's presence off the enumerated outputs.
    #[must_use]
    pub fn new(outputs: &[Output], service: VadService) -> Self {
        Self {
            endpoints: outputs
                .iter()
                .filter(|output| output.is_sonos_virtual())
                .count(),
            service,
        }
    }

    /// Whether `sonosvad.sys` is loaded and publishing endpoints.
    ///
    /// Endpoints are the only proof available from user mode without touching
    /// the driver: if they enumerate, the driver started.
    #[must_use]
    pub const fn is_driver_loaded(&self) -> bool {
        self.endpoints > 0
    }

    /// A sentence about the kernel-driver mode, when there is one worth saying.
    ///
    /// Deliberately silent in the two consistent cases — nothing installed
    /// (the overwhelmingly common one, where mentioning it would be noise), and
    /// everything installed and running. It speaks up only when the two halves
    /// disagree, because that is a state the user cannot diagnose by looking:
    /// the endpoints are either missing or silent, and nothing in the Windows
    /// sound picker explains why.
    #[must_use]
    pub fn advice(&self) -> Option<String> {
        match (self.is_driver_loaded(), self.service) {
            // The driver is loaded but nothing is driving it: its endpoints
            // appear in the sound picker under generic names and silently
            // discard audio. Routing them here is a legitimate way out, so
            // offer it alongside starting the service.
            (true, VadService::Stopped | VadService::NotInstalled) => Some(format!(
                "{} kernel-driver output(s) are present, but the sonosvad service is {}, so audio \
                 sent to them goes nowhere. Start it with `Start-Service {VAD_SERVICE_NAME}` (as \
                 administrator), or route those outputs here instead.",
                self.endpoints,
                if self.service == VadService::Stopped {
                    "stopped"
                } else {
                    "not installed"
                }
            )),
            // The service is installed but has no endpoints to work with. Very
            // likely test signing: the driver installs happily and then never
            // starts, and Windows reports that nowhere the user will look.
            (false, VadService::Running | VadService::Stopped) => Some(
                "The sonosvad service is installed but the driver is not loaded, so no per-room \
                 outputs exist. Test signing is usually the reason — run \
                 `driver\\scripts\\Setup-Driver.ps1` to check. This app does not need the driver \
                 and works regardless."
                    .to_owned(),
            ),
            _ => None,
        }
    }
}

/// Ask the Service Control Manager what the `sonosvad` service is doing.
///
/// Blocking, so call it from a background thread. Querying status needs no
/// elevation, so this works from the app as an ordinary user.
#[cfg(windows)]
#[must_use]
pub fn probe_service() -> VadService {
    probe_named(VAD_SERVICE_NAME)
}

/// The body of [`probe_service`], with the service name as a parameter.
///
/// Split out purely so the Windows path can be tested: no machine has the
/// `sonosvad` service during a normal test run, so probing it could only ever
/// prove the "not installed" branch. Against a name that *does* exist, the open
/// and query branches get exercised too.
#[cfg(windows)]
fn probe_named(name: &str) -> VadService {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    /// `ERROR_SERVICE_DOES_NOT_EXIST` — the service is simply not installed,
    /// which is a normal answer rather than a failure.
    const SERVICE_DOES_NOT_EXIST: i32 = 1060;

    let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    {
        Ok(manager) => manager,
        Err(err) => {
            tracing::debug!(%err, "could not connect to the service control manager");
            return VadService::Unknown;
        }
    };

    let service = match manager.open_service(name, ServiceAccess::QUERY_STATUS) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(err))
            if err.raw_os_error() == Some(SERVICE_DOES_NOT_EXIST) =>
        {
            return VadService::NotInstalled;
        }
        Err(err) => {
            tracing::debug!(%err, service = name, "could not open the service");
            return VadService::Unknown;
        }
    };

    match service.query_status() {
        Ok(status) => match status.current_state {
            // Both pending states count as running: the service is on its way
            // somewhere, and warning about a stream that is about to exist is
            // better than staying quiet until it does.
            windows_service::service::ServiceState::Running
            | windows_service::service::ServiceState::StartPending
            | windows_service::service::ServiceState::ContinuePending => VadService::Running,
            // Stopped, stopping, paused or pausing: installed, but not moving
            // audio, which for this app's purposes is the same thing.
            _ => VadService::Stopped,
        },
        Err(err) => {
            tracing::debug!(%err, "could not query the sonosvad service status");
            VadService::Unknown
        }
    }
}

/// Non-Windows builds have no Service Control Manager to ask.
#[cfg(not(windows))]
#[must_use]
pub fn probe_service() -> VadService {
    VadService::NotInstalled
}

#[cfg(test)]
mod tests {
    use super::{DriverStatus, VadService};
    use crate::state::{Output, SONOSVAD_ADAPTER};

    fn outputs() -> Vec<Output> {
        vec![
            Output::new("{0.0.0}.a", "Speakers", "Realtek(R) Audio", true),
            Output::new("{0.0.0}.b", "Küche", SONOSVAD_ADAPTER, false),
            Output::new("{0.0.0}.c", "Bad", SONOSVAD_ADAPTER, false),
        ]
    }

    #[test]
    fn the_driver_is_detected_by_adapter_not_by_name() {
        let status = DriverStatus::new(&outputs(), VadService::Running);
        assert_eq!(status.endpoints, 2);
        assert!(status.is_driver_loaded());
    }

    #[test]
    fn nothing_installed_says_nothing() {
        let plain = vec![Output::new("{0.0.0}.a", "Speakers", "Realtek", true)];
        let status = DriverStatus::new(&plain, VadService::NotInstalled);
        assert!(!status.is_driver_loaded());
        assert_eq!(status.advice(), None, "the common case must not nag");
    }

    #[test]
    fn a_fully_working_driver_setup_says_nothing() {
        assert_eq!(
            DriverStatus::new(&outputs(), VadService::Running).advice(),
            None
        );
    }

    #[test]
    fn endpoints_without_a_running_service_are_reported_as_silent() {
        let advice = DriverStatus::new(&outputs(), VadService::Stopped)
            .advice()
            .expect("a stopped service leaves its endpoints silent");
        assert!(advice.contains('2'), "should count the endpoints: {advice}");
        assert!(
            advice.contains("stopped"),
            "should name the state: {advice}"
        );
        assert!(
            advice.contains("Start-Service sonosvad"),
            "should give the fix: {advice}"
        );
    }

    #[test]
    fn endpoints_with_no_service_at_all_are_also_silent() {
        let advice = DriverStatus::new(&outputs(), VadService::NotInstalled)
            .advice()
            .expect("endpoints with no service are just as silent");
        assert!(advice.contains("not installed"), "{advice}");
    }

    #[test]
    fn a_service_without_endpoints_points_at_test_signing() {
        let plain = vec![Output::new("{0.0.0}.a", "Speakers", "Realtek", true)];
        for service in [VadService::Running, VadService::Stopped] {
            let advice = DriverStatus::new(&plain, service)
                .advice()
                .expect("an installed service with no driver is worth explaining");
            assert!(advice.contains("Test signing"), "{advice}");
            assert!(
                advice.contains("does not need the driver"),
                "must reassure rather than alarm: {advice}"
            );
        }
    }

    #[test]
    fn an_unqueryable_service_never_produces_advice() {
        // Unknown means the SCM could not be asked. Guessing either way would
        // put a wrong sentence on screen, so say nothing and rely on the
        // routing warning, which does treat unknown as "might be streaming".
        assert_eq!(
            DriverStatus::new(&outputs(), VadService::Unknown).advice(),
            None
        );
        let plain = vec![Output::new("{0.0.0}.a", "Speakers", "Realtek", true)];
        assert_eq!(
            DriverStatus::new(&plain, VadService::Unknown).advice(),
            None
        );
    }

    #[test]
    fn only_a_confirmed_stop_silences_the_double_stream_warning() {
        assert!(VadService::Running.may_be_streaming());
        assert!(
            VadService::Unknown.may_be_streaming(),
            "not knowing must not silence the warning"
        );
        assert!(!VadService::Stopped.may_be_streaming());
        assert!(!VadService::NotInstalled.may_be_streaming());
    }

    #[test]
    fn installed_means_the_scm_knows_the_service() {
        assert!(VadService::Running.is_installed());
        assert!(VadService::Stopped.is_installed());
        assert!(!VadService::NotInstalled.is_installed());
        assert!(!VadService::Unknown.is_installed());
    }

    /// The Windows path, against services that certainly exist on any Windows
    /// machine — otherwise only the "not installed" branch would ever run,
    /// since no test machine has `sonosvad`.
    #[cfg(windows)]
    #[test]
    fn the_scm_probe_reads_real_services() {
        use super::probe_named;

        // Windows Audio: present and running wherever this app makes sense.
        assert_eq!(
            probe_named("Audiosrv"),
            VadService::Running,
            "the Windows Audio service should read as running"
        );

        // A name nothing could plausibly have registered must come back as
        // "not installed" rather than "unknown": the difference decides whether
        // the app explains itself or stays quiet.
        assert_eq!(
            probe_named("sonos-driver-no-such-service-2f1c"),
            VadService::NotInstalled
        );
    }

    /// `sonosvad` itself, whatever this machine happens to have.
    ///
    /// The value is not asserted — it legitimately differs between a developer
    /// box and one with the driver installed. What is asserted is that probing
    /// returns an answer at all rather than hanging, panicking, or needing
    /// elevation the app does not have.
    #[cfg(windows)]
    #[test]
    fn probing_sonosvad_always_answers_without_elevation() {
        let service = super::probe_service();
        assert_ne!(
            service,
            VadService::Unknown,
            "the SCM should be reachable unelevated; Unknown means the query itself failed"
        );
    }
}
