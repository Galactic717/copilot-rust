use super::detector::{classify, is_expand_request, similarity, Confidence};
use crate::types::*;

#[derive(Debug, Clone)]
pub struct Utterance {
    pub source: Source,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Ignore(&'static str),
    Candidate(Candidate),
    New { question: QuestionEvent, superseded: Option<QuestionEvent> },
    Revise(QuestionEvent),
    Duplicate(String),
    Reanswer(QuestionEvent),
    Expand(QuestionEvent),
}

#[derive(Debug, Clone, Copy)]
pub struct CoordinatorOptions {
    /// Practice mode: the user reads questions aloud themselves.
    pub answer_user_source: bool,
    pub dup_window_ms: i64,
    pub dup_threshold: f64,
    /// A question split by VAD into several utterances: a gap below this merges them.
    pub continuation_gap_ms: i64,
    pub follow_up_window_ms: i64,
}

impl Default for CoordinatorOptions {
    fn default() -> Self {
        Self { answer_user_source: false, dup_window_ms: 20_000, dup_threshold: 0.85, continuation_gap_ms: 3_500, follow_up_window_ms: 30_000 }
    }
}

struct Tracked {
    u: Utterance,
    consumed: bool,
}

struct Pending {
    c: Candidate,
    u: Utterance,
}

pub struct QuestionCoordinator {
    session_id: String,
    opts: CoordinatorOptions,
    active: Option<QuestionEvent>,
    history: Vec<QuestionEvent>,
    pending: Vec<Pending>,
    recent: Vec<Tracked>,
    new_id: Box<dyn Fn() -> String + Send + Sync>,
}

impl QuestionCoordinator {
    pub fn new(session_id: String, opts: CoordinatorOptions, new_id: Box<dyn Fn() -> String + Send + Sync>) -> Self {
        Self { session_id, opts, active: None, history: Vec::new(), pending: Vec::new(), recent: Vec::new(), new_id }
    }

    pub fn active(&self) -> Option<&QuestionEvent> {
        self.active.as_ref()
    }

    pub fn candidates(&self) -> Vec<Candidate> {
        self.pending.iter().map(|p| p.c.clone()).collect()
    }

    pub fn questions(&self) -> &[QuestionEvent] {
        &self.history
    }

    pub fn is_current(&self, id: &str, revision: u32) -> bool {
        self.active.as_ref().is_some_and(|a| a.question_id == id && a.revision == revision)
    }

    pub fn on_utterance(&mut self, u: Utterance) -> Decision {
        let text = u.text.trim().to_string();
        if text.is_empty() {
            return Decision::Ignore("empty");
        }
        self.recent.retain(|r| u.end_ms - r.u.end_ms < 60_000);

        if u.source == Source::User {
            // Speakers instead of headphones: the mic hears the remote voice too.
            let echo = self.recent.iter().any(|r| r.u.source == Source::Remote && (r.u.start_ms - u.start_ms).abs() < 2500 && similarity(&r.u.text, &text) >= 0.6);
            if echo {
                return Decision::Ignore("echo");
            }
            if let Some(a) = &self.active {
                if is_expand_request(&text) {
                    return Decision::Expand(a.clone());
                }
            }
            if !self.opts.answer_user_source {
                self.recent.push(Tracked { u: Utterance { text, ..u }, consumed: true });
                return Decision::Ignore("own_speech");
            }
        }

        let (kind, confidence) = classify(&text);
        let idx = self.recent.len();
        self.recent.push(Tracked { u: Utterance { text: text.clone(), ..u.clone() }, consumed: false });
        if kind == UtteranceKind::SmallTalk {
            self.recent[idx].consumed = true;
            return Decision::Ignore("small_talk");
        }

        if let Some(a) = self.active.clone().filter(|a| a.source == u.source) {
            let sim = similarity(&a.text, &text);
            if sim >= self.opts.dup_threshold && u.end_ms - a.end_ms < self.opts.dup_window_ms {
                self.recent[idx].consumed = true;
                return Decision::Duplicate(a.question_id.clone());
            }
            let gap = u.start_ms - a.end_ms;
            if gap >= 0 && gap <= self.opts.continuation_gap_ms && kind != UtteranceKind::Unknown {
                self.recent[idx].consumed = true;
                return self.revise(&a, format!("{} {}", a.text, text), u.end_ms, merge_kind(a.kind, kind));
            }
            if kind == UtteranceKind::FollowUp && confidence == Confidence::High && u.start_ms - a.end_ms <= self.opts.follow_up_window_ms {
                self.recent[idx].consumed = true;
                return self.revise(&a, format!("{} → {}", a.text, text), u.end_ms, a.kind);
            }
        }

        if let Some(dup) = self.history.iter().find(|q| u.end_ms - q.end_ms < self.opts.dup_window_ms && similarity(&q.text, &text) >= self.opts.dup_threshold) {
            self.recent[idx].consumed = true;
            return Decision::Duplicate(dup.question_id.clone());
        }

        let askable = matches!(kind, UtteranceKind::Question | UtteranceKind::CodingTask | UtteranceKind::ScreenReference | UtteranceKind::FollowUp);
        if askable && confidence == Confidence::High {
            self.recent[idx].consumed = true;
            return self.open(text.clone(), Utterance { text, ..u }, kind, QuestionTrigger::Auto, None);
        }
        if askable {
            let c = Candidate {
                question_id: (self.new_id)(),
                text: text.clone(),
                kind,
                reason: crate::i18n::t("немає явних ознак питання", "no clear question signals").into(),
            };
            if self.pending.len() > 2 {
                self.pending.remove(0);
            }
            self.pending.push(Pending { c: c.clone(), u: Utterance { text, ..u } });
            return Decision::Candidate(c);
        }
        Decision::Ignore(kind.as_str())
    }

