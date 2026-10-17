//! Paired Devices: `daemon/secrets/devices.json` (architecture §9.1; management-api §2.12; #13,
//! #21).
//!
//! The file holds one record per paired Device: its id, label, platform, the SPKI-SHA-256 of its
//! client cert (`spki`, the same value as [`DeviceInfo::fingerprint`]) and its created/last-seen
//! times. A revoked Device is kept as a **tombstone** — the record with `revoked_at` set — so its
//! cert can be answered with `401 device_revoked` instead of `401 unknown_device` (#21 D14).
//!
//! The file is a typed value here; persistence is [`crate::backend::pairing::SecretsStore`], so
//! `claudeclaw-server` never touches the filesystem (§3.1).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};

use chrono::{DateTime, Utc};
use claudeclaw_api::DeviceInfo;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::tls::identity::DeviceId;

/// The `schema` this build writes into `devices.json`.
pub const DEVICES_SCHEMA: u32 = 1;

/// `daemon/secrets/devices.json` (architecture §9.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevicesFile {
    #[serde(default = "devices_schema")]
    pub schema: u32,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Top-level keys a later build added, kept on a rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn devices_schema() -> u32 {
    DEVICES_SCHEMA
}

impl Default for DevicesFile {
    fn default() -> Self {
        DevicesFile {
            schema: DEVICES_SCHEMA,
            devices: Vec::new(),
            extra: Map::new(),
        }
    }
}

/// One paired Device, or its tombstone once revoked (#21 D14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub label: String,
    /// The Device's OS, e.g. `macos` (#41 D4); `None` for one paired before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The SPKI-SHA-256 of the Device cert's key, in unpadded base64url (43 characters).
    pub spki: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Set once the Device is revoked; the record stays as a tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Device {
    /// Whether the record is a live Device rather than a revoked tombstone.
    pub fn is_live(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// What a client cert's SPKI-SHA-256 means to the listener (architecture §4 `claudeclaw-server`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceMatch {
    /// A live paired Device.
    Known(DeviceId),
    /// A revoked Device's tombstone: `401 device_revoked` (#20, #21 D14).
    Revoked,
    /// No record at all: `401 unknown_device`, or `Pairing` on `POST /v1/pair`.
    Unknown,
}

/// The listener's read-only view of `devices.json` (architecture §4).
pub trait DeviceLookup: Send + Sync + 'static {
    fn lookup_spki(&self, spki_sha256: &str) -> DeviceMatch;
}

impl<T: DeviceLookup + ?Sized> DeviceLookup for Arc<T> {
    fn lookup_spki(&self, spki_sha256: &str) -> DeviceMatch {
        (**self).lookup_spki(spki_sha256)
    }
}

impl DeviceLookup for RwLock<DevicesFile> {
    fn lookup_spki(&self, spki_sha256: &str) -> DeviceMatch {
        read(self).lookup_spki(spki_sha256)
    }
}

impl DevicesFile {
    /// The live Devices, in created order, as the wire [`DeviceInfo`]. `connected` is the set of
    /// Device ids with a live connection; `current` is the calling Device, if any.
    pub fn infos(&self, connected: &BTreeSet<DeviceId>, current: Option<&str>) -> Vec<DeviceInfo> {
        self.devices
            .iter()
            .filter(|device| device.is_live())
            .map(|device| DeviceInfo {
                id: device.id.clone(),
                label: device.label.clone(),
                fingerprint: device.spki.clone(),
                created_at: device.created_at,
                last_seen_at: device.last_seen_at,
                connected: connected.contains(&device.id),
                current: current == Some(device.id.as_str()),
            })
            .collect()
    }

    /// The record for `id`, live or tombstone.
    pub fn get(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.id == id)
    }

    /// What this SPKI-SHA-256 means (#21 D14).
    pub fn lookup_spki(&self, spki_sha256: &str) -> DeviceMatch {
        match self
            .devices
            .iter()
            .find(|device| device.spki == spki_sha256)
        {
            Some(device) if device.is_live() => DeviceMatch::Known(device.id.clone()),
            Some(_) => DeviceMatch::Revoked,
            None => DeviceMatch::Unknown,
        }
    }

    /// Records a newly paired Device, or refreshes the label/platform on an existing one whose key
    /// pairs again. Returns the record's id.
    pub fn pair(
        &mut self,
        id: DeviceId,
        label: String,
        platform: Option<String>,
        spki: String,
        now: DateTime<Utc>,
    ) -> DeviceId {
        if let Some(device) = self.devices.iter_mut().find(|device| device.spki == spki) {
            device.label = label;
            device.platform = platform;
            device.revoked_at = None;
            device.last_seen_at = Some(now);
            return device.id.clone();
        }
        self.devices.push(Device {
            id: id.clone(),
            label,
            platform,
            spki,
            created_at: now,
            last_seen_at: Some(now),
            revoked_at: None,
        });
        id
    }

    /// Renames a live Device. `None` when there is no such live Device.
    pub fn rename(&mut self, id: &str, label: String) -> Option<()> {
        let device = self
            .devices
            .iter_mut()
            .find(|device| device.id == id && device.is_live())?;
        device.label = label;
        Some(())
    }

    /// Marks a live Device revoked, keeping its tombstone. `false` when it was already revoked or
    /// absent.
    pub fn revoke(&mut self, id: &str, now: DateTime<Utc>) -> bool {
        match self
            .devices
            .iter_mut()
            .find(|device| device.id == id && device.is_live())
        {
            Some(device) => {
                device.revoked_at = Some(now);
                true
            }
            None => false,
        }
    }

    /// Records a connection from a live Device (#13). `false` for an unknown or revoked id.
    pub fn mark_seen(&mut self, id: &str, now: DateTime<Utc>) -> bool {
        match self
            .devices
            .iter_mut()
            .find(|device| device.id == id && device.is_live())
        {
            Some(device) => {
                device.last_seen_at = Some(now);
                true
            }
            None => false,
        }
    }
}

