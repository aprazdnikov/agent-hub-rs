//! Authoritative slash-command menus for the configured group.

use std::time::Duration;

use hub_core::commands::Command;
use hub_core::settings::TelegramSettings;
use teloxide::payloads::{GetMyCommandsSetters, SetMyCommandsSetters};
use teloxide::prelude::{Bot, Requester};
use teloxide::types::{BotCommand, BotCommandScope, ChatId, UserId};

const LANGUAGES: [&str; 3] = ["", "ru", "en"];
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum MenuError {
    #[error("Telegram command menu synchronization timed out after 30 s")]
    Timeout,
    #[error("Telegram command menu {operation} failed")]
    Request { operation: &'static str },
    #[error("Telegram command menu readback differs from the source registry")]
    Mismatch,
}

/// Fully replaces managed menus; callers may keep polling when registration fails.
/// Does not change command authorization or global menus for other chats.
/// Covers the group, its administrators and allowlisted members, in fallback/ru/en.
/// Bot API cannot enumerate arbitrary old language/member scopes.
pub async fn synchronize_commands(bot: &Bot, settings: &TelegramSettings) -> Result<(), MenuError> {
    tokio::time::timeout(SYNC_TIMEOUT, replace_and_verify(bot, settings))
        .await
        .map_err(|_| MenuError::Timeout)?
}

async fn replace_and_verify(bot: &Bot, settings: &TelegramSettings) -> Result<(), MenuError> {
    let chat_id = ChatId(settings.chat.0);
    let scopes = [
        BotCommandScope::Chat { chat_id: chat_id.into() },
        BotCommandScope::ChatAdministrators { chat_id: chat_id.into() },
    ]
    .into_iter()
    .chain(settings.users.iter().map(|user| BotCommandScope::ChatMember {
        chat_id: chat_id.into(),
        user_id: UserId(user.0),
    }));
    let commands: Vec<_> = Command::ALL
        .into_iter()
        .map(|command| BotCommand::new(command.name(), command.description()))
        .collect();
    for scope in scopes {
        for language in LANGUAGES {
            // setMyCommands replaces the entire list for this exact scope/language.
            // No read/merge with BotFather and no empty-menu interval from delete+set.
            bot.set_my_commands(commands.clone())
                .scope(scope.clone())
                .language_code(language)
                .await
                // RequestError can contain a URL with the bot token; never expose it in logs.
                .map_err(|_| MenuError::Request { operation: "setMyCommands" })?;

            let found = bot
                .get_my_commands()
                .scope(scope.clone())
                .language_code(language)
                .await
                .map_err(|_| MenuError::Request { operation: "getMyCommands" })?;
            if found != commands {
                return Err(MenuError::Mismatch);
            }
        }
    }
    Ok(())
}