    /// «Відповісти зараз» — always available, even when auto-detection found nothing.
    pub fn manual(&mut self, trigger: QuestionTrigger, typed: Option<&str>) -> Decision {
        if let Some(t) = typed.map(str::trim).filter(|t| !t.is_empty()) {
            // I anchor a typed question at the latest heard speech, so everything said so far stays in its context window.
            let now = self.active.as_ref().map(|a| a.end_ms).unwrap_or(0).max(self.recent.last().map(|r| r.u.end_ms).unwrap_or(0));
            let kind = classify(t).0;
            return self.open(t.into(), Utterance { source: Source::User, text: t.into(), start_ms: now, end_ms: now }, kind, QuestionTrigger::Typed, None);
        }
        if let Some(id) = self.pending.last().map(|p| p.c.question_id.clone()) {
            return self.confirm_candidate(&id, trigger);
        }
        let fresh: Vec<usize> = self.recent.iter().enumerate().filter(|(_, r)| r.u.source == Source::Remote && !r.consumed).map(|(i, _)| i).collect();
        if let Some(&last) = fresh.last() {
            let last_end = self.recent[last].u.end_ms;
            let span: Vec<usize> = fresh.iter().copied().filter(|&i| last_end - self.recent[i].u.start_ms <= 15_000).collect();
            let span = &span[span.len().saturating_sub(2)..];
            for &i in span {
                self.recent[i].consumed = true;
            }
            let text = span.iter().map(|&i| self.recent[i].u.text.as_str()).collect::<Vec<_>>().join(" ");
            let kind = match classify(&text).0 {
                UtteranceKind::Statement => UtteranceKind::Question,
                k => k,
            };
            let start = self.recent[span[0]].u.start_ms;
            return self.open(text.clone(), Utterance { source: Source::Remote, text, start_ms: start, end_ms: last_end }, kind, trigger, None);
        }
        if let Some(a) = &self.active {
            return Decision::Reanswer(a.clone());
        }
        Decision::Ignore("nothing_to_answer")
    }

    pub fn confirm_candidate(&mut self, id: &str, trigger: QuestionTrigger) -> Decision {
        let Some(pos) = self.pending.iter().position(|p| p.c.question_id == id) else { return Decision::Ignore("no_candidate") };
        let p = self.pending.remove(pos);
        if let Some(t) = self.recent.iter_mut().find(|r| r.u.start_ms == p.u.start_ms && r.u.source == p.u.source) {
            t.consumed = true;
        }
        self.open(p.c.text, p.u, p.c.kind, trigger, Some(p.c.question_id))
    }

    pub fn dismiss_candidate(&mut self, id: &str) {
        self.pending.retain(|p| p.c.question_id != id);
    }