/// A shared, mutable `devices.json` for the listener and the service to read (architecture §4).
pub type SharedDevices = Arc<RwLock<DevicesFile>>;

/// Locks for read, recovering a poisoned lock (the data is a plain struct, safe to keep using).
pub(crate) fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// `BTreeMap` is re-exported for callers building a `connected` set; keeps the file's ordering.
pub type ConnectedSet = BTreeMap<DeviceId, DateTime<Utc>>;

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 16, h, 0, 0).unwrap()
    }

    fn devices() -> DevicesFile {
        let mut file = DevicesFile::default();
        file.pair(
            "dev-1".into(),
            "laptop".into(),
            Some("macos".into()),
            "FP1".into(),
            at(9),
        );
        file.pair("dev-2".into(), "phone".into(), None, "FP2".into(), at(10));
        file
    }

    #[test]
    fn paired_devices_round_trip_through_json() {
        let file = devices();
        let text = serde_json::to_string(&file).unwrap();
        let back: DevicesFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, file);
        // Unknown top-level keys survive.
        let mut value: Value = serde_json::from_str(&text).unwrap();
        value["future_key"] = serde_json::json!(true);
        let back: DevicesFile = serde_json::from_value(value).unwrap();
        assert_eq!(back.extra["future_key"], serde_json::json!(true));
        assert_eq!(
            serde_json::to_value(&back).unwrap()["future_key"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn lookup_finds_pairs_and_tombstones() {
        let mut file = devices();
        assert_eq!(file.lookup_spki("FP1"), DeviceMatch::Known("dev-1".into()));
        assert_eq!(file.lookup_spki("nope"), DeviceMatch::Unknown);
        assert!(file.revoke("dev-1", at(11)));
        assert_eq!(file.lookup_spki("FP1"), DeviceMatch::Revoked);
        assert_eq!(file.get("dev-1").unwrap().revoked_at, Some(at(11)));
    }

    #[test]
    fn revoking_twice_is_false_and_tombstones_stay() {
        let mut file = devices();
        assert!(file.revoke("dev-1", at(11)));
        assert!(!file.revoke("dev-1", at(12)));
        assert_eq!(file.get("dev-1").unwrap().revoked_at, Some(at(11)));
        assert!(!file.revoke("missing", at(12)));
    }

    #[test]
    fn pairing_the_same_key_again_reuses_the_record_and_unrevokes() {
        let mut file = devices();
        assert!(file.revoke("dev-1", at(11)));
        let id = file.pair(
            "dev-9".into(),
            "old laptop".into(),
            Some("linux".into()),
            "FP1".into(),
            at(12),
        );
        assert_eq!(id, "dev-1");
        assert_eq!(file.devices.len(), 2);
        let record = file.get("dev-1").unwrap();
        assert_eq!(record.label, "old laptop");
        assert_eq!(record.platform.as_deref(), Some("linux"));
        assert_eq!(record.created_at, at(9));
        assert_eq!(record.last_seen_at, Some(at(12)));
        assert_eq!(record.revoked_at, None);
    }

    #[test]
    fn rename_and_mark_seen_ignore_tombstones() {
        let mut file = devices();
        assert_eq!(file.rename("dev-1", "new".into()), Some(()));
        assert_eq!(file.rename("missing", "new".into()), None);
        assert!(file.mark_seen("dev-2", at(12)));
        assert_eq!(file.get("dev-2").unwrap().last_seen_at, Some(at(12)));
        assert!(!file.mark_seen("missing", at(12)));
        file.revoke("dev-2", at(13));
        assert!(!file.mark_seen("dev-2", at(14)));
        assert_eq!(file.rename("dev-2", "x".into()), None);
    }

    #[test]
    fn infos_exclude_tombstones_and_flag_current() {
        let mut file = devices();
        file.revoke("dev-2", at(11));
        let connected: BTreeSet<DeviceId> = ["dev-1".to_owned()].into_iter().collect();
        let infos = file.infos(&connected, Some("dev-1"));
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, "dev-1");
        assert!(infos[0].connected);
        assert!(infos[0].current);
        assert_eq!(infos[0].fingerprint, "FP1");
    }

    #[test]
    fn an_empty_file_is_the_default() {
        assert_eq!(DevicesFile::default().schema, DEVICES_SCHEMA);
        let parsed: DevicesFile = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, DevicesFile::default());
    }
}
