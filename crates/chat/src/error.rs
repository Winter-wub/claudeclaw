//! The seam's error types (architecture §6).

/// Any error, boxed: the source of a transport or `core` failure.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A Chat adapter's own failure: from [`Adapter::run`](crate::Adapter::run) or an
/// [`AdapterControl`](crate::AdapterControl) call (architecture §6).
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// The adapter isn't connected to its transport yet, or has stopped.
    #[error("the chat adapter is not connected")]
    NotConnected,
    /// The transport doesn't know the channel or thread, or the bot can't see it.
    #[error("{what} {id} not found")]
    NotFound { what: &'static str, id: String },
    /// The adapter's settings can't be used, such as a missing or rejected token. It won't
    /// retry until the settings change.
    #[error("chat adapter settings: {0}")]
    Settings(String),
    /// A transport call failed.
    #[error("chat transport call failed")]
    Transport(#[source] BoxError),
}

/// A failed owner notification, from [`Notifier::notify_owners`](crate::Notifier::notify_owners)
/// (architecture §6).
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    /// The adapter isn't connected, so the notice wasn't sent.
    #[error("the chat adapter is not connected")]
    NotConnected,
    /// The adapter's settings name no owner to notify.
    #[error("no owner to notify")]
    NoOwners,
    /// A transport call failed.
    #[error("chat transport call failed")]
    Transport(#[source] BoxError),
}

/// A failed [`SessionControl`](crate::SessionControl) or
/// [`LauncherControl`](crate::LauncherControl) call (architecture §6).
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// `core` refused the request; the message is for the user, such as an unrunnable
    /// Launcher or `inherit` on the Global Scope.
    #[error("{0}")]
    Rejected(String),
    /// `core` failed to carry it out.
    #[error("internal error")]
    Internal(#[source] BoxError),
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn messages() {
        let not_found = AdapterError::NotFound {
            what: "thread",
            id: "12".into(),
        };
        assert_eq!(not_found.to_string(), "thread 12 not found");
        assert_eq!(
            ControlError::Rejected("not runnable".into()).to_string(),
            "not runnable"
        );
        let io = std::io::Error::other("boom");
        let err = NotifyError::Transport(Box::new(io));
        assert_eq!(err.source().unwrap().to_string(), "boom");
    }
}
