//! Conversations: one serial queue per Scope and the run each Turn becomes (architecture §4
//! `claudeclaw-core` `conversations`, §5 run pipeline; #8, #33, #37, #79).
//!
//! - [`prompt`]: the pure prompt shaping — the clock prefix, `<untrusted-…>` wrapping, the
//!   System-prompt file text, model choice (agentic routing) and the timeout category.

pub mod prompt;
