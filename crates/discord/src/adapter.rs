//! The Discord chat adapter (architecture §4 `claudeclaw-discord`): the value `core` registers
//! and runs. A port of the TS adapter's `startDiscord`/`stopDiscord` glue
//! (`src/commands/discord.ts:1707-2013`), minus the WebSocket plumbing twilight's
//! [`Shard`](twilight_gateway::Shard) provides.
//!
//! [`DiscordAdapter::new`] takes the seams core hands it: where to connect
//! ([`DiscordEndpoints`]), the outbound side ([`ReplySink`], [`AttachmentFetcher`]) and the
//! [`Rest`] client and owner ids the outbound [`Notifier`] DMs through. The gateway's own token
//! is not a field: it is read from [`ChatHost::settings`] inside [`run`](Adapter::run), which is
//! also where the adapter learns the host it talks back to.

use std::sync::Arc;

use claudeclaw_chat::{Adapter, AdapterControl, AdapterError, ChatHost, Notifier, Transport};
use tokio_util::sync::CancellationToken;

use crate::control::DiscordControl;
use crate::gateway::{self, Ctx, Shared};
use crate::notifier::DiscordNotifier;
use crate::rest::Rest;
use crate::seams::{AttachmentFetcher, DiscordEndpoints, ReplySink};

/// The Discord transport's [`Adapter`]. One value owns the shared cache the gateway fills and
/// the [`AdapterControl`]/[`Notifier`] read, so `core` can call them while the gateway runs.
pub struct DiscordAdapter {
    shared: Arc<Shared>,
    endpoints: DiscordEndpoints,
    rest: Arc<Rest>,
    owners: Vec<String>,
    reply: Arc<dyn ReplySink>,
    fetcher: Arc<dyn AttachmentFetcher>,
}

impl DiscordAdapter {
    /// A not-yet-connected adapter. `endpoints` normally default to Discord's own; the Parity
    /// suite redirects them (#18). `rest` and `owners` build the outbound [`DiscordNotifier`].
    pub fn new(
        endpoints: DiscordEndpoints,
        rest: Arc<Rest>,
        owners: Vec<String>,
        reply: Arc<dyn ReplySink>,
        fetcher: Arc<dyn AttachmentFetcher>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared::new()),
            endpoints,
            rest,
            owners,
            reply,
            fetcher,
        }
    }
}

impl Adapter for DiscordAdapter {
    const TRANSPORT: Transport = Transport::Discord;

    fn notifier(&self) -> Arc<dyn Notifier> {
        Arc::new(DiscordNotifier::new(Arc::clone(&self.rest), &self.owners))
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
