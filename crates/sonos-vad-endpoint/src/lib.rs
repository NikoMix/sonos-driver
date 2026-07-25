//! Enumeration and renaming of the fixed `sonosvad` render endpoints.
//!
//! The kernel driver ([`driver/sonosvad`](https://github.com/NikoMix/sonos-driver/tree/main/driver/sonosvad))
//! exposes a **fixed** pool of render endpoints declared in its INF - "Sonos
//! Output 1", "Sonos Output 2", ... - because creating and destroying audio
//! subdevices at runtime from kernel mode is the single riskiest thing a
//! `PortCls` driver can do (ADR-0018).
//!
//! Making each Sonos room show up under its own name in the Windows sound
//! picker is therefore a **user-mode** job: bind a room to a free slot and
//! rename that slot's endpoint. That is exactly what the Sound control panel
//! does when you rename a device - it writes `PKEY_Device_FriendlyName` on the
//! endpoint's property store - so the change is persistent, survives reboots,
//! and needs no kernel involvement.
//!
//! # Layering
//!
//! - [`naming`](mod@naming) - pure, platform-independent logic: which slot an
//!   endpoint belongs to, what its factory name is, and how a set of rooms maps
//!   onto a set of slots. Fully unit-tested everywhere.
//! - `sys` - the only `unsafe` island: `IMMDeviceEnumerator` /
//!   `IPropertyStore` calls. Windows-only.
//!
//! [`VadEndpoints`] is the seam between the two, so the planner can be tested
//! against an in-memory fake on any platform.
//!
//! # Privileges
//!
//! Writing `PKEY_Device_FriendlyName` requires the process to be elevated.
//! [`Error::AccessDenied`] is returned otherwise, and is worth surfacing
//! distinctly: it is the single most likely failure in normal use.

mod naming;

#[cfg(windows)]
#[allow(unsafe_code)]
// The sole sanctioned `unsafe` island: MMDevice/IPropertyStore COM calls.
mod sys;

pub use naming::{
    factory_name_for_slot, plan_bindings, plan_bindings_for, plan_release_all,
    slot_from_factory_name, AggregateBinding, Binding, BindingPlan, PlanOptions, Release,
    AGGREGATE_NAME, DEFAULT_ADAPTER_NAME, FACTORY_NAME_PREFIX,
};

use sonos_vad_control::RoomKey;

/// Errors raised while inspecting or renaming sonosvad endpoints.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Renaming an endpoint requires an elevated process.
    #[error("renaming audio endpoints requires an elevated process (run as administrator)")]
    AccessDenied,

    /// No endpoint with the given id is present.
    #[error("audio endpoint not found: {0}")]
    NotFound(String),

    /// The sonosvad driver is not installed, or exposes no render endpoints.
    #[error(
        "no endpoints from adapter {adapter:?} were found; is the sonosvad driver installed and started?"
    )]
    DriverNotPresent {
        /// The adapter friendly name that was searched for.
        adapter: String,
    },

    /// A Windows API call failed.
    #[error("{call} failed: {source}")]
    Windows {
        /// The Win32/COM call that failed.
        call: &'static str,
        /// The underlying error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The operation is only implemented on Windows.
    #[error("sonosvad endpoint control is only available on Windows")]
    UnsupportedPlatform,
}

/// Convenient result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A single render endpoint exposed by the sonosvad driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VadEndpoint {
    /// Stable WASAPI endpoint id (`{0.0.0.00000000}.{...}`).
    ///
    /// This is the durable key: it survives renames and reboots, and is what
    /// [`audio_capture`](https://docs.rs/audio-capture) needs to open a loopback
    /// capture against this endpoint.
    pub device_id: String,

    /// The name currently shown in the Windows sound picker.
    pub friendly_name: String,

    /// The endpoint's factory description, as declared in the driver INF
    /// (`Sonos Output 3`). Renaming does not change it, so it is how the slot
    /// number is recovered after a bind.
    pub device_desc: String,

    /// The adapter (device interface) this endpoint belongs to, i.e. its
    /// `PKEY_DeviceInterface_FriendlyName`. For sonosvad endpoints this is
    /// always [`DEFAULT_ADAPTER_NAME`]; it is carried so a diagnostic listing
    /// of *all* endpoints can show where each one came from.
    pub adapter: String,

    /// 1-based slot number parsed from [`Self::device_desc`], when it matches
    /// the expected pattern.
    pub slot: Option<u32>,

    /// Whether Windows currently reports the endpoint as active.
    pub active: bool,

    /// Whether Windows reports the endpoint as *disabled* specifically.
    ///
    /// Distinct from `!active`, which also covers unplugged and not-present.
    /// Only a disabled endpoint may be re-enabled by writing `DeviceState`;
    /// forcing a not-present one to "active" would leave a phantom entry in the
    /// sound picker pointing at hardware that is not there.
    pub disabled: bool,
}

