//! Scopes, Launcher resolution, sessions, Conversations and notifications.
//!
//! - [`disk`]: the free-space guard ([`disk::DiskGuard`]) — probes, warns and gates intake.
//! - [`health`]: the stall monitor ([`health::HealthMonitor`]) — named and Turn beacons.
//! - [`housekeeping`]: the daily pass ([`housekeeping::Housekeeping`]) — retention rules.

pub mod disk;
pub mod health;
pub mod housekeeping;