    /// The user corrected a misrecognized question.
    pub fn edit(&mut self, text: &str) -> Decision {
        let t = text.trim();
        match self.active.clone() {
            Some(a) if !t.is_empty() => {
                let kind = classify(t).0;
                self.revise(&a, t.into(), a.end_ms, kind)
            }
            _ => Decision::Ignore("no_active"),
        }
    }

    /// «Наступне питання»: I clear the board without inventing a new question.
    pub fn clear(&mut self) {
        self.active = None;
        self.pending.clear();
    }

    pub fn mark_answered(&mut self, id: &str, revision: u32) {
        if self.is_current(id, revision) {
            if let Some(a) = self.active.as_mut() {
                a.status = QuestionStatus::Answered;
            }
        }
    }

    fn open(&mut self, text: String, u: Utterance, kind: UtteranceKind, trigger: QuestionTrigger, id: Option<String>) -> Decision {
        let prev = self.active.clone();
        let q = QuestionEvent {
            session_id: self.session_id.clone(),
            question_id: id.unwrap_or_else(|| (self.new_id)()),
            revision: 1,
            source: u.source,
            text: text.clone(),
            start_ms: u.start_ms,
            end_ms: u.end_ms,
            kind,
            status: QuestionStatus::Confirmed,
            supersedes: prev.as_ref().map(|p| p.question_id.clone()),
            trigger,
        };
        if let Some(p) = &prev {
            for h in self.history.iter_mut().filter(|h| h.question_id == p.question_id) {
                h.status = QuestionStatus::Superseded;
            }
        }
        self.history.push(q.clone());
        if self.history.len() > 200 {
            self.history.remove(0);
        }
        self.active = Some(q.clone());
        let thr = self.opts.dup_threshold;
        self.pending.retain(|p| similarity(&p.c.text, &text) < thr);
        Decision::New { question: q, superseded: prev }
    }

