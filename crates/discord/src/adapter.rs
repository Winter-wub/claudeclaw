//! The Discord chat adapter (architecture §4 `claudeclaw-discord`): the value `core` registers
//! and runs. A port of the TS adapter's `startDiscord`/`stopDiscord` glue
//! (`src/commands/discord.ts:1707-2013`), minus the WebSocket plumbing twilight's
//! [`Shard`](twilight_gateway::Shard) provides.
//!
//! [`DiscordAdapter::new`] takes only the seams core hands it: where to connect
//! ([`DiscordEndpoints`]) and the outbound side ([`ReplySink`], [`AttachmentFetcher`]). The token
//! is not a field — it arrives through [`ChatHost::settings`] inside [`run`](Adapter::run), which
//! is also where the adapter learns the host it talks back to.

use std::sync::Arc;

use claudeclaw_chat::{Adapter, AdapterControl, AdapterError, ChatHost, Notifier, Transport};
use tokio_util::sync::CancellationToken;

use crate::control::DiscordControl;
use crate::gateway::{self, Ctx, Shared};
use crate::seams::{AttachmentFetcher, DiscordEndpoints, ReplySink};

/// The Discord transport's [`Adapter`]. One value owns the shared cache the gateway fills and
/// the [`AdapterControl`]/[`Notifier`] read, so `core` can call them while the gateway runs.
pub struct DiscordAdapter {
    shared: Arc<Shared>,
    endpoints: DiscordEndpoints,
    reply: Arc<dyn ReplySink>,
    fetcher: Arc<dyn AttachmentFetcher>,
}

impl DiscordAdapter {
    /// A not-yet-connected adapter. `endpoints` normally default to Discord's own; the Parity
    /// suite redirects them (#18).
    pub fn new(
        endpoints: DiscordEndpoints,
        reply: Arc<dyn ReplySink>,
        fetcher: Arc<dyn AttachmentFetcher>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared::new()),
            endpoints,
            reply,
            fetcher,
        }
    }
}

impl Adapter for DiscordAdapter {
    const TRANSPORT: Transport = Transport::Discord;

    fn notifier(&self) -> Arc<dyn Notifier> {
        gateway::notifier(Arc::clone(&self.shared))
    }

    fn control(&self) -> Arc<dyn AdapterControl> {
        Arc::new(DiscordControl::new(Arc::clone(&self.shared)))
    }

    async fn run(
        self,
        host: Arc<dyn ChatHost>,
        shutdown: CancellationToken,
    ) -> Result<(), AdapterError> {
        let ctx = Ctx {
            shared: self.shared,
            host,
            reply: self.reply,
            fetcher: self.fetcher,
        };
        gateway::run(ctx, self.endpoints, shutdown).await
    }
}
