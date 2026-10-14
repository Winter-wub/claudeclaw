//! Which transport a message came through, and where in it (architecture §6).

use std::fmt;

/// A chat transport (architecture §6). Discord is built now; Telegram and Slack are phase 2.
///
/// This is the seam's own type: `chat` depends on nothing in the workspace (§3.1), so it does
/// not reuse `claudeclaw-api`'s wire enum of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Transport {
    Discord,
    Telegram,
    Slack,
}

impl Transport {
    /// Every transport, in declaration order.
    pub const ALL: [Transport; 3] = [Transport::Discord, Transport::Telegram, Transport::Slack];

    /// The transport's lowercase name: the `inbox/<transport>/` directory (§6) and the
    /// `adapter:<transport>` health beacon (§4 `claudeclaw-core`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::Discord => "discord",
            Transport::Telegram => "telegram",
            Transport::Slack => "slack",
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a Chat adapter names where a message came from (architecture §6). `core` maps it to a
/// Scope; IDs are the transport's own, as strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChatScope {
    /// A direct message, which maps to the Global Scope (TS parity).
    Direct { transport: Transport },
    /// A channel (a Discord or Slack channel, a Telegram group).
    Channel { transport: Transport, id: String },
    /// A thread under the channel `parent`.
    Thread {
        transport: Transport,
        id: String,
        parent: String,
    },
}

impl ChatScope {
    /// The transport the message came through.
    pub fn transport(&self) -> Transport {
        match self {
            ChatScope::Direct { transport }
            | ChatScope::Channel { transport, .. }
            | ChatScope::Thread { transport, .. } => *transport,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_names() {
        let names: Vec<_> = Transport::ALL.iter().map(|t| t.to_string()).collect();
        assert_eq!(names, ["discord", "telegram", "slack"]);
    }

    #[test]
    fn scope_reports_its_transport() {
        let scopes = [
            ChatScope::Direct {
                transport: Transport::Telegram,
            },
            ChatScope::Channel {
                transport: Transport::Slack,
                id: "12".into(),
            },
            ChatScope::Thread {
                transport: Transport::Discord,
                id: "34".into(),
                parent: "12".into(),
            },
        ];
        let got: Vec<_> = scopes.iter().map(ChatScope::transport).collect();
        assert_eq!(
            got,
            [Transport::Telegram, Transport::Slack, Transport::Discord]
        );
    }
}
