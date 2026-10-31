use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::types::UtteranceKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// May auto-trigger.
    High,
    /// Shown as «Можливе питання» only.
    Low,
}

fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}

// Rust regex has no look-around; Unicode `\b` gives the same "not followed by a letter" test for Cyrillic words.
static SMALL_TALK: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"\b(how are you|can you hear me|hear me ok|nice to meet|thank(s| you)|good (morning|afternoon|evening)|hello|hi there)\b"),
        // «як ви» only as the whole phrase: «Як ви вирішуєте конфлікти?» is a real question.
        re(r"\b(як справи|як (ви|ти)\s*[?!.,]*$|мене чути|чуєте мене|ви мене чуєте|приємно познайомитися|дякую|добрий (день|вечір)|доброго (ранку|дня)|привіт|вітаю)\b"),
    ]
});

static FOLLOW_UP: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"^(and |so |but )?(why|what about|what if|how so|for example|such as|can you elaborate|elaborate|go deeper|tell me more|more details|in what way)\b"),
        re(r"^(а |і |й |але )?(чому|навіщо|що якщо|а якщо|наприклад|детальніше|можна детальніше|розкрийте|поясніть|в якому сенсі|у якому сенсі|а як|а що)\b"),
    ]
});

static CODING: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"\b(write (a|the) (function|method|class|query|program)|implement|leetcode|algorithm|time complexity|space complexity|big o|refactor|debug this|reverse a|binary tree|linked list)\b"),
        re(r"(напишіть|напиши|реалізуйте|реалізуй) (функцію|метод|клас|запит|алгоритм|код)|алгоритм|складність (за часом|алгоритму)|асимптотик|зв'язаний список|зв’язаний список|бінарне дерево"),
    ]
});

static SCREEN: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"\b(on (the|my|your) screen|i('m| am) sharing|this code|look at (this|the code)|in this snippet|what does this (code|function) do)\b"),
        re(r"(на екрані|подивіться на (код|екран)|цей код|у цьому фрагменті|що робить (цей|ця) )"),
    ]
});

static LEAD_EN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(what|why|how|when|where|which|who|whom|whose|is|are|do|does|did|can|could|would|will|should|have|has|tell me|explain|describe|walk me through|give me|share|talk about|compare|define)\b")
});
static LEAD_UK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(що|чим|чому|навіщо|як|коли|де|куди|звідки|який|яка|яке|які|яким|яких|хто|кого|скільки|розкажіть|розкажи|поясніть|поясни|опишіть|опиши|наведіть|наведи|поділіться|порівняйте|можете|можеш|чи|назвіть)\b")
});
static EMBEDDED_ASK: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(i('d| would) like to (know|hear)|i('m| am) curious|tell us)\b|(хотів би (дізнатися|почути)|хотіла б (дізнатися|почути)|цікаво,? (як|що|чому))"));
static LEADING_FILLER: LazyLock<Regex> = LazyLock::new(|| re(r"^(ok(ay)?|so|well|alright|окей|добре|гаразд|отже|так|ну)[, ]+"));
static ENDS_Q: LazyLock<Regex> = LazyLock::new(|| re(r"\?\s*$"));
static EXPAND: LazyLock<Regex> = LazyLock::new(|| re(r"\b(repeat|say (it|that) again|more detail|elaborate|expand)\b|(повтори|розкрий|детальніше|розгорни)"));

pub fn normalize(text: &str) -> String {
    let lower: String = text.to_lowercase().chars().filter(|c| !matches!(c, '«' | '»' | '"' | '“' | '”' | '„')).collect();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn classify(raw: &str) -> (UtteranceKind, Confidence) {
    use Confidence::*;
    use UtteranceKind::*;
    let t = normalize(raw);
    let n = t.split_whitespace().count();
    if n == 0 {
        return (Unknown, Low);
    }
    let ends_q = ENDS_Q.is_match(&t);
    let stripped = LEADING_FILLER.replace(&t, "").into_owned();
    if n <= 8 && SMALL_TALK.iter().any(|r| r.is_match(&stripped)) {
        return (SmallTalk, High);
    }
    if FOLLOW_UP.iter().any(|r| r.is_match(&stripped)) {
        return (FollowUp, if ends_q || n >= 2 { High } else { Low });
    }
    if SCREEN.iter().any(|r| r.is_match(&stripped)) {
        return (ScreenReference, High);
    }
    if CODING.iter().any(|r| r.is_match(&stripped)) {
        return (CodingTask, High);
    }
    let lead = LEAD_EN.is_match(&stripped) || LEAD_UK.is_match(&stripped);
    if ends_q && n >= 3 {
        return (Question, High);
    }
    if lead && n >= 4 {
        return (Question, High);
    }
    if EMBEDDED_ASK.is_match(&stripped) && n >= 5 {
        return (Question, High);
    }
    if ends_q || lead {
        return (Question, Low);
    }
    if n < 3 {
        return (Unknown, Low);
    }
    (Statement, High)
}

pub fn is_expand_request(raw: &str) -> bool {
    EXPAND.is_match(&normalize(raw))
}

/// Character-trigram Dice coefficient: robust to ASR word-boundary noise and morphology.
pub fn similarity(a: &str, b: &str) -> f64 {
    let ga = trigrams(&normalize(a));
    let gb = trigrams(&normalize(b));
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let inter: usize = ga.iter().map(|(g, c)| (*c).min(*gb.get(g).unwrap_or(&0))).sum();
    let total: usize = ga.values().sum::<usize>() + gb.values().sum::<usize>();
    2.0 * inter as f64 / total as f64
}

fn trigrams(s: &str) -> HashMap<[char; 3], usize> {
    let clean: Vec<char> = std::iter::once(' ')
        .chain(s.chars().filter(|c| c.is_alphanumeric() || *c == ' '))
        .chain(std::iter::once(' '))
        .collect();
    let mut m = HashMap::new();
    for w in clean.windows(3) {
        *m.entry([w[0], w[1], w[2]]).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use UtteranceKind::*;

    #[test]
    fn classifies_en_and_uk() {
        for (text, kind) in [
            ("Explain idempotency in REST APIs", Question),
            ("Розкажіть про ваш досвід з Kubernetes", Question),
            ("Як працює збирач сміття в Java", Question),
            ("Чи працювали ви з Kafka", Question),
            ("Напишіть функцію розвороту зв'язаного списку", CodingTask),
            ("Подивіться на код на екрані", ScreenReference),
            ("А чому саме так?", FollowUp),
            ("Як справи?", SmallTalk),
            ("Can you hear me?", SmallTalk),
            ("Ми зараз використовуємо мікросервісну архітектуру в компанії", Statement),
        ] {
            assert_eq!(classify(text).0, kind, "{text}");
        }
    }

    #[test]
    fn low_confidence_without_question_signals() {
        assert_eq!(classify("ваш досвід з докером?").1, Confidence::High);
        assert_ne!(classify("ну а докер").0, Question);
    }

    #[test]
    fn similarity_bounds() {
        assert!(similarity("Explain idempotency in REST APIs", "explain idempotency in rest api") > 0.85);
        assert!(similarity("What is a mutex", "How does garbage collection work") < 0.4);
    }
}
