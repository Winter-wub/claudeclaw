//! The adapter's slash commands and the interaction handler (architecture §4
//! `claudeclaw-discord`, §6). A port of the TS adapter's `registerSlashCommands`
//! (`src/commands/discord.ts:723-806`) and `handleInteractionCreate` (`:1380-1668`).
//!
//! Registration offers the commands Discord shows in `/`; handling maps each to a
//! [`SessionControl`](claudeclaw_chat::SessionControl) or
//! [`LauncherControl`](claudeclaw_chat::LauncherControl) call on `core`. Where TS read
//! Claude Code's own session JSONL for `/context` and a `settings.json` blob for `/config`, this
//! port asks `core` through the seam (§6): [`SessionControl::status`], [`SessionControl::context`]
//! and [`LauncherControl::show`]. Two TS behaviours have no seam and are not ported: setting the
//! model (`/config model`) and the secretary `sec_yes_/sec_no_` button callback.

use std::time::SystemTime;

use claudeclaw_chat::{
    ChatOutcome, ChatScope, ContextUsage, LauncherSource, LauncherView, SessionStatus,
};
use twilight_model::application::command::{
    Command, CommandOption, CommandOptionChoice, CommandOptionChoiceValue, CommandOptionType,
    CommandType,
};
use twilight_model::application::interaction::application_command::CommandOptionValue;
use twilight_model::application::interaction::{Interaction, InteractionData, InteractionType};
use twilight_model::id::Id;
use twilight_model::id::marker::CommandVersionMarker;

use crate::gateway::{Ctx, interaction_channel, interaction_scope};
use crate::triggers::is_allowed;

/// The value of the `/config profile` "direct" choice and the actual Launcher name it sets
/// (`DIRECT_CHOICE_VALUE`, `src/commands/discord.ts:701`).
const DIRECT_CHOICE_VALUE: &str = "direct";

/// The value of the `/config profile` "inherit" choice (`INHERIT_CHOICE_VALUE`,
/// `src/commands/discord.ts:702`).
const INHERIT_CHOICE_VALUE: &str = "__inherit__";

/// Discord caps a command's choices at 25 (`src/commands/discord.ts:791`).
const MAX_CHOICES: usize = 25;

/// A choice name and value may be at most 100 characters.
const CHOICE_MAX_CHARS: usize = 100;

/// `PUT /applications/{id}/commands` (`registerSlashCommands`, `src/commands/discord.ts:723`),
/// run off the gateway loop once READY has named the application. Best-effort: a failure is
/// logged, not fatal.
pub(crate) async fn register(ctx: &Ctx) {
    let Some(application) = ctx.shared.application() else {
        return;
    };
    let Ok(rest) = ctx.shared.rest() else {
        return;
    };
    // The Launcher picker (`direct` plus every path a Scope already uses) stands in for TS's CCS
    // profile list (`buildProfileChoices`, `src/commands/discord.ts:705`).
    let picker = ctx.host.launchers().picker().await.unwrap_or_default();
    let commands = slash_commands(&picker);
    match rest.set_global_commands(application, &commands).await {
        Ok(()) => tracing::info!("discord.slash_registered"),
        Err(error) => tracing::warn!(error = %error, "discord.slash_register_failed"),
    }
}

/// The command list Discord registers (`src/commands/discord.ts:728-797`).
fn slash_commands(picker: &[String]) -> Vec<Command> {
    vec![
        command(
            "start",
            "Show welcome message and usage instructions",
            Vec::new(),
        ),
        command(
            "reset",
            "Reset the global session for a fresh start",
            Vec::new(),
        ),
        command(
            "compact",
            "Compact session to reduce context size",
            Vec::new(),
        ),
        command("status", "Show current session status", Vec::new()),
        command("context", "Show context window usage", Vec::new()),
        command(
            "config",
            "Show or change the model and CCS profile",
            vec![
                subcommand("show", "Show the current model and runtime launcher", None),
                subcommand(
                    "profile",
                    "Switch the runtime launcher (a CCS profile, or direct claude)",
                    Some(profile_value_option(picker)),
                ),
            ],
        ),
    ]
}