impl VadEndpoint {
    /// Whether this endpoint still carries its factory name, i.e. is unbound.
    #[must_use]
    pub fn is_unbound(&self) -> bool {
        self.friendly_name == self.device_desc
    }
}

/// The operations the binder needs from the platform.
///
/// Implemented by [`SystemEndpoints`] against the real Windows APIs, and by
/// test fakes elsewhere, so [`plan_bindings`] and the reconcile loop can be
/// exercised without a driver installed.
pub trait VadEndpoints {
    /// List every render endpoint belonging to the sonosvad adapter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DriverNotPresent`] when the adapter exposes no
    /// endpoints, or [`Error::Windows`] when enumeration fails.
    fn list(&self) -> Result<Vec<VadEndpoint>>;

    /// Set the sound-picker name of `device_id` to `name`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AccessDenied`] when the process is not elevated,
    /// [`Error::NotFound`] when the endpoint has disappeared, or
    /// [`Error::Windows`] for any other failure.
    fn rename(&self, device_id: &str, name: &str) -> Result<()>;

    /// Show or hide `device_id` in the Windows sound UI.
    ///
    /// Hiding is how the fixed pool of driver slots is reconciled with a
    /// household of a different size: spare endpoints are disabled rather than
    /// left in the picker as "Sonos Output 7".
    ///
    /// # Errors
    ///
    /// Returns [`Error::AccessDenied`] when the process is not elevated,
    /// [`Error::NotFound`] when the endpoint has disappeared, or
    /// [`Error::Windows`] for any other failure.
    fn set_visible(&self, device_id: &str, visible: bool) -> Result<()>;
}

/// Applies a [`BindingPlan`] by renaming the affected endpoints and reconciling
/// their visibility.
///
/// Every step is attempted even after a failure, and the first error is
/// returned at the end, so one bad endpoint cannot strand the others in a
/// half-applied state.
///
/// Order matters: endpoints are revealed before they are named (so a room's
/// output exists in the picker by the time it carries the room's name) and
/// hidden last (so a slot being handed from one room to another is never
/// momentarily invisible).
///
/// # Errors
///
/// Returns the first [`Error`] encountered, after attempting every step.
pub fn apply_plan<E: VadEndpoints + ?Sized>(endpoints: &E, plan: &BindingPlan) -> Result<()> {
    let mut first_error = None;

    for device_id in &plan.show {
        tracing::debug!(%device_id, "revealing endpoint");
        if let Err(error) = endpoints.set_visible(device_id, true) {
            tracing::warn!(%device_id, %error, "failed to reveal endpoint");
            first_error.get_or_insert(error);
        }
    }

    for release in &plan.releases {
        tracing::debug!(device_id = %release.device_id, name = %release.name, "releasing endpoint");
        if let Err(error) = endpoints.rename(&release.device_id, &release.name) {
            tracing::warn!(device_id = %release.device_id, %error, "failed to release endpoint");
            first_error.get_or_insert(error);
        }
    }

    for bind in &plan.binds {
        if !bind.rename_needed {
            continue;
        }
        tracing::debug!(device_id = %bind.device_id, name = %bind.name, room = %bind.room.uuid, "binding endpoint");
        if let Err(error) = endpoints.rename(&bind.device_id, &bind.name) {
            tracing::warn!(device_id = %bind.device_id, %error, "failed to bind endpoint");
            first_error.get_or_insert(error);
        }
    }

    if let Some(aggregate) = &plan.aggregate {
        if aggregate.rename_needed {
            tracing::debug!(device_id = %aggregate.device_id, name = %aggregate.name, rooms = aggregate.rooms.len(), "binding aggregate endpoint");
            if let Err(error) = endpoints.rename(&aggregate.device_id, &aggregate.name) {
                tracing::warn!(device_id = %aggregate.device_id, %error, "failed to bind aggregate endpoint");
                first_error.get_or_insert(error);
            }
        }
    }

    for device_id in &plan.hide {
        tracing::debug!(%device_id, "hiding spare endpoint");
        if let Err(error) = endpoints.set_visible(device_id, false) {
            tracing::warn!(%device_id, %error, "failed to hide spare endpoint");
            first_error.get_or_insert(error);
        }
    }

    first_error.map_or(Ok(()), Err)
}

