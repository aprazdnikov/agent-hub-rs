//! Send and retire one diagnostic card in an explicitly selected Telegram topic.
use hub_app::{
    dirs::AppDirs,
    secrets::{Keyring, Secret, Secrets},
};
use hub_core::{
    domain::{ChatId, ThreadId, TopicKey},
    settings::{Draft, SettingsFile},
};
use hub_telegram::{
    hub::tasks::result_buttons,
    messenger::{Messenger, Outgoing, Target},
    telegram::connect,
};

#[tokio::test]
#[ignore = "sends a real Telegram message to the supplied chat and forum topic"]
async fn live_result_card() {
    let chat = ChatId(std::env::var("AGENT_HUB_LIVE_CHAT").unwrap().parse().unwrap());
    let thread = ThreadId(std::env::var("AGENT_HUB_LIVE_TOPIC").unwrap().parse().unwrap());
    assert!(thread.0 > 0);
    let file: SettingsFile =
        toml::from_str(&std::fs::read_to_string(AppDirs::locate().unwrap().settings()).unwrap())
            .unwrap();
    assert_eq!(file.telegram.chat, Some(chat.0));
    let draft = Draft {
        token: Keyring.read(Secret::TelegramToken).unwrap().expect("missing Telegram bot token"),
        chat: chat.0.to_string(),
        workspace_root: file.workspace.root,
        users: file.telegram.users.iter().map(ToString::to_string).collect::<Vec<_>>().join(","),
        ..Default::default()
    };
    let settings = draft.parse(&std::env::temp_dir()).unwrap();
    let messenger = connect(&settings.telegram.token, chat).await.unwrap().messenger;
    let message =
        Outgoing::plain("Хаб проверяет доставку карточки с кнопками. Это тестовая задача.".into())
            .with_keyboard(result_buttons(1));
    let id = messenger
        .send(Target::Topic(TopicKey { chat, thread }), message)
        .await
        .unwrap_or_else(|_| panic!("Telegram did not confirm card delivery"));
    println!("Telegram message ID {}", id.0);
    let finished = Outgoing::plain(
        "Хаб доставил тестовую карточку с кнопками «Принять», «Открыть diff» и «Вернуть с замечанием». Проверка доставки завершена.".into()
    );
    messenger
        .edit_card(chat, id, finished)
        .await
        .unwrap_or_else(|_| panic!("Telegram did not confirm the edit"));
}