/// One `type: 1` chat-input command.
// `Command::dm_permission` is deprecated in favour of `contexts`, but the field still must be
// set when building the struct by hand (twilight ships no builder); `None` serializes nothing.
#[allow(deprecated)]
fn command(name: &str, description: &str, options: Vec<CommandOption>) -> Command {
    Command {
        application_id: None,
        contexts: None,
        default_member_permissions: None,
        dm_permission: None,
        description: description.to_owned(),
        description_localizations: None,
        guild_id: None,
        id: None,
        integration_types: None,
        kind: CommandType::ChatInput,
        name: name.to_owned(),
        name_localizations: None,
        nsfw: None,
        options,
        version: Id::<CommandVersionMarker>::new(1),
    }
}

/// One `type: 1` subcommand, with a single nested option when it takes a value.
fn subcommand(name: &str, description: &str, value: Option<CommandOption>) -> CommandOption {
    CommandOption {
        autocomplete: None,
        channel_types: None,
        choices: None,
        description: description.to_owned(),
        description_localizations: None,
        kind: CommandOptionType::SubCommand,
        max_length: None,
        max_value: None,
        min_length: None,
        min_value: None,
        name: name.to_owned(),
        name_localizations: None,
        options: value.map(|option| vec![option]),
        required: None,
    }
}

/// The required `value` string option under `/config profile` (`src/commands/discord.ts:779-792`):
/// `inherit`, `direct`, then every Launcher the picker knows, capped at Discord's 25.
fn profile_value_option(picker: &[String]) -> CommandOption {
    let mut choices = vec![
        choice("inherit — follow the global default", INHERIT_CHOICE_VALUE),
        choice("direct — plain claude binary", DIRECT_CHOICE_VALUE),
    ];
    for launcher in picker {
        if launcher == DIRECT_CHOICE_VALUE
            || launcher.chars().count() > CHOICE_MAX_CHARS
            || choices
                .iter()
                .any(|existing| matches!(&existing.value, CommandOptionChoiceValue::String(v) if v == launcher))
        {
            continue;
        }
        choices.push(choice(launcher, launcher));
    }
    choices.truncate(MAX_CHOICES);
    CommandOption {
        autocomplete: None,
        channel_types: None,
        choices: Some(choices),
        description: "CCS profile, or 'direct' for the plain claude binary".to_owned(),
        description_localizations: None,
        kind: CommandOptionType::String,
        max_length: None,
        max_value: None,
        min_length: None,
        min_value: None,
        name: "value".to_owned(),
        name_localizations: None,
        options: None,
        required: Some(true),
    }
}

fn choice(name: &str, value: &str) -> CommandOptionChoice {
    CommandOptionChoice {
        name: name.to_owned(),
        name_localizations: None,
        value: CommandOptionChoiceValue::String(value.to_owned()),
    }
}

/// One interaction (`handleInteractionCreate`, `src/commands/discord.ts:1380`). Unauthorized
/// callers get an ephemeral refusal; commands route to [`handle_command`]; every other type —
/// buttons, modals — gets the default `OK` ack (`:1661-1667`).
pub(crate) async fn handle_interaction(ctx: Ctx, interaction: Interaction) {
    let settings = ctx.shared.settings();
    if !is_allowed(interaction.author_id(), &settings.allowed_user_ids) {
        respond(&ctx, &interaction, "Unauthorized.", true).await;
        return;
    }
    if interaction.kind == InteractionType::ApplicationCommand
        && let Some(InteractionData::ApplicationCommand(data)) = &interaction.data
        && !data.name.is_empty()
    {
        handle_command(&ctx, &interaction, &data.name, &data.options).await;
        return;
    }
    respond(&ctx, &interaction, "OK", true).await;
}

/// One slash command (`src/commands/discord.ts:1390-1627`).
async fn handle_command(
    ctx: &Ctx,
    interaction: &Interaction,
    name: &str,
    options: &[twilight_model::application::interaction::application_command::CommandDataOption],
) {
    let scope = interaction_scope(&ctx.shared, interaction);
    let is_guild = is_guild_command(interaction);
    match name {
        "start" => {
            respond(
                ctx,
                interaction,
                "Hello! Send me a message and I'll respond using Claude.\nUse `/reset` to \
                 start a fresh session.",
                false,
            )
            .await;
        }
        "reset" => reset(ctx, interaction, scope, is_guild).await,
        "compact" => compact(ctx, interaction, scope).await,
        "status" => status(ctx, interaction, scope).await,
        "context" => context(ctx, interaction, scope).await,
        "config" => config(ctx, interaction, scope, options, is_guild).await,
        _ => respond(ctx, interaction, "Unknown command.", false).await,
    }
}

