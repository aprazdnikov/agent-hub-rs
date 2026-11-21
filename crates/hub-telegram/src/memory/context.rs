//! Explicit context form; no model calls, source lookup or automatic handoff.
use hub_memory::{ContextBrief, ContextPacket, ContextRequest, ContextSelection};
use std::collections::BTreeMap;

pub const HELP: &str = "Пакет контекста: заполните форму (каждое поле с новой строки). Источники не проверяются; полномочия не передаются.\n\n/memory context\nЗаписи: ключ@1 другой-ключ@2\nЦель: что нужно сделать\nОграничения: что нужно соблюдать\nСостояние: что уже сделано\nСледующий шаг: с чего продолжить\nИсточник: поручение пользователя или документ\nБюджет: 16384\n\nСтроку «Источник:» можно повторить до 8 раз. Бюджет — байты UTF-8, от 1024 до 65536; по умолчанию 16384. Сначала будет предварительный просмотр.";

pub fn parse(form: &str) -> Result<ContextRequest, String> {
    if form.len() > 32768 {
        return Err("Форма пакета слишком длинная: максимум 32768 байт.".into());
    }
    let mut values = BTreeMap::new();
    let mut sources = Vec::new();
    for line in form.lines().filter(|line| !line.trim().is_empty()) {
        let (name,value)=line.split_once(':').ok_or("Каждое поле формы должно начинаться с названия и двоеточия. Используйте /memory context для образца.")?;
        let name = name.trim();
        let value = value.trim();
        match name {
            "Источник" if sources.len()<8 => sources.push(value.to_owned()),
            "Записи"|"Цель"|"Ограничения"|"Состояние"|"Следующий шаг"|"Бюджет" => {
                if values.insert(name,value).is_some() { return Err(format!("Поле «{name}» указано несколько раз. Оставьте одно значение.")); }
            }
            _ => return Err("Неизвестное поле или слишком много источников. Используйте форму из /memory context.".into()),
        }
    }
    let get = |name| values.get(name).copied().ok_or_else(|| format!("Заполните поле «{name}»."));
    let selections = get("Записи")?
        .split_whitespace()
        .map(|item| {
            let (key, revision) = item
                .rsplit_once('@')
                .ok_or("Укажите каждую запись в виде ключ@ревизия, например pilot@4.")?;
            Ok(ContextSelection {
                key: key.into(),
                revision: revision
                    .parse()
                    .map_err(|_| "Номер ревизии должен быть целым числом от 1.")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ContextRequest {
        brief: ContextBrief {
            goal: get("Цель")?.into(),
            constraints: get("Ограничения")?.into(),
            state: get("Состояние")?.into(),
            next_step: get("Следующий шаг")?.into(),
            sources,
        },
        selections,
        max_bytes: values.get("Бюджет").map_or(Ok(16384), |value| {
            value.parse::<usize>().map_err(|_| "Бюджет должен быть целым числом байт.")
        })?,
    })
}
#[must_use]
pub fn summary(packet: &ContextPacket) -> String {
    let keys = packet
        .selections()
        .iter()
        .map(|s| format!("{}@{}", s.key, s.revision))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Пакет контекста\nПроект: {}\nРазмер: {} байт UTF-8\nЗаписи: {keys}\n{}\n\nПеред выдачей проверяется, что память не изменилась. Затем передайте текст выбранному агенту обычным сообщением; /handoff и разрешения сессии не изменяются.",
        packet.project().display(),
        packet.text().len(),
        packet.warnings().join("\n")
    )
}

pub(crate) fn parts(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut units = 0;
        let cut = rest
            .char_indices()
            .find_map(|(offset, ch)| {
                units += ch.len_utf16();
                (units > 3500).then_some(offset)
            })
            .unwrap_or(rest.len());
        // cut is a UTF-8 character boundary returned by char_indices, or the full length.
        let (part, tail) = rest.split_at(cut);
        result.push(part);
        rest = tail;
    }
    result
}
async fn send(
    sender: &crate::sender::Sender,
    key: hub_core::domain::TopicKey,
    text: &str,
) -> Result<(), String> {
    let chunks = parts(text);
    for (index, chunk) in chunks.iter().enumerate() {
        let label = format!(
            "Пакет контекста — часть {} из {}. Используйте только полный набор частей.\n\n{chunk}",
            index + 1,
            chunks.len()
        );
        if sender
            .one(crate::messenger::Target::Topic(key), crate::messenger::Outgoing::plain(label))
            .await
            .is_none()
        {
            return Err(format!(
                "Не удалось отправить часть {} из {}. Не используйте неполный пакет; повторите выдачу целиком.",
                index + 1,
                chunks.len()
            ));
        }
    }
    Ok(())
}
impl super::Browser {
    pub(super) fn deliver_context(&mut self, binding: super::Binding, packet: ContextPacket) {
        self.busy = true;
        let epoch = self.epoch;
        let cancel = self.cancel.clone();
        let sender = self.sender.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let sent = tokio::select! {
                biased;
                ()=cancel.cancelled()=>Err("Выдача пакета отменена. Не используйте ранее полученные части как полный пакет.".into()),
                result=tokio::time::timeout(std::time::Duration::from_secs(30),send(&sender,binding.key,packet.text()))=>match result {
                    Ok(result)=>result,
                    Err(_)=>Err("Отправка пакета превысила 30 с. Часть сообщений могла быть доставлена; используйте только полный набор частей.".into()),
                },
            };
            let result=sent.map(|()|super::View::Notice("Пакет передан полностью. Перешлите все его части выбранному агенту. Свежесть проверена перед выдачей; разрешения сессии не передаются.".into()));
            let _ = mailbox
                .send(super::HubMessage::MemoryDone(super::Done { epoch, binding, result }))
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbered_parts_preserve_all_bytes_and_unicode_boundaries() {
        let text =
            format!("  {}\n\n{}  ", "🦀Привет".repeat(1600), "Ограничения: ничего не удалять.");
        let parts = parts(&text);
        assert!(parts.len() > 1);
        assert_eq!(parts.concat(), text);
        assert!(parts.iter().all(|part| part.encode_utf16().count() <= 3500));
    }
    #[tokio::test]
    async fn delivery_stops_at_failed_part_and_never_reports_complete() {
        use crate::{messenger::SendError, sender::Sender, testing::FakeMessenger};
        use hub_core::domain::{ChatId, ThreadId, TopicKey};
        let messenger = std::sync::Arc::new(FakeMessenger::with_sends(vec![
            Ok(()),
            Err(SendError::Failed("offline".into())),
        ]));
        let sender = Sender::new(messenger.clone());
        let result =
            send(&sender, TopicKey { chat: ChatId(-100), thread: ThreadId(1) }, &"x".repeat(8000))
                .await;
        assert!(result.is_err());
        assert_eq!(messenger.send_attempts().len(), 2);
        assert!(messenger.sent_texts().first().unwrap().contains("1 из 3"));
    }
}
