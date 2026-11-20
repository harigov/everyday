//! Which of the owner's devices the bot trusts, and making that true of its
//! encryption.
//!
//! The owner's Matrix ID is not enough to be believed: anybody who signs in to
//! that account on a new device -- and the homeserver itself -- can send
//! events under it, and could read every reply the bot gave if the bot shared
//! its keys with whatever device the account has. So the bot trusts a
//! hand-picked list of the owner's device ids, chosen in Settings → Chat and
//! kept on the link. A device that is cross-signed, or was verified by
//! comparing emoji, is not trusted for that: only the list counts.
//!
//! The list does two jobs, and this is where both are kept in step with it:
//!
//! * **Whose words are believed.** `hearing` and `rtc` ask [`Trust::is_trusted`]
//!   of the device an encrypted message, or a call's key, came from. That is
//!   the list in memory, swapped the moment it changes.
//! * **Who can read the replies.** In the crypto store, each of the owner's
//!   devices on the list is marked [`LocalTrust::Verified`] and every other
//!   one [`LocalTrust::BlackListed`] ([`Trust::apply`]). The client is built to
//!   share room keys only with trusted devices (`signin::build_client`), and a
//!   blacklisted device is never given one, so what the bot writes is
//!   unreadable on a device that is not on the list. It is re-applied when the
//!   bot starts, when the list changes, and when the owner's device list does
//!   (a device signed in a moment ago is turned away before the next message,
//!   not after).
//!
//! An empty list trusts no device: nothing is believed and nothing is
//! readable. That is what an existing link has until its owner picks devices,
//! so a device that writes while not trusted is reported
//! ([`Trust::note_untrusted`]) for Settings to say "your device X wrote to
//! the assistant but is not trusted yet", and for the service to raise a
//! notification, once.

use super::rules::{Reported, TrustedDevices};
use crate::connector::UntrustedReport;
use futures::StreamExt;
use futures::future::BoxFuture;
use jiff::Timestamp;
use karmayoga_service::chat::{ChatDevice, ChatDevices, ChatUntrustedDevice};
use karmayoga_service::error::{CommandError, CommandResult, codes};
use matrix_sdk::Client;
use matrix_sdk::encryption::LocalTrust;
use matrix_sdk::ruma::{DeviceId, OwnedUserId};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;

/// How long asking the server for the owner's devices may take before the
/// bot goes on with the ones it already knows.
const LOOKUP_PATIENCE: Duration = Duration::from_secs(20);

/// The owner's trusted devices, for one running bot.
pub(super) struct Trust {
    client: Client,
    owner: OwnedUserId,
    /// The list, as of the last time it was set. Read by every decision.
    trusted: RwLock<TrustedDevices>,
    report: UntrustedReport,
    /// Which untrusted devices were reported lately.
    reported: Mutex<Reported>,
}