/// `/reset` (`src/commands/discord.ts:1398-1411`): end the Scope's session so its next Turn
/// starts new.
async fn reset(ctx: &Ctx, interaction: &Interaction, scope: ChatScope, is_guild: bool) {
    match ctx.host.sessions().reset(scope).await {
        Ok(()) => {
            let text = if is_guild {
                "Channel session reset. Next message starts fresh."
            } else {
                "Global session reset. Next message starts fresh."
            };
            respond(ctx, interaction, text, false).await;
        }
        Err(error) => {
            respond(ctx, interaction, &format!("Reset failed: {error}"), true).await;
        }
    }
}

/// `/compact` (`src/commands/discord.ts:1413-1430`): acknowledge, run compact, then edit that
/// acknowledgment in place with `core`'s outcome.
async fn compact(ctx: &Ctx, interaction: &Interaction, scope: ChatScope) {
    respond(ctx, interaction, "⏳ Compacting session...", false).await;
    let text = match ctx.host.sessions().compact(scope).await {
        Ok(ChatOutcome { text, .. }) => text,
        Err(error) => format!("Compact failed: {error}"),
    };
    if let (Some(application), Ok(rest)) = (ctx.shared.application(), ctx.shared.rest()) {
        let _ = rest
            .update_interaction_response(application, &interaction.token, &text)
            .await;
    }
}

/// `/status` (`src/commands/discord.ts:1532-1564`).
async fn status(ctx: &Ctx, interaction: &Interaction, scope: ChatScope) {
    match ctx.host.sessions().status(scope).await {
        Ok(None) => respond(ctx, interaction, "📊 No active session.", false).await,
        Ok(Some(status)) => {
            respond(ctx, interaction, &status_message(&status), false).await;
        }
        Err(error) => {
            respond(
                ctx,
                interaction,
                &format!("Status unavailable: {error}"),
                true,
            )
            .await;
        }
    }
}

/// `/context` (`src/commands/discord.ts:1566-1623`).
async fn context(ctx: &Ctx, interaction: &Interaction, scope: ChatScope) {
    match ctx.host.sessions().status(scope.clone()).await {
        Ok(None) => {
            respond(ctx, interaction, "No active session.", false).await;
        }
        Ok(Some(status)) => match ctx.host.sessions().context(scope).await {
            Ok(Some(usage)) => {
                respond(
                    ctx,
                    interaction,
                    &context_message(&usage, status.turns),
                    false,
                )
                .await;
            }
            Ok(None) => {
                respond(ctx, interaction, "No usage data found.", false).await;
            }
            Err(error) => {
                respond(
                    ctx,
                    interaction,
                    &format!("Failed to read context: {error}"),
                    false,
                )
                .await;
            }
        },
        Err(error) => {
            respond(
                ctx,
                interaction,
                &format!("Failed to read context: {error}"),
                false,
            )
            .await;
        }
    }
}

