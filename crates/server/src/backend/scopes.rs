//! The scopes seam (management-api §2.5: `scopes.list`, `launchers.list`, `launchers.set`,
//! `launchers.unset` and `launchers.test`; #8, #22, #40, #42).
//!
//! A Launcher is `"direct"` or an absolute path (#8). The routes check that shape and refuse a
//! `job:` Scope on the Scope-centric writes before the backend is asked (§2.5, D9), so the
//! backend sees only the Scopes whose Launcher lives in `settings.json`. Whether a path can
//! actually be run is the backend's question: Launcher resolution answers
//! `422 launcher_not_runnable` (architecture §5).

use claudeclaw_api::scope::Scope;
use claudeclaw_api::scopes::{
    LauncherOverview, LauncherSetResult, LauncherTestRequest, LauncherTestResult, LauncherUnset,
    ScopeKind, ScopeList,
};

use super::BackendFuture;

/// `scopes.list`'s `?kind=` filter (management-api §2.5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopeListQuery {
    /// One kind of Scope when `?kind=` named it; every Scope when it didn't.
    pub kind: Option<ScopeKind>,
}

/// The Scopes and Launchers routes (management-api §2.5). The composition root wires this to the
/// settings, the session index, Launcher resolution and the Test launcher run.
pub trait ScopesBackend: Send + Sync + 'static {
    /// `global`, every Discord channel or thread with config, a session or an override, every
    /// job and every agent, filtered by `query`.
    fn list(&self, query: ScopeListQuery) -> BackendFuture<'_, ScopeList>;

    /// The Launchers screen: the default, the picker, every Launcher card and the scope →
    /// effective-Launcher table (§2.5, #22 `launcher list`).
    fn launchers(&self) -> BackendFuture<'_, LauncherOverview>;

    /// Sets `scope`'s Launcher in `settings.json`. `launcher` is `"direct"` or an absolute path
    /// (the route has checked the shape); a path that cannot be run is
    /// `422 launcher_not_runnable`. The result says whether the Scope's next Turn resumes a
    /// session or starts one (#8).
    fn set_launcher(&self, scope: Scope, launcher: String) -> BackendFuture<'_, LauncherSetResult>;

    /// Drops `scope`'s own Launcher so it inherits again (§2.5). The route has already refused
    /// `global`, which has nothing to inherit from, and `job:` Scopes.
    fn unset_launcher(&self, scope: Scope) -> BackendFuture<'_, LauncherUnset>;

    /// Runs the Test launcher (§2.5, #40). `request.timeout_s` is already within 10–600.
    ///
    /// A request for a Launcher that is already under test **joins** that test instead of
    /// starting a second one: this seam decides, so the route calls it once per request and
    /// never starts a run of its own.
    fn test_launcher(&self, request: LauncherTestRequest) -> BackendFuture<'_, LauncherTestResult>;
}
