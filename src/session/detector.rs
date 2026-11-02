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
        re(r"\b(wie geht'?s|hallo|guten (tag|morgen|abend)|danke|hola|buenos días|buenas tardes|gracias|cómo estás|bonjour|bonsoir|salut|merci|ça va|ciao|buongiorno|buonasera|grazie|come stai|olá|bom dia|boa tarde|obrigad[oa]|tudo bem|dzień dobry|cześć|dziękuję|jak się masz|merhaba|günaydın|teşekkürler|nasılsın)\b"),
        re(r"(你好|您好|谢谢|こんにちは|おはよう|ありがとう|안녕하세요|감사합니다)"),
    ]
});

static FOLLOW_UP: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"^(and |so |but )?(why|what about|what if|how so|for example|such as|can you elaborate|elaborate|go deeper|tell me more|more details|in what way)\b"),
        re(r"^(а |і |й |але )?(чому|навіщо|що якщо|а якщо|наприклад|детальніше|можна детальніше|розкрийте|поясніть|в якому сенсі|у якому сенсі|а як|а що)\b"),
        re(r"^(und |aber |y |pero |et |mais |e |ma |i |ale |peki )?(warum|wieso|zum beispiel|genauer|por qué|por ejemplo|pourquoi|par exemple|perché|per esempio|por que|por exemplo|dlaczego|na przykład|neden|örneğin)\b"),
        re(r"^(为什么|比如|例如|なぜ|どうして|例えば|왜|예를 들어)"),
    ]
});

static CODING: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"\b(write (a|the) (function|method|class|query|program)|implement|leetcode|algorithm|time complexity|space complexity|big o|refactor|debug this|reverse a|binary tree|linked list)\b"),
        re(r"(напишіть|напиши|реалізуйте|реалізуй) (функцію|метод|клас|запит|алгоритм|код)|алгоритм|складність (за часом|алгоритму)|асимптотик|зв'язаний список|зв’язаний список|бінарне дерево"),
        re(r"(eine funktion|implementier|algorithmus|una función|implementa|algoritmo|une fonction|implémente|algorithme|una funzione|uma função|napisz funkcję|zaimplementuj|algorytm|fonksiyon yaz|algoritma|写一个函数|算法|関数を書|アルゴリズム|함수를 작성|알고리즘)"),
    ]
});

static SCREEN: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"\b(on (the|my|your) screen|i('m| am) sharing|this code|look at (this|the code)|in this snippet|what does this (code|function) do)\b"),
        re(r"(на екрані|подивіться на (код|екран)|цей код|у цьому фрагменті|що робить (цей|ця) )"),
        re(r"(auf dem bildschirm|en la pantalla|à l'écran|sullo schermo|na tela|na ekranie|ekranda|屏幕上|画面の|화면에)"),
    ]
});