/// `/config` (`src/commands/discord.ts:1432-1530`). `show` (or no subcommand) renders the
/// Scope's effective Launcher; `profile` switches it and resets the Scope's session so the next
/// message does not resume across runtimes. Setting the model has no seam and is not offered.
async fn config(
    ctx: &Ctx,
    interaction: &Interaction,
    scope: ChatScope,
    options: &[twilight_model::application::interaction::application_command::CommandDataOption],
    is_guild: bool,
) {
    let sub = options.first();
    match sub.map(|option| option.name.as_str()) {
        None | Some("show") => {
            respond_config(ctx, interaction, scope, is_guild, None).await;
        }
        Some("profile") => {
            let value = sub
                .and_then(|option| match &option.value {
                    CommandOptionValue::SubCommand(inner) => inner.first(),
                    _ => None,
                })
                .and_then(|option| match &option.value {
                    CommandOptionValue::String(value) => Some(value.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            if value == INHERIT_CHOICE_VALUE {
                if !is_guild {
                    respond(
                        ctx,
                        interaction,
                        "`inherit` only applies inside a channel — the global default has nothing \
                         to inherit from.",
                        true,
                    )
                    .await;
                    return;
                }
                match ctx.host.launchers().inherit(scope.clone()).await {
                    Ok(view) => {
                        let _ = ctx.host.sessions().reset(scope.clone()).await;
                        let note = "Override cleared — this channel now follows the global \
                                    default. Channel session reset.";
                        respond_config(ctx, interaction, scope, is_guild, Some((&view, note)))
                            .await;
                    }
                    Err(error) => {
                        respond(
                            ctx,
                            interaction,
                            &format!("Config change failed: {error}"),
                            true,
                        )
                        .await;
                    }
                }
                return;
            }
            let launcher = if value == DIRECT_CHOICE_VALUE {
                DIRECT_CHOICE_VALUE.to_owned()
            } else {
                value.to_owned()
            };
            match ctx
                .host
                .launchers()
                .set(scope.clone(), launcher, true)
                .await
            {
                Ok(view) => {
                    let _ = ctx.host.sessions().reset(scope.clone()).await;
                    let note = if is_guild {
                        "Switched — channel session reset, next message starts fresh."
                    } else {
                        "Switched — global session reset, next message starts fresh."
                    };
                    respond_config(ctx, interaction, scope, is_guild, Some((&view, note))).await;
                }
                Err(error) => {
                    respond(
                        ctx,
                        interaction,
                        &format!("Config change failed: {error}"),
                        true,
                    )
                    .await;
                }
            }
        }
        Some(other) => {
            respond(
                ctx,
                interaction,
                &format!("Unknown subcommand: `{other}`"),
                true,
            )
            .await;
        }
    }
}

/// Render the effective Launcher for [`config`], optionally appending a note after a change.
async fn respond_config(
    ctx: &Ctx,
    interaction: &Interaction,
    scope: ChatScope,
    is_guild: bool,
    note: Option<(&LauncherView, &str)>,
) {
    let view = match note {
        Some((view, _)) => view.clone(),
        None => match ctx.host.launchers().show(scope).await {
            Ok(view) => view,
            Err(error) => {
                respond(
                    ctx,
                    interaction,
                    &format!("Config change failed: {error}"),
                    true,
                )
                .await;
                return;
            }
        },
    };
    let scope_label = if is_guild { "this channel" } else { "global" };
    let origin = launcher_origin(is_guild, view.source);
    let mut text = config_message(&view, scope_label, origin);
    if let Some((_, note)) = note {
        text.push_str("\n\n");
        text.push_str(note);
    }
    respond(ctx, interaction, &text, true).await;
}

/// The `/status` body (`src/commands/discord.ts:1543-1552`), with the fields the seam carries.
/// TS also prints the security level and the compact-warned flag, which `core` does not expose
/// here; the Launcher line stands in their place.
fn status_message(status: &SessionStatus) -> String {
    [
        "📊 **Session Status**".to_owned(),
        format!("Session: `{}`", short_id(&status.session_id)),
        format!("Turns: {}", status.turns),
        format!("Model: {}", status.model.as_deref().unwrap_or("default")),
        format!("Launcher: `{}`", status.launcher),
        format!("Created: {}", age(status.created_at)),
        format!("Last used: {}", age(status.last_used_at)),
    ]
    .join("\n")
}

/// The `/context` body (`src/commands/discord.ts:1604-1615`). The window comes from the Daemon's
/// own stream ([`ContextUsage::window_tokens`], #39), not TS's hard-coded 200000; cumulative
/// output tokens are not exposed by the seam and are omitted.
fn context_message(usage: &ContextUsage, turns: u32) -> String {
    let total = usage.used_tokens();
    let window = usage.window_tokens;
    let ratio = if window == 0 {
        0.0
    } else {
        (total as f64 / window as f64).min(1.0)
    };
    let percent = ratio * 100.0;
    let filled = (ratio * 20.0).round() as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(20 - filled));
    [
        "📐 **Context Window**".to_owned(),
        format!("{bar} {percent:.1}%"),
        String::new(),
        format!(
            "Total: `{}` / `{}` tokens",
            thousands(total),
            thousands(window)
        ),
        format!("├ Input: `{}`", thousands(usage.input_tokens)),
        format!(
            "├ Cache creation: `{}`",
            thousands(usage.cache_write_tokens)
        ),
        format!("└ Cache read: `{}`", thousands(usage.cache_read_tokens)),
        String::new(),
        format!("Turns: {turns}"),
    ]
    .join("\n")
}

/// The `/config` body (`describe`, `src/commands/discord.ts:1434-1440`).
fn config_message(view: &LauncherView, scope_label: &str, origin: &str) -> String {
    format!(
        "⚙️ **Runtime configuration**\nModel: `{}`\nLauncher: `{}`\nScope: `{scope_label}`\n{origin}",
        view.model.as_deref().unwrap_or("default"),
        view.launcher,
    )
}

/// Where the Scope's Launcher comes from (`src/commands/discord.ts:1451-1456`).
fn launcher_origin(is_guild: bool, source: LauncherSource) -> &'static str {
    if !is_guild {
        "_Global default._"
    } else if source == LauncherSource::Scope {
        "_Channel override._"
    } else {
        "_Inherited from the global default._"
    }
}

/// TS's `isGuildCmd`: a command used in a guild channel rather than a DM
/// (`src/commands/discord.ts:1399`).
fn is_guild_command(interaction: &Interaction) -> bool {
    interaction.guild_id.is_some() && interaction_channel(interaction).is_some()
}

/// Send an interaction response, best-effort (`respondToInteraction`,
/// `src/commands/discord.ts:810`).
async fn respond(ctx: &Ctx, interaction: &Interaction, content: &str, ephemeral: bool) {
    let (Some(application), Ok(rest)) = (ctx.shared.application(), ctx.shared.rest()) else {
        return;
    };
    let _ = rest
        .respond_to_interaction(
            application,
            interaction.id,
            &interaction.token,
            content,
            ephemeral,
        )
        .await;
}

/// The first eight characters of a session id (`sessionId.slice(0, 8)`,
/// `src/commands/discord.ts:1545`).
fn short_id(id: &str) -> &str {
    match id.char_indices().nth(8) {
        Some((index, _)) => &id[..index],
        None => id,
    }
}

/// A relative age ("3m ago") for the `/status` timestamps, where TS printed the raw ISO date.
fn age(then: SystemTime) -> String {
    let seconds = SystemTime::now()
        .duration_since(then)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    if seconds < 60 {
        "just now".to_owned()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

/// `n.toLocaleString("en-US")` (`src/commands/discord.ts:1608`): comma-grouped thousands.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (len - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use claudeclaw_chat::{ContextUsage, LauncherSource, LauncherView, SessionStatus};
    use twilight_model::application::command::{CommandOptionType, CommandType};

    use super::*;

    fn session() -> SessionStatus {
        SessionStatus {
            session_id: "abcdef0123456789".to_owned(),
            launcher: "direct".to_owned(),
            model: None,
            turns: 7,
            created_at: SystemTime::now() - Duration::from_secs(3 * 86_400 + 10),
            last_used_at: SystemTime::now() - Duration::from_secs(120),
        }
    }

    #[test]
    fn slash_commands_shape_matches_ts() {
        let commands = slash_commands(&["direct".to_owned(), "/usr/local/bin/claude".to_owned()]);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["start", "reset", "compact", "status", "context", "config"]
        );
        for command in &commands {
            assert_eq!(command.kind, CommandType::ChatInput);
        }
        let config = commands.last().unwrap();
        let subs: Vec<&str> = config
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect();
        assert_eq!(subs, ["show", "profile"]);
        assert_eq!(config.options[0].kind, CommandOptionType::SubCommand);

        let profile = &config.options[1];
        let value = &profile.options.as_ref().unwrap()[0];
        assert_eq!(value.kind, CommandOptionType::String);
        assert_eq!(value.required, Some(true));
        let choices = value.choices.as_ref().unwrap();
        let values: Vec<&str> = choices
            .iter()
            .map(|choice| match &choice.value {
                CommandOptionChoiceValue::String(value) => value.as_str(),
                _ => "",
            })
            .collect();
        // inherit, direct, then the one non-direct picker entry; the duplicate direct is dropped.
        assert_eq!(values, ["__inherit__", "direct", "/usr/local/bin/claude"]);
    }

    #[test]
    fn profile_choices_are_capped_and_bounded() {
        let mut picker = vec!["direct".to_owned()];
        for index in 0..40 {
            picker.push(format!("/bin/tool{index}"));
        }
        // One over Discord's 100-character choice limit is dropped.
        picker.push(format!("/{}", "x".repeat(200)));
        let choices = profile_value_option(&picker).choices.unwrap();
        assert_eq!(choices.len(), MAX_CHOICES);
        let values: Vec<&str> = choices
            .iter()
            .map(|choice| match &choice.value {
                CommandOptionChoiceValue::String(value) => value.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(values[0], "__inherit__");
        assert_eq!(values[1], "direct");
        assert!(!values.contains(&"/bin/tool39"));
    }

    #[test]
    fn status_message_uses_the_relative_age_and_model_default() {
        let message = status_message(&session());
        assert!(message.contains("Session: `abcdef01`"), "{message}");
        assert!(message.contains("Turns: 7"), "{message}");
        assert!(message.contains("Model: default"), "{message}");
        assert!(message.contains("Launcher: `direct`"), "{message}");
        assert!(message.contains("Created: 3d ago"), "{message}");
        assert!(message.contains("Last used: 2m ago"), "{message}");
    }

    #[test]
    fn context_message_draws_the_bar_and_groups_digits() {
        let usage = ContextUsage {
            input_tokens: 1_234,
            cache_write_tokens: 56_000,
            cache_read_tokens: 42_766,
            window_tokens: 200_000,
        };
        let message = context_message(&usage, 3);
        // total = 100_000, half the window: 50.0%, ten filled cells.
        assert!(message.contains("██████████░░░░░░░░░░ 50.0%"), "{message}");
        assert!(
            message.contains("Total: `100,000` / `200,000` tokens"),
            "{message}"
        );
        assert!(message.contains("├ Input: `1,234`"), "{message}");
        assert!(message.contains("├ Cache creation: `56,000`"), "{message}");
        assert!(message.contains("└ Cache read: `42,766`"), "{message}");
        assert!(message.contains("Turns: 3"), "{message}");
    }

    #[test]
    fn context_message_handles_a_zero_window() {
        let usage = ContextUsage {
            input_tokens: 0,
            cache_write_tokens: 0,
            cache_read_tokens: 0,
            window_tokens: 0,
        };
        let message = context_message(&usage, 0);
        assert!(message.contains("░░░░░░░░░░░░░░░░░░░░ 0.0%"), "{message}");
        assert!(message.contains("Total: `0` / `0` tokens"), "{message}");
    }

    #[test]
    fn config_message_reports_the_launcher_and_origin() {
        let view = LauncherView {
            launcher: "/opt/claude".to_owned(),
            source: LauncherSource::ParentChannel,
            model: Some("claude-sonnet".to_owned()),
        };
        let message = config_message(&view, "this channel", launcher_origin(true, view.source));
        assert!(message.contains("Model: `claude-sonnet`"), "{message}");
        assert!(message.contains("Launcher: `/opt/claude`"), "{message}");
        assert!(message.contains("Scope: `this channel`"), "{message}");
        assert!(
            message.contains("_Inherited from the global default._"),
            "{message}"
        );

        assert_eq!(
            launcher_origin(true, LauncherSource::Scope),
            "_Channel override._"
        );
        assert_eq!(
            launcher_origin(false, LauncherSource::Global),
            "_Global default._"
        );
    }

    #[test]
    fn thousands_groups_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn short_id_never_splits_a_character() {
        assert_eq!(short_id("abcdef01"), "abcdef01");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
    }

    #[test]
    fn age_is_relative_and_never_negative() {
        assert_eq!(age(SystemTime::now()), "just now");
        assert_eq!(age(SystemTime::now() + Duration::from_secs(60)), "just now");
        assert_eq!(age(SystemTime::now() - Duration::from_secs(90)), "1m ago");
        assert_eq!(
            age(SystemTime::now() - Duration::from_secs(7_200)),
            "2h ago"
        );
        assert_eq!(
            age(SystemTime::now() - Duration::from_secs(2 * 86_400)),
            "2d ago"
        );
    }

    /// A `Command` round-trips through the JSON Discord would receive.
    #[test]
    fn commands_serialize_with_a_version() {
        let commands = slash_commands(&[]);
        let json = serde_json::to_value(&commands).unwrap();
        let config = &json[5];
        assert_eq!(config["name"], "config");
        assert_eq!(config["type"], 1);
        assert_eq!(config["version"], "1");
        assert_eq!(config["options"][1]["options"][0]["name"], "value");
        assert_eq!(config["options"][1]["options"][0]["required"], true);
    }
}
