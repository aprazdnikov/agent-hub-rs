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
use uuid::Uuid;

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

/// Contract probe for a topic removed in Telegram: creates a throwaway topic, deletes it through
/// the bot API, then checks the classifier maps the missing-topic answers to `Ok(false)`/`Ok(())`.
///
/// This test is intentionally `#[ignore]`d and is NOT run in CI or during plan execution. The
/// exact Telegram `description` it is meant to record as the live contract is confirmed only by
/// the owner (`AGENT_HUB_LIVE_CHAT` + keyring token); the matcher in `hub-telegram` is fixed by
/// normalisation until that evidence is gathered (see the plan's Task 4, owner-pending).
///
/// [`init_evidence_logging`] attaches a debug subscriber, so the run prints the `topic api
/// error` lines — the normalized descriptions (the same form `TOPIC_GONE_MARKERS` matches, no
/// token/URL per the plan constraint) — for every `Api` answer of `delete_topic`/`probe_topic`.
/// If an assert fails, those lines are the evidence of what Telegram actually answered.
#[tokio::test]
#[ignore = "creates and deletes a real forum topic in the configured Telegram group"]
async fn live_topic_gone_probe() {
    init_evidence_logging();
    let chat = ChatId(std::env::var("AGENT_HUB_LIVE_CHAT").unwrap().parse().unwrap());
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
    let thread = messenger
        .create_topic(chat, format!("probe-{}", Uuid::new_v4()))
        .await
        .expect("Telegram did not create the probe topic");
    let key = TopicKey { chat, thread };
    messenger.delete_topic(key).await.expect("the first delete of a live topic must succeed");
    // A removed topic must probe as gone and delete idempotently; the raw `description` behind
    // `Ok(false)` is the evidence the owner records against the matcher's needle list.
    let probe = messenger.probe_topic(key).await;
    let delete = messenger.delete_topic(key).await;
    tracing::info!(?probe, ?delete, "live topic-gone probe outcomes for the matcher contract");
    assert_eq!(probe, Ok(false), "a topic removed in Telegram must probe as gone");
    assert_eq!(delete, Ok(()), "deleting an already-removed topic must be idempotent");
}

/// Prints the matcher's evidence for this manual run: `hub_telegram::telegram` debug (the
/// normalized `topic api error` descriptions) plus this test's own info lines. Scoped to those
/// targets so no third-party debug output (which could carry request URLs) reaches the log.
fn init_evidence_logging() {
    use tracing_subscriber::layer::Layer as _;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target("hub_telegram::telegram", tracing::Level::DEBUG)
        .with_target(module_path!(), tracing::Level::INFO);
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_test_writer().with_filter(filter))
        .try_init();
}