/// Rebinds the sonosvad endpoints so they match `rooms`, and reports the
/// resulting room-to-endpoint map.
///
/// Rooms beyond the number of available slots are dropped, in the order
/// [`plan_bindings`] defines. The caller is expected to surface that to the
/// user: it means the driver needs to be reinstalled with a larger
/// `SONOSVAD_ENDPOINT_COUNT`.
///
/// # Errors
///
/// Propagates any error from [`VadEndpoints::list`] or [`apply_plan`].
pub fn bind_rooms<E: VadEndpoints + ?Sized>(
    endpoints: &E,
    rooms: &[RoomKey],
) -> Result<Vec<Binding>> {
    Ok(bind_rooms_for(endpoints, rooms, PlanOptions::default())?.rooms)
}

/// What [`bind_rooms_for`] resolved: one endpoint per room, plus the aggregate
/// output when one was reserved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bindings {
    /// One endpoint per discovered room.
    pub rooms: Vec<Binding>,
    /// The "All Sonos" endpoint, when [`PlanOptions::aggregate`] asked for one
    /// and there were at least two rooms to aggregate.
    pub aggregate: Option<AggregateBinding>,
}

/// [`bind_rooms`], with aggregate and spare-hiding behaviour from `options`.
///
/// # Errors
///
/// Propagates any error from [`VadEndpoints::list`] or [`apply_plan`].
pub fn bind_rooms_for<E: VadEndpoints + ?Sized>(
    endpoints: &E,
    rooms: &[RoomKey],
    options: PlanOptions,
) -> Result<Bindings> {
    let current = endpoints.list()?;
    let plan = plan_bindings_for(&current, rooms, options);

    if plan.unplaced > 0 {
        tracing::warn!(
            unplaced = plan.unplaced,
            slots = current.len(),
            "more Sonos rooms than sonosvad endpoints; rebuild the driver with a larger SONOSVAD_ENDPOINT_COUNT"
        );
    }
    if !plan.hide.is_empty() {
        tracing::info!(
            hidden = plan.hide.len(),
            "hiding spare sonosvad endpoints so the sound picker shows only your rooms"
        );
    }
    if options.aggregate && plan.aggregate.is_none() {
        tracing::info!(
            rooms = rooms.len(),
            "not exposing the All Sonos output: it needs at least two rooms"
        );
    }

    apply_plan(endpoints, &plan)?;
    Ok(Bindings {
        rooms: plan.binds,
        aggregate: plan.aggregate,
    })
}

/// The real Windows implementation of [`VadEndpoints`].
#[derive(Debug, Clone)]
pub struct SystemEndpoints {
    adapter: String,
}

impl SystemEndpoints {
    /// Targets the default sonosvad adapter name ([`DEFAULT_ADAPTER_NAME`]).
    #[must_use]
    pub fn new() -> Self {
        Self::for_adapter(DEFAULT_ADAPTER_NAME)
    }

    /// Targets a specific adapter friendly name.
    ///
    /// Only endpoints whose `PKEY_DeviceInterface_FriendlyName` matches are
    /// considered, which is what keeps the renamer from touching the user's
    /// real sound card.
    ///
    /// An **empty** adapter name disables the filter and lists every render
    /// endpoint on the system. That is a diagnostic mode only: never call
    /// [`VadEndpoints::rename`] with ids obtained that way.
    #[must_use]
    pub fn for_adapter(adapter: impl Into<String>) -> Self {
        Self {
            adapter: adapter.into(),
        }
    }

