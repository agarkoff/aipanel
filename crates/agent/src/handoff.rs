//! Текст передачи сессии другому инструменту.
//!
//! Claude Code и opencode хранят сессии по-разному и не открывают чужие. Поэтому
//! контекст переносится первым сообщением новой сессии: в него попадают реплики
//! пользователя и ассистента из исходной сессии. Вызовы инструментов, их
//! результаты и размышления не переносятся — они привязаны к исходной сессии,
//! а их объём вытеснил бы саму переписку.

/// Предел текста. Запуск окна на Windows принимает не больше 12 000 единиц
/// UTF-16, поэтому здесь с запасом: 5 800 символов даже из эмодзи — это
/// меньше 12 000 единиц.
const MAX_CHARS: usize = 5_800;
/// Первый запрос обычно содержит постановку задачи, поэтому сохраняется отдельно,
/// даже если с тех пор прошло много сообщений.
const FIRST_LIMIT: usize = 1_500;
const ITEM_LIMIT: usize = 1_500;
/// Меньше этого обрезок реплики бесполезен: лучше остановиться на предыдущей.
const MIN_ROOM: usize = 80;
/// Пометка о пропущенных сообщениях; её место резервируется заранее.
const OMITTED: &str = "[…более ранние сообщения опущены…]\n\n";

fn role_label(role: &str) -> &'static str {
    if role == "user" {
        "Пользователь"
    } else {
        "Ассистент"
    }
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.trim().chars().take(max).collect();
    if text.trim().chars().count() > max {
        out.push('…');
    }
    out
}

/// Собирает первое сообщение для новой сессии: постановка задачи из начала
/// переписки и последние реплики, сколько помещается в предел запуска окна.
pub fn build(source: &str, cwd: &str, messages: &[(String, String)]) -> String {
    let header = format!(
        "Продолжаем задачу из сессии {source} (каталог {cwd}). Ниже — выжимка из переписки той сессии. \
         Продолжай с места остановки, не начинай задачу заново.\n\n"
    );
    let messages: Vec<(&str, &str)> = messages
        .iter()
        .map(|(role, text)| (role.as_str(), text.trim()))
        .filter(|(_, text)| !text.is_empty())
        .collect();

    // Место под пометку резервируется всегда: она добавляется уже после отбора.
    let mut used = header.chars().count() + OMITTED.chars().count();
    let mut picked: Vec<(usize, &str, String)> = Vec::new();

    // Постановка задачи из начала переписки.
    let first = messages.iter().position(|(role, _)| *role == "user");
    if let Some(i) = first {
        let body = clip(messages[i].1, FIRST_LIMIT);
        // «Пользователь:\n» + текст + «\n\n».
        let cost = role_label(messages[i].0).chars().count() + body.chars().count() + 4;
        if used + cost < MAX_CHARS {
            used += cost;
            picked.push((i, messages[i].0, body));
        }
    }

    // Свежие реплики — от последней к первой, пока хватает места.
    for i in (0..messages.len()).rev() {
        if Some(i) == first {
            continue;
        }
        let (role, text) = messages[i];
        let label = role_label(role).chars().count() + 4;
        let room = MAX_CHARS.saturating_sub(used + label);
        if room < MIN_ROOM {
            break;
        }
        let body = clip(text, room.min(ITEM_LIMIT));
        used += label + body.chars().count();
        picked.push((i, role, body));
        if used >= MAX_CHARS {
            break;
        }
    }
    picked.sort_by_key(|(i, _, _)| *i);

    let mut out = header;
    if picked.len() < messages.len() {
        out.push_str(OMITTED);
    }
    for (_, role, text) in picked {
        out.push_str(role_label(role));
        out.push_str(":\n");
        out.push_str(&text);
        out.push_str("\n\n");
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> (String, String) {
        (role.to_owned(), text.to_owned())
    }

    #[test]
    fn keeps_first_request_and_latest_messages() {
        let messages = vec![
            msg("user", "сделай панель"),
            msg("assistant", "начал"),
            msg("user", &"x".repeat(10_000)),
            msg("assistant", "готово наполовину"),
        ];
        let text = build("Claude Code", "D:\\p", &messages);
        assert!(text.contains("Продолжаем задачу из сессии Claude Code (каталог D:\\p)"));
        assert!(text.contains("Пользователь:\nсделай панель"));
        assert!(text.contains("Ассистент:\nготово наполовину"));
        assert!(text.contains('…'));
        assert!(text.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn marks_dropped_messages() {
        let mut messages = vec![msg("user", "старт")];
        for i in 0..10 {
            messages.push(msg("assistant", &format!("ответ {i} {}", "y".repeat(900))));
        }
        let text = build("opencode", "/tmp/p", &messages);
        assert!(text.contains("Пользователь:\nстарт"));
        assert!(text.contains("более ранние сообщения опущены"));
        assert!(text.contains("ответ 9"));
        assert!(text.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn skips_empty_messages() {
        let messages = vec![msg("user", "  "), msg("assistant", "ответ")];
        let text = build("opencode", "/tmp/p", &messages);
        assert!(!text.contains("Пользователь:"));
        assert!(text.contains("Ассистент:\nответ"));
    }
}