impl Trust {
    pub(super) fn new(
        client: Client,
        owner: OwnedUserId,
        trusted: TrustedDevices,
        report: UntrustedReport,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            owner,
            trusted: RwLock::new(trusted),
            report,
            reported: Mutex::new(Reported::default()),
        })
    }

    /// The list as it is now, for a decision to be made against.
    pub(super) fn trusted(&self) -> TrustedDevices {
        self.trusted.read().unwrap().clone()
    }

    /// Whether `device`, one of the owner's, is on the list.
    pub(super) fn is_trusted(&self, device: &str) -> bool {
        self.trusted.read().unwrap().contains(device)
    }

    /// Whether the list has anything on it.
    pub(super) fn trusts_anybody(&self) -> bool {
        !self.trusted.read().unwrap().is_empty()
    }

    /// Trust exactly `devices` from now on, and make the encryption say so.
    ///
    /// The list is swapped before the store is touched, so that a device
    /// taken off it stops being believed at once, whatever the store does.
    pub(super) async fn set_trusted(&self, devices: TrustedDevices) -> Result<(), String> {
        *self.trusted.write().unwrap() = devices;
        self.apply().await
    }

    /// Mark each of the owner's devices the store knows trusted if it is on
    /// the list and blacklisted if it is not. Idempotent: it changes only
    /// what is not as the list says, so the store's own note of the change
    /// does not start it again.
    ///
    /// Tries every device even when one fails, and says what failed.
    pub(super) async fn apply(&self) -> Result<(), String> {
        let trusted = self.trusted.read().unwrap().clone();
        let devices = self
            .client
            .encryption()
            .get_user_devices(&self.owner)
            .await
            .map_err(|e| format!("The owner's devices could not be read ({e})."))?;
        let mut failed = Vec::new();
        for device in devices.devices() {
            let wanted = if trusted.contains(device.device_id().as_str()) {
                LocalTrust::Verified
            } else {
                LocalTrust::BlackListed
            };
            if device.local_trust_state() == wanted {
                continue;
            }
            if let Err(error) = device.set_local_trust(wanted).await {
                let id = device.device_id();
                tracing::warn!(device = %id, "couldn't change a device's trust: {error}");
                failed.push(device.device_id().to_string());
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!("The trust of {} could not be saved.", failed.join(", ")))
        }
    }

    /// Follow the owner's devices as the SDK learns of them, and apply the
    /// list whenever one is new or changed. Subscribed before this returns,
    /// so nothing the first sync brings falls between; stops when the
    /// returned guard is dropped.
    pub(super) async fn watch(self: &Arc<Self>) -> Option<Watching> {
        let updates = match self.client.encryption().devices_stream().await {
            Ok(updates) => updates,
            Err(error) => {
                tracing::warn!("couldn't watch the owner's devices: {error}");
                return None;
            }
        };
        let me = Arc::clone(self);
        let task = tokio::spawn(async move {
            futures::pin_mut!(updates);
            while let Some(update) = updates.next().await {
                if !update.new.contains_key(&me.owner) && !update.changed.contains_key(&me.owner) {
                    continue;
                }
                if let Err(problem) = me.apply().await {
                    tracing::warn!("{problem}");
                }
            }
        });
        Some(Watching(task.abort_handle()))
    }

    /// The owner's devices as the bot sees them, asking the server first so a
    /// device signed in a moment ago is on the list.
    pub(super) async fn list(&self) -> CommandResult<Vec<ChatDevice>> {
        self.refresh().await;
        // Whatever the lookup found is turned away before anything else is
        // sent, rather than at the next change.
        if let Err(problem) = self.apply().await {
            tracing::warn!("{problem}");
        }
        let trusted = self.trusted.read().unwrap().clone();
        let devices =
            self.client.encryption().get_user_devices(&self.owner).await.map_err(|e| {
                CommandError::new(
                    codes::NETWORK,
                    format!("The owner's devices could not be read ({e})."),
                )
            })?;
        let mut listed: Vec<ChatDevice> = devices
            .devices()
            .map(|device| ChatDevice {
                device_id: device.device_id().to_string(),
                display_name: device
                    .display_name()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                cross_signed: device.is_cross_signed_by_owner(),
                trusted: trusted.contains(device.device_id().as_str()),
            })
            .collect();
        listed.sort_by(|a, b| {
            (!a.trusted, a.display_name.as_deref(), &a.device_id).cmp(&(
                !b.trusted,
                b.display_name.as_deref(),
                &b.device_id,
            ))
        });
        Ok(listed)
    }

    /// Ask the server for the owner's current devices, within
    /// [`LOOKUP_PATIENCE`]. A failure leaves the store as it was.
    async fn refresh(&self) {
        let encryption = self.client.encryption();
        let ask = encryption.request_user_identity(&self.owner);
        match tokio::time::timeout(LOOKUP_PATIENCE, ask).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::debug!("couldn't ask for the owner's devices, using known ones: {error}")
            }
            Err(_) => tracing::debug!("asking for the owner's devices took too long"),
        }
    }

    /// An encrypted message, or a call, came from a device of the owner's
    /// that is not on the list, and was ignored. Tell Settings which device
    /// it was -- by the name its owner gave it -- unless it was told about
    /// that device a moment ago.
    ///
    /// `None` is a device the encryption could not name: nothing can be
    /// shown for it, so the owner's devices are asked for, and the next try
    /// will be one that can.
    pub(super) async fn note_untrusted(self: &Arc<Self>, device: Option<&DeviceId>) {
        let now = Instant::now();
        if !self.reported.lock().unwrap().should_report(device.map(DeviceId::as_str), now) {
            return;
        }
        let Some(device) = device else {
            tracing::warn!("ignoring a message from a device of the owner's that can't be named");
            let me = Arc::clone(self);
            tokio::spawn(async move {
                me.refresh().await;
                let _ = me.apply().await;
            });
            return;
        };
        let display_name = match self.client.encryption().get_device(&self.owner, device).await {
            Ok(Some(known)) => known
                .display_name()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            _ => None,
        };
        tracing::warn!(%device, "ignoring a device of the owner's that is not trusted");
        (self.report)(ChatUntrustedDevice {
            device_id: device.to_string(),
            display_name,
            at: Timestamp::now(),
        });
    }
}

/// Stops following the owner's devices when dropped.
pub(super) struct Watching(AbortHandle);

impl Drop for Watching {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// [`Trust`] as the service sees it: a type of its own because the futures it
/// hands back must own what they use, and an `Arc` is what they can own.
pub(super) struct Devices(pub Arc<Trust>);

impl ChatDevices for Devices {
    fn list(&self) -> BoxFuture<'static, CommandResult<Vec<ChatDevice>>> {
        let trust = Arc::clone(&self.0);
        Box::pin(async move { trust.list().await })
    }

    fn trust(&self, devices: Vec<String>) -> BoxFuture<'static, CommandResult<()>> {
        let trust = Arc::clone(&self.0);
        Box::pin(async move {
            trust
                .set_trusted(TrustedDevices::new(devices))
                .await
                .map_err(|problem| CommandError::new(codes::INTERNAL, problem))
        })
    }
}
