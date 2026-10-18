//! The settings seam (management-api §2.6: `settings.get` and `settings.update`; #13, #14, #23).
//!
//! The key catalogue lives behind this seam, next to the file it describes: the backend refuses
//! a patch's TS-only keys (`422 ts_only_key`), the Rust `launcher` key (`422 invalid_value`,
//! with a hint to use §2.5) and unknown keys (`422 unknown_key`) before anything is written. The
//! route only owns the rule that needs the caller: `remote.*` is local-only (§2.6). Secrets are
//! write-only, so neither call hands one back (`SecretState{set}`, #13).

use claudeclaw_api::revision::Revision;
use claudeclaw_api::settings::{SettingsApplied, SettingsPatch, SettingsView};

use super::BackendFuture;

/// The Settings routes (management-api §2.6). The composition root wires this to the settings
/// store, which reads `settings.json` fresh and patches it atomically.
pub trait SettingsBackend: Send + Sync + 'static {
    /// The typed view of `settings.json` with its Revision, the TS-only keys present and the
    /// keys an env var wins over (§2.6).
    fn get(&self) -> BackendFuture<'_, SettingsView>;

    /// A JSON Merge Patch (RFC 7396) over a fresh read of the file's own key names (§2.6).
    /// `expected` is the optional `If-Match` revision (§1.5): a stale one is
    /// `409 revision_mismatch` with `details.current`. A patch that changes nothing writes
    /// nothing and answers with an empty `changed_keys`.
    fn update(
        &self,
        patch: SettingsPatch,
        expected: Option<Revision>,
    ) -> BackendFuture<'_, SettingsApplied>;
}
