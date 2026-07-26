//! Service Control Manager integration for the driverless host.
//!
//! Only the SCM handshake lives here; the work is in [`sonos_cast_service::worker`], so
//! `sonos-cast-svc console` exercises an identical code path.

use std::ffi::OsString;
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use sonos_driver_core::StopHandle;
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use sonos_cast_service::worker::{self, WorkerSettings};

/// The SCM service name.
///
/// Distinct from the driver host's `sonosvad`, because both can be installed
/// on one machine even though they should not drive the same rooms.
pub(crate) const SERVICE_NAME: &str = "sonoscast";
/// What a human sees in services.msc.
pub(crate) const DISPLAY_NAME: &str = "Sonos Cast Bridge (driverless)";
/// A service that owns its process, so a fault cannot take anything else down.
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

windows_service::define_windows_service!(ffi_service_main, service_main);

/// Settings captured from the command line before the SCM takes over.
static SETTINGS: OnceLock<WorkerSettings> = OnceLock::new();

/// Hand the process to the SCM. Only valid when launched by the SCM itself.
///
/// # Errors
///
/// Fails if the process was not started as a service.
pub(crate) fn run_as_service(settings: WorkerSettings) -> anyhow::Result<()> {
    let _ = SETTINGS.set(settings);
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        tracing::error!(error = %format!("{err:#}"), "service terminated with an error");
    }
}

fn run_service() -> anyhow::Result<()> {
    let settings = SETTINGS.get().cloned().unwrap_or_default();

    // The control handler runs on an SCM thread, so it may only flip a flag and
    // return; anything slower makes the SCM report the service as unresponsive.
    let stop = StopHandle::new();
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let handler_stop = stop.clone();
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop | ServiceControl::Shutdown => {
                handler_stop.stop();
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    let set_state = |state: ServiceState, wait_hint: Duration| -> windows_service::Result<()> {
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint,
            process_id: None,
        })
    };

    set_state(ServiceState::StartPending, Duration::from_secs(30))?;
    tracing::info!(
        config = %sonos_cast_service::config::default_config_path().display(),
        "sonos cast service starting"
    );
    set_state(ServiceState::Running, Duration::default())?;

    worker::run(&settings, &stop);

    set_state(ServiceState::StopPending, Duration::from_secs(30))?;
    drop(shutdown_rx);
    tracing::info!("sonos cast service stopped cleanly");

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

/// Register the service with the SCM, or reconfigure it if it already exists.
///
/// Idempotent on purpose: re-running the installer after moving or rebuilding
/// the binary should repoint the existing service rather than fail.
///
/// # Errors
///
/// Fails without administrator rights.
pub(crate) fn install(seed_host: Option<&str>) -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    let exe = std::env::current_exe()?;
    let mut launch_arguments = vec![OsString::from("run")];
    if let Some(seed) = seed_host {
        launch_arguments.push(OsString::from("--seed-host"));
        launch_arguments.push(OsString::from(seed));
    }

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments,
        // WASAPI is unusable until the audio stack is up. AudioEndpointBuilder
        // is itself a dependency of Audiosrv, so naming Audiosrv covers both.
        dependencies: vec![windows_service::service::ServiceDependency::Service(
            OsString::from("Audiosrv"),
        )],
        // LocalSystem: loopback capture in session 0 sees the whole machine's
        // mix, which is exactly what should reach the speakers. No endpoint is
        // renamed here, so this needs less privilege than the driver host --
        // it is used for the same session-0 reason, not for registry access.
        account_name: None,
        account_password: None,
    };

    let service = if let Ok(service) = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_CONFIG,
    ) {
        service.change_config(&service_info)?;
        println!("Reconfigured existing service '{SERVICE_NAME}'.");
        service
    } else {
        let service = manager.create_service(
            &service_info,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
        )?;
        println!("Installed service '{SERVICE_NAME}' ({DISPLAY_NAME}).");
        service
    };

    service.set_description(
        "Streams configured audio sources to Sonos rooms without a virtual audio driver.",
    )?;

    let config_path = sonos_cast_service::config::default_config_path();
    if sonos_cast_service::config::write_example_if_absent(&config_path)? {
        println!(
            "Wrote a starter configuration to {}.",
            config_path.display()
        );
    }

    println!("Binary:  {}", exe.display());
    println!("Config:  {}", config_path.display());
    println!("Edit the config, then start it with:  sc start {SERVICE_NAME}");
    Ok(())
}

/// Stop (if running) and delete the service.
///
/// # Errors
///
/// Fails without administrator rights.
pub(crate) fn uninstall() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;

    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
        // Deleting a running service only marks it for deletion, leaving the
        // name unusable until the next reboot, so wait for the stop to land.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if service.query_status()?.current_state == ServiceState::Stopped {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    service.delete()?;
    println!("Removed service '{SERVICE_NAME}'.");
    Ok(())
}
