use serde_json::json;

use crate::db::{i, s, HistoryDetail, Row};
use crate::i18n::t;
use crate::redact::redact;

fn ts(ms: i64) -> String {
    format!("{:02}:{:02}", ms / 60000, (ms % 60000) / 1000)
}

pub fn kind_label(k: &str) -> &'static str {
    match k {
        "say" => "Say it",
        "points" => t("Тези", "Talking points"),
        "deep" => t("Детально", "Deep dive"),
        "screen" => t("Екран", "Screen"),
        "feedback" => t("Розбір відповіді", "Answer review"),
        _ => "?",
    }
}

/// Latest revision of every question, in first-asked order.
pub fn latest_questions(rows: &[Row]) -> Vec<&Row> {
    let mut out: Vec<&Row> = Vec::new();
    for q in rows {
        match out.iter().position(|x| s(x, "id") == s(q, "id")) {
            Some(p) => out[p] = q,
            None => out.push(q),
        }
    }
    out
}

pub fn export_session(d: &HistoryDetail, json_fmt: bool) -> Result<String, String> {
    let Some(session) = &d.session else { return Err(t("Сесію не знайдено", "Session not found").into()) };
    if json_fmt {
        let v = json!({ "format": "copilot/session@1", "session": session, "segments": d.segments, "questions": d.questions, "answers": d.answers });
        return Ok(redact(&serde_json::to_string_pretty(&v).unwrap_or_default()));
    }
    let title = s(session, "title");
    let mut lines = vec![
        format!("# {}", if title.is_empty() { t("Сесія", "Session").to_string() } else { title }),
        String::new(),
        format!("{}: {}", t("Початок", "Started"), crate::i18n::fmt_datetime(i(session, "started_at"))),
        String::new(),
    ];
    let summary = s(session, "summary");
    if !summary.is_empty() {
        lines.push(format!("## {}", t("Підсумок", "Debrief")));
        lines.push(String::new());
        lines.push(summary);
        lines.push(String::new());
    }
    lines.push(format!("## {}", t("Питання і відповіді", "Questions and answers")));
    lines.push(String::new());
    for q in latest_questions(&d.questions) {
        lines.push(format!("### {} — {}", ts(i(q, "start_ms")), s(q, "text")));
        lines.push(String::new());
        for a in d.answers.iter().filter(|a| s(a, "question_id") == s(q, "id") && s(a, "status") == "done") {
            let ft = i(a, "first_token_ms");
            lines.push(format!(
                "**{}** · {}/{}{}",
                kind_label(&s(a, "kind")),
                s(a, "provider"),
                s(a, "model"),
                if ft > 0 { format!(" · {ft} {}", t("мс", "ms")) } else { String::new() }
            ));
            lines.push(String::new());
            lines.push(s(a, "text"));
            lines.push(String::new());
        }
    }
    lines.push(format!("## {}", t("Транскрипт", "Transcript")));
    lines.push(String::new());
    for seg in &d.segments {
        let who = if s(seg, "source") == "remote" { t("Співрозмовник", "Interviewer") } else { t("Я", "Me") };
        lines.push(format!("- `{}` **{}:** {}", ts(i(seg, "start_ms")), who, s(seg, "text")));
    }
    Ok(redact(&lines.join("\n")))
}