static LEAD_EN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(what|why|how|when|where|which|who|whom|whose|is|are|do|does|did|can|could|would|will|should|have|has|tell me|explain|describe|walk me through|give me|share|talk about|compare|define)\b")
});
static LEAD_UK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(що|чим|чому|навіщо|як|коли|де|куди|звідки|який|яка|яке|які|яким|яких|хто|кого|скільки|розкажіть|розкажи|поясніть|поясни|опишіть|опиши|наведіть|наведи|поділіться|порівняйте|можете|можеш|чи|назвіть)\b")
});
static LEAD_OTHER: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r"^(was|warum|wieso|wie|wann|wo|welche[rsmn]?|wer|können|könnten|haben|sind|ist|erzählen|erkläre[n]?|beschreibe[n]?",
        r"|qué|que|por qué|cómo|como|cuándo|cuando|dónde|donde|cuál|cual|quién|puede[s]?|podría[s]?|cuénta(me|nos)|explica|describe",
        r"|pourquoi|comment|quand|où|quel(le)?s?|qui|pouvez|peux|parlez|expliquez|décrivez|est-ce",
        r"|cosa|che|come|perché|quando|dove|quale|chi|puoi|può|parlami|spiega|descrivi",
        r"|o que|por que|quando|onde|qual|quem|pode|podes|conte|explique|descreva",
        r"|co|dlaczego|jak|kiedy|gdzie|który|która|które|kto|czy|proszę|opowiedz|wyjaśnij|opisz",
        r"|neden|nasıl|ne zaman|nerede|hangi|kim|anlat|açıkla)\b"
    ))
});
// Scripts without word boundaries: a question word anywhere counts, and the sentence-final particle is the question mark.
static LEAD_CJK: LazyLock<Regex> = LazyLock::new(|| re(r"(什么|为什么|怎么|如何|哪|谁|请解释|请描述|介绍一下|なぜ|どう|いつ|どこ|何|誰|説明して|教えて|무엇|뭐|왜|어떻게|언제|어디|누가|설명해|말씀해)"));
static ENDS_PARTICLE: LazyLock<Regex> = LazyLock::new(|| re(r"(吗|呢|ですか|ますか|でしょうか|까요|습니까|나요|\b(mi|mı|mu|mü|misin|mısın|musun|müsün|misiniz|mısınız|musunuz|müsünüz))[.。!！]?\s*$"));
static EMBEDDED_ASK: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(i('d| would) like to (know|hear)|i('m| am) curious|tell us)\b|(хотів би (дізнатися|почути)|хотіла б (дізнатися|почути)|цікаво,? (як|що|чому))"));
static LEADING_FILLER: LazyLock<Regex> = LazyLock::new(|| re(r"^(ok(ay)?|so|well|alright|окей|добре|гаразд|отже|так|ну|also|gut|vale|bueno|bon|alors|allora|bene|então|dobrze|tak|peki|tamam)[, ]+"));
static ENDS_Q: LazyLock<Regex> = LazyLock::new(|| re(r"[?？؟]\s*$"));
static EXPAND: LazyLock<Regex> = LazyLock::new(|| re(r"\b(repeat|say (it|that) again|more detail|elaborate|expand)\b|(повтори|розкрий|детальніше|розгорни)"));

pub fn normalize(text: &str) -> String {
    let lower: String = text.to_lowercase().chars().filter(|c| !matches!(c, '«' | '»' | '"' | '“' | '”' | '„')).collect();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn classify(raw: &str) -> (UtteranceKind, Confidence) {
    use Confidence::*;
    use UtteranceKind::*;
    let t = normalize(raw);
    // Chinese and Japanese do not separate words: two characters count as about one, so a full sentence is not "one word".
    let cjk = t.chars().filter(|c| matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{9fff}' | '\u{ac00}'..='\u{d7af}')).count();
    let n = t.split_whitespace().count() + cjk / 2;
    if n == 0 || t.is_empty() {
        return (Unknown, Low);
    }
    let ends_q = ENDS_Q.is_match(&t) || ENDS_PARTICLE.is_match(&t);
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
    let lead = LEAD_EN.is_match(&stripped) || LEAD_UK.is_match(&stripped) || LEAD_OTHER.is_match(&stripped) || LEAD_CJK.is_match(&stripped);
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
    fn classifies_other_languages() {
        for (text, kind) in [
            ("Erzählen Sie mir von Ihrer Erfahrung mit Kafka", Question),
            ("Wie funktioniert ein Index in einer Datenbank", Question),
            ("¿Cómo funciona un índice en una base de datos?", Question),
            ("Comment fonctionne un index dans une base de données", Question),
            ("Come funziona un indice in un database relazionale", Question),
            ("Pourquoi avez-vous choisi cette architecture", FollowUp),
            ("Czy pracowałeś wcześniej z Kafką w produkcji", Question),
            ("Kafka ile daha önce çalıştınız mı", Question),
            ("请解释一下数据库索引的工作原理", Question),
            ("データベースのインデックスはなぜ速いのですか", Question),
            ("데이터베이스 인덱스는 어떻게 작동합니까", Question),
            ("Guten Tag", SmallTalk),
            ("Muchas gracias", SmallTalk),
            ("Schreiben Sie eine Funktion zum Umkehren einer Liste", CodingTask),
            ("算法的时间复杂度是多少", CodingTask),
            ("Wir nutzen im Team seit zwei Jahren Microservices überall", Statement),
        ] {
            assert_eq!(classify(text).0, kind, "{text}");
            assert_eq!(classify(text).1, Confidence::High, "{text}");
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
