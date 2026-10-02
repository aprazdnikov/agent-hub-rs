//! Button presses: approvals and question answers coming back from the chat.
use hub_core::approvals;
use hub_core::questions::{self, Press as Pressed};

use crate::channel::{Shared, lock};
use crate::inbound::Press;
use crate::messenger::Format;
use crate::sender::Sender;
use crate::texts;

pub async fn handle(registries: Shared, sender: Sender, press: Press) {
    if let Some(answer) = approvals::parse_callback_data(&press.data) {
        let resolved = lock(&registries).approvals.resolve(&answer);
        let delivered = resolved.and_then(|(responder, decision)| {
            responder.send(decision.clone()).is_ok().then_some(decision)
        });
        match delivered {
            Some(decision) => {
                sender.answer(press.callback, None).await;
                if let (Some(chat), Some(message), Some(html)) =
                    (press.chat, press.message, press.html)
                {
                    let text = format!("{html}\n\n{}", texts::verdict(&decision));
                    sender.edit(chat, message, text, Format::Html).await;
                }
            }
            None => {
                sender.answer(press.callback, Some(texts::APPROVAL_STALE)).await;
            }
        }
        return;
    }
    let Some(pressed) = questions::parse_callback_data(&press.data) else {
        sender.answer(press.callback, Some(texts::QUESTION_STALE)).await;
        return;
    };
    let result = lock(&registries).questions.press(&pressed);
    match result {
        Pressed::Accepted { responder, answer } => {
            // The asking channel edits the message once it has the answer.
            let stale = responder.send(answer).is_err().then_some(texts::QUESTION_STALE);
            sender.answer(press.callback, stale).await;
        }
        Pressed::SelectionChanged { question, selected } => {
            sender.answer(press.callback, None).await;
            if let (Some(chat), Some(message)) = (press.chat, press.message) {
                let keyboard = questions::keyboard(&pressed.id, &question, &selected);
                sender.edit_keyboard(chat, message, keyboard).await;
            }
        }
        Pressed::NothingSelected => {
            sender.answer(press.callback, Some(texts::NOTHING_SELECTED)).await;
        }
        Pressed::Stale => {
            sender.answer(press.callback, Some(texts::QUESTION_STALE)).await;
        }
    }
}