    /// The adapter friendly name this instance filters on.
    #[must_use]
    pub fn adapter(&self) -> &str {
        &self.adapter
    }
}

impl Default for SystemEndpoints {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl VadEndpoints for SystemEndpoints {
    fn list(&self) -> Result<Vec<VadEndpoint>> {
        sys::list(&self.adapter)
    }

    fn rename(&self, device_id: &str, name: &str) -> Result<()> {
        sys::rename(device_id, name)
    }

    fn set_visible(&self, device_id: &str, visible: bool) -> Result<()> {
        sys::set_visible(device_id, visible)
    }
}

#[cfg(not(windows))]
impl VadEndpoints for SystemEndpoints {
    fn list(&self) -> Result<Vec<VadEndpoint>> {
        Err(Error::UnsupportedPlatform)
    }

    fn rename(&self, _device_id: &str, _name: &str) -> Result<()> {
        Err(Error::UnsupportedPlatform)
    }

    fn set_visible(&self, _device_id: &str, _visible: bool) -> Result<()> {
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeEndpoints {
        endpoints: RefCell<Vec<VadEndpoint>>,
        fail_on: Option<String>,
    }

    impl FakeEndpoints {
        fn with_slots(n: u32) -> Self {
            let endpoints = (1..=n)
                .map(|slot| {
                    let desc = factory_name_for_slot(slot);
                    VadEndpoint {
                        device_id: format!("{{0.0.0.00000000}}.{{slot-{slot}}}"),
                        friendly_name: desc.clone(),
                        device_desc: desc,
                        adapter: DEFAULT_ADAPTER_NAME.to_owned(),
                        slot: Some(slot),
                        active: true,
                        disabled: false,
                    }
                })
                .collect();
            Self {
                endpoints: RefCell::new(endpoints),
                fail_on: None,
            }
        }

        fn names(&self) -> Vec<String> {
            self.endpoints
                .borrow()
                .iter()
                .map(|e| e.friendly_name.clone())
                .collect()
        }

        /// The factory names of the endpoints Windows would still show.
        fn visible_slots(&self) -> Vec<u32> {
            self.endpoints
                .borrow()
                .iter()
                .filter(|e| e.active)
                .filter_map(|e| e.slot)
                .collect()
        }
    }

    impl VadEndpoints for FakeEndpoints {
        fn list(&self) -> Result<Vec<VadEndpoint>> {
            Ok(self.endpoints.borrow().clone())
        }

        fn rename(&self, device_id: &str, name: &str) -> Result<()> {
            if self.fail_on.as_deref() == Some(device_id) {
                return Err(Error::AccessDenied);
            }
            let mut endpoints = self.endpoints.borrow_mut();
            let target = endpoints
                .iter_mut()
                .find(|e| e.device_id == device_id)
                .ok_or_else(|| Error::NotFound(device_id.to_owned()))?;
            target.friendly_name = name.to_owned();
            Ok(())
        }

        fn set_visible(&self, device_id: &str, visible: bool) -> Result<()> {
            if self.fail_on.as_deref() == Some(device_id) {
                return Err(Error::AccessDenied);
            }
            let mut endpoints = self.endpoints.borrow_mut();
            let target = endpoints
                .iter_mut()
                .find(|e| e.device_id == device_id)
                .ok_or_else(|| Error::NotFound(device_id.to_owned()))?;
            target.active = visible;
            target.disabled = !visible;
            Ok(())
        }
    }

    #[test]
    fn binds_rooms_to_free_slots_in_order() {
        let fake = FakeEndpoints::with_slots(4);
        let rooms = vec![
            RoomKey::new("RINCON_1", "Living Room"),
            RoomKey::new("RINCON_2", "Kitchen"),
        ];

        let bindings = bind_rooms(&fake, &rooms).expect("bind");

        assert_eq!(bindings.len(), 2);
        assert_eq!(
            fake.names(),
            vec![
                "Sonos Living Room",
                "Sonos Kitchen",
                "Sonos Output 3",
                "Sonos Output 4",
            ]
        );
    }

    #[test]
    fn rebinding_the_same_rooms_is_idempotent() {
        let fake = FakeEndpoints::with_slots(3);
        let rooms = vec![RoomKey::new("RINCON_1", "Living Room")];

        bind_rooms(&fake, &rooms).expect("first bind");
        let before = fake.names();
        bind_rooms(&fake, &rooms).expect("second bind");

        assert_eq!(before, fake.names());
    }

    #[test]
    fn a_room_that_disappears_releases_its_slot() {
        let fake = FakeEndpoints::with_slots(2);
        let rooms = vec![
            RoomKey::new("RINCON_1", "Living Room"),
            RoomKey::new("RINCON_2", "Kitchen"),
        ];
        bind_rooms(&fake, &rooms).expect("bind both");

        bind_rooms(&fake, &rooms[..1]).expect("bind one");

        assert_eq!(fake.names(), vec!["Sonos Living Room", "Sonos Output 2"]);
    }

    #[test]
    fn surfaces_access_denied_but_still_applies_the_rest() {
        let mut fake = FakeEndpoints::with_slots(2);
        fake.fail_on = Some("{0.0.0.00000000}.{slot-1}".to_owned());
        let rooms = vec![
            RoomKey::new("RINCON_1", "Living Room"),
            RoomKey::new("RINCON_2", "Kitchen"),
        ];

        let error = bind_rooms(&fake, &rooms).expect_err("should report the failure");

        assert!(matches!(error, Error::AccessDenied));
        // The second endpoint was still renamed.
        assert_eq!(fake.names()[1], "Sonos Kitchen");
    }

    #[test]
    fn more_rooms_than_slots_leaves_the_extras_unbound() {
        let fake = FakeEndpoints::with_slots(1);
        let rooms = vec![
            RoomKey::new("RINCON_1", "Living Room"),
            RoomKey::new("RINCON_2", "Kitchen"),
        ];

        let bindings = bind_rooms(&fake, &rooms).expect("bind");

        assert_eq!(bindings.len(), 1);
        assert_eq!(fake.names(), vec!["Sonos Living Room"]);
    }

    #[test]
    fn spare_endpoints_are_hidden_so_the_picker_shows_only_rooms() {
        let fake = FakeEndpoints::with_slots(8);
        let rooms = vec![
            RoomKey::new("RINCON_1", "Wohnzimmer"),
            RoomKey::new("RINCON_2", "Küche"),
        ];

        let bindings = bind_rooms_for(
            &fake,
            &rooms,
            PlanOptions {
                hide_spares: true,
                aggregate: false,
            },
        )
        .expect("bind succeeds");

        assert_eq!(bindings.rooms.len(), 2);
        assert_eq!(
            fake.visible_slots(),
            vec![1, 2],
            "only the endpoints backing a real room may stay in the sound picker"
        );
    }

    #[test]
    fn a_hidden_endpoint_is_revealed_when_a_room_needs_it() {
        let fake = FakeEndpoints::with_slots(3);
        let options = PlanOptions {
            hide_spares: true,
            aggregate: false,
        };

        // One room: slots 2 and 3 get hidden.
        let one = vec![RoomKey::new("RINCON_1", "Wohnzimmer")];
        bind_rooms_for(&fake, &one, options).expect("first bind succeeds");
        assert_eq!(fake.visible_slots(), vec![1]);

        // A second room appears; its slot has to come back.
        let two = vec![
            RoomKey::new("RINCON_1", "Wohnzimmer"),
            RoomKey::new("RINCON_2", "Küche"),
        ];
        bind_rooms_for(&fake, &two, options).expect("second bind succeeds");
        assert_eq!(fake.visible_slots(), vec![1, 2]);
        assert_eq!(
            fake.names(),
            vec![
                "Sonos Wohnzimmer".to_owned(),
                "Sonos Küche".to_owned(),
                factory_name_for_slot(3),
            ]
        );
    }

    #[test]
    fn hiding_is_opt_in_so_the_default_leaves_windows_alone() {
        let fake = FakeEndpoints::with_slots(4);
        let rooms = vec![RoomKey::new("RINCON_1", "Wohnzimmer")];

        bind_rooms(&fake, &rooms).expect("bind succeeds");

        assert_eq!(fake.visible_slots(), vec![1, 2, 3, 4]);
    }
}