    fn revise(&mut self, a: &QuestionEvent, text: String, end_ms: i64, kind: UtteranceKind) -> Decision {
        let q = QuestionEvent { text, end_ms, kind, revision: a.revision + 1, status: QuestionStatus::Confirmed, ..a.clone() };
        for h in self.history.iter_mut().filter(|h| h.question_id == a.question_id) {
            *h = q.clone();
        }
        self.active = Some(q.clone());
        Decision::Revise(q)
    }
}

fn merge_kind(a: UtteranceKind, b: UtteranceKind) -> UtteranceKind {
    if a == UtteranceKind::CodingTask || b == UtteranceKind::CodingTask {
        UtteranceKind::CodingTask
    } else if a == UtteranceKind::ScreenReference || b == UtteranceKind::ScreenReference {
        UtteranceKind::ScreenReference
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::session::board::AnswerBoard;

    fn coord(practice: bool) -> QuestionCoordinator {
        let n = Arc::new(AtomicUsize::new(0));
        QuestionCoordinator::new(
            "s".into(),
            CoordinatorOptions { answer_user_source: practice, ..Default::default() },
            Box::new(move || format!("id{}", n.fetch_add(1, Ordering::SeqCst))),
        )
    }

    fn u(source: Source, text: &str, start: i64, end: i64) -> Utterance {
        Utterance { source, text: text.into(), start_ms: start, end_ms: end }
    }

    #[test]
    fn new_question_supersedes_previous() {
        let mut c = coord(false);
        let Decision::New { question: a, .. } = c.on_utterance(u(Source::Remote, "What is a mutex?", 0, 1500)) else { panic!() };
        let Decision::New { question: b, .. } = c.on_utterance(u(Source::Remote, "How does garbage collection work in Java?", 20000, 22000)) else { panic!() };
        assert_eq!(b.supersedes, Some(a.question_id));
    }

    #[test]
    fn continuation_revises_duplicate_suppressed_follow_up_keeps_context() {
        let mut c = coord(false);
        c.on_utterance(u(Source::Remote, "Tell me about databases.", 0, 1500));
        let Decision::Revise(q) = c.on_utterance(u(Source::Remote, "Specifically how indexes work.", 2500, 4000)) else { panic!() };
        assert_eq!(q.revision, 2);

        let mut c = coord(false);
        c.on_utterance(u(Source::Remote, "Explain idempotency in REST APIs", 0, 1500));
        assert!(matches!(c.on_utterance(u(Source::Remote, "explain idempotency in rest APIs", 9000, 10500)), Decision::Duplicate(_)));

        let mut c = coord(false);
        c.on_utterance(u(Source::Remote, "Як працює індекс у PostgreSQL?", 0, 1500));
        let Decision::Revise(q) = c.on_utterance(u(Source::Remote, "А чому B-tree, а не hash?", 15000, 16500)) else { panic!() };
        assert!(q.text.contains("PostgreSQL"));
    }

    #[test]
    fn own_speech_echo_candidates_and_manual() {
        assert!(matches!(coord(false).on_utterance(u(Source::User, "What is a mutex?", 0, 1000)), Decision::Ignore(_)));
        assert!(matches!(coord(true).on_utterance(u(Source::User, "What is a mutex?", 0, 1000)), Decision::New { .. }));

        let mut c = coord(true);
        c.on_utterance(u(Source::Remote, "What is a mutex and when to use it?", 0, 2000));
        assert!(matches!(c.on_utterance(u(Source::User, "what is a mutex and when to use it", 200, 2100)), Decision::Ignore("echo")));

        let mut c = coord(false);
        let d = c.on_utterance(u(Source::Remote, "досвід з докером", 0, 1000));
        assert!(matches!(d, Decision::Candidate(_) | Decision::Ignore(_)));
        assert!(matches!(c.manual(QuestionTrigger::Hotkey, None), Decision::New { .. }));

        assert!(matches!(coord(false).manual(QuestionTrigger::Hotkey, None), Decision::Ignore("nothing_to_answer")));
    }

    /// 1000 synthetic turn transitions: an answer for a stale (question, revision) must never become visible.
    #[test]
    fn board_never_shows_stale_answers() {
        let mut seed: u64 = 42;
        let mut rnd = move || {
            seed = (seed.wrapping_mul(1103515245).wrapping_add(12345)) % (1 << 31);
            seed as f64 / (1u64 << 31) as f64
        };
        let mut c = coord(false);
        let mut board = AnswerBoard::default();
        let topics = ["mutex", "garbage collection", "REST idempotency", "CAP theorem", "Kubernetes pods", "database indexes", "event loop", "TCP handshake"];
        let mut inflight: Vec<AnswerEvent> = Vec::new();
        let mut t = 0i64;
        let mut n = 0;
        for step in 0..1000 {
            t += 500 + (rnd() * 6000.0) as i64;
            let r = rnd();
            let dec = if r < 0.35 {
                let topic = topics[(rnd() * topics.len() as f64) as usize];
                c.on_utterance(u(Source::Remote, &format!("Can you explain {topic} in detail number {step}?"), t, t + 1500))
            } else if r < 0.5 {
                c.on_utterance(u(Source::Remote, "and specifically the edge cases", t, t + 900))
            } else if r < 0.6 {
                c.manual(QuestionTrigger::Hotkey, None)
            } else {
                Decision::Ignore("tick")
            };
            if let Decision::New { question: q, .. } | Decision::Revise(q) = &dec {
                board.set_active(Some(&q.question_id), q.revision);
                n += 1;
                let ev = AnswerEvent {
                    session_id: "s".into(),
                    question_id: q.question_id.clone(),
                    revision: q.revision,
                    answer_id: format!("a{n}"),
                    kind: AnswerKind::Say,
                    provider: String::new(),
                    model: String::new(),
                    prompt_preset: String::new(),
                    text: q.text.clone(),
                    status: AnswerStatus::Streaming,
                    first_token_ms: None,
                    latency_ms: None,
                    usage: None,
                    usage_cost_usd: None,
                    error: None,
                };
                board.claim(AnswerKind::Say, &ev.answer_id);
                inflight.push(ev);
            }
            // Late chunks from every in-flight request arrive in random order.
            for ev in inflight.iter() {
                if rnd() < 0.5 {
                    board.apply(ev.clone());
                }
            }
            if let (Some(a), Some(shown)) = (c.active(), board.answers().get(&AnswerKind::Say)) {
                assert!(shown.question_id == a.question_id && shown.revision == a.revision, "stale answer visible at step {step}");
            }
            if inflight.len() > 20 {
                inflight.drain(..10);
            }
        }
        assert!(n > 100);
    }
}
