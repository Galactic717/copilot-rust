use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::board::AnswerBoard;
use super::coordinator::{CoordinatorOptions, Decision, QuestionCoordinator, Utterance};
use crate::budget::{BudgetManager, Gate};
use crate::context::prompt::{build_prompt, BuiltPrompt, PromptInput, ScreenInput};
use crate::context::retrieval::Bm25Index;
use crate::db::Db;
use crate::i18n::t;
use crate::providers::errors::app_error;
use crate::providers::registry::ResolvedTarget;
use crate::providers::runner::{run_stream, RunEvent, RunOptions};
use crate::tr;
use crate::types::*;

pub trait SessionDeps: Send + Sync {
    fn settings(&self) -> Settings;
    fn db(&self) -> &Db;
    fn profile(&self) -> Profile;
    fn resolve(&self, role: ModelRole) -> Option<ResolvedTarget>;
    fn fallback(&self, role: ModelRole, exclude: &str) -> Option<ResolvedTarget>;
    fn budget(&self) -> &Mutex<BudgetManager>;
    fn emit(&self);
    fn log(&self, kind: &str, data: Value);
    fn outcome(&self, provider_id: &str, model: &str, ok: bool);
    fn new_id(&self) -> String {
        new_id()
    }
    fn now(&self) -> i64 {
        now_ms()
    }
}

/// Budget slot of one request; released exactly once (finish, error or abort — whichever comes first).
struct Slot {
    held: AtomicBool,
    provider: String,
    model: String,
    is_local: bool,
}

impl Slot {
    fn release(&self, d: &dyn SessionDeps, usage: Option<(u64, u64)>) -> Option<f64> {
        if !self.held.swap(false, Ordering::SeqCst) {
            return None;
        }
        d.budget().lock().unwrap().end(&self.provider, &self.model, self.is_local, usage)
    }
}

struct Inflight {
    cancel: CancellationToken,
    question_id: String,
    revision: u32,
    /// Freed now on abort: cancellation settles asynchronously, and a replacement request starts right away.
    slot: Arc<Slot>,
}

type Then = Option<Box<dyn FnOnce(&mut State) + Send>>;

pub struct State {
    snap: AppSnapshot,
    coord: Option<QuestionCoordinator>,
    board: AnswerBoard,
    inflight: HashMap<String, Inflight>,
    finals: Vec<TranscriptSegment>,
    index: Option<Bm25Index>,
    started_at: i64,
    persist: bool,
    answer_user_source: bool,
}

pub fn off() -> ComponentStatus {
    ComponentStatus::new(ComponentState::Off, t("вимк.", "off"))
}

pub fn empty_snapshot() -> AppSnapshot {
    AppSnapshot {
        session_id: None,
        state: ConvState::Idle,
        listening: false,
        active_question: None,
        candidates: Vec::new(),
        answers: BTreeMap::new(),
        recent_questions: Vec::new(),
        partial: [None, None],
        segments: Vec::new(),
        status: Statuses {
            mic: off(),
            system: off(),
            asr: off(),
            llm: off(),
            vision: off(),
            overlay: off(),
            protection: off(),
            network: ComponentStatus::new(ComponentState::Ok, t("онлайн", "online")),
        },
        cost: SessionCost { usd: Some(0.0), unknown_priced: 0, requests: 0 },
        budget_blocked: None,
        levels: [0.0, 0.0],
        notice: None,
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Via {
    Auto,
    Manual,
}

struct ReqCtx {
    q: QuestionEvent,
    kind: AnswerKind,
    role: ModelRole,
    answer_id: String,
    base: AnswerEvent,
    prompt: BuiltPrompt,
    target: ResolvedTarget,
    cancel: CancellationToken,
    slot: Arc<Slot>,
    then: Then,
}

pub struct SessionManager {
    d: Arc<dyn SessionDeps>,
    rt: tokio::runtime::Handle,
    st: Mutex<State>,
}

impl SessionManager {
    pub fn new(d: Arc<dyn SessionDeps>, rt: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            d,
            rt,
            st: Mutex::new(State {
                snap: empty_snapshot(),
                coord: None,
                board: AnswerBoard::default(),
                inflight: HashMap::new(),
                finals: Vec::new(),
                index: None,
                started_at: 0,
                persist: true,
                answer_user_source: false,
            }),
        })
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.st.lock().unwrap().snap.clone()
    }

    pub fn with_snap<R>(&self, f: impl FnOnce(&mut AppSnapshot) -> R) -> R {
        let r = f(&mut self.st.lock().unwrap().snap);
        self.d.emit();
        r
    }

    pub fn active(&self) -> bool {
        self.st.lock().unwrap().snap.session_id.is_some()
    }

    pub fn listening(&self) -> bool {
        self.st.lock().unwrap().snap.listening
    }

    pub fn session_id(&self) -> Option<String> {
        self.st.lock().unwrap().snap.session_id.clone()
    }

    /// Epoch ms of the session start (0 when idle).
    pub fn started_at(&self) -> i64 {
        self.st.lock().unwrap().started_at
    }

    pub fn start(self: &Arc<Self>, mode: SessionMode) -> String {
        let mut st = self.st.lock().unwrap();
        self.start_locked(&mut st, mode)
    }

    fn start_locked(self: &Arc<Self>, st: &mut State, mode: SessionMode) -> String {
        if st.snap.session_id.is_some() {
            self.stop_locked(st);
        }
        let id = self.d.new_id();
        let s = self.d.settings();
        st.persist = s.history && s.consent.history;
        st.started_at = self.d.now();
        st.answer_user_source = mode == SessionMode::Practice;
        let d = self.d.clone();
        st.coord = Some(QuestionCoordinator::new(
            id.clone(),
            CoordinatorOptions { answer_user_source: st.answer_user_source, ..Default::default() },
            Box::new(move || d.new_id()),
        ));
        st.board.set_active(None, 0);
        st.finals.clear();
        self.reload_knowledge_locked(st);
        self.d.budget().lock().unwrap().reset_session();
        if st.persist {
            let p = self.d.profile();
            let title = format!("{} · {}", crate::i18n::fmt_datetime(st.started_at), p.name);
            self.d.db().start_session(&id, &p.id, mode.as_str(), &title);
        }
        let snap = &mut st.snap;
        snap.session_id = Some(id.clone());
        snap.state = ConvState::Listening;
        snap.listening = true;
        snap.active_question = None;
        snap.candidates.clear();
        snap.answers.clear();
        snap.recent_questions.clear();
        snap.partial = [None, None];
        snap.segments.clear();
        snap.budget_blocked = None;
        snap.notice = None;
        self.sync_cost(st);
        self.d.emit();
        id
    }

    /// Continue a prior session: start fresh, then preload its transcript so the conversation keeps its context.
    pub fn continue_previous(self: &Arc<Self>, prior: Vec<TranscriptSegment>, mode: SessionMode) -> String {
        let mut st = self.st.lock().unwrap();
        let id = self.start_locked(&mut st, mode);
        let recent = &prior[prior.len().saturating_sub(300)..];
        if let Some(last) = recent.last() {
            // The old timestamps are on the old session's clock; I rebase them to just-before-now so they fall in the context window.
            let shift = -1000 - last.end_ms;
            st.finals = recent
                .iter()
                .map(|f| TranscriptSegment { session_id: id.clone(), start_ms: f.start_ms + shift, end_ms: f.end_ms + shift, ..f.clone() })
                .collect();
            st.snap.segments = st.finals[st.finals.len().saturating_sub(40)..].to_vec();
        }
        st.snap.notice = Some(t("Продовжено минулу сесію — контекст відновлено.", "Previous session resumed — context restored.").into());
        self.d.emit();
        id
    }

    pub fn stop(&self) {
        let mut st = self.st.lock().unwrap();
        self.stop_locked(&mut st);
    }

    fn stop_locked(&self, st: &mut State) {
        let Some(id) = st.snap.session_id.clone() else { return };
        self.cancel_all(st);
        if st.persist {
            self.d.db().end_session(&id);
        }
        let snap = &mut st.snap;
        snap.session_id = None;
        snap.state = ConvState::Stopped;
        snap.listening = false;
        snap.partial = [None, None];
        snap.candidates.clear();
        // Capture statuses come from the audio threads only while they capture; after stop nothing else would reset them.
        snap.status.mic = off();
        snap.status.system = off();
        snap.levels = [0.0, 0.0];
        st.coord = None;
        st.started_at = 0;
        self.d.emit();
    }

    pub fn pause(&self) {
        let mut st = self.st.lock().unwrap();
        if st.snap.session_id.is_none() {
            return;
        }
        st.snap.listening = false;
        st.snap.state = ConvState::Paused;
        st.snap.partial = [None, None];
        self.d.emit();
    }

    pub fn resume(&self) {
        let mut st = self.st.lock().unwrap();
        if st.snap.session_id.is_none() {
            return;
        }
        st.snap.listening = true;
        st.snap.state = if st.snap.active_question.is_some() { ConvState::AnswerReady } else { ConvState::Listening };
        self.d.emit();
    }

    /// Emergency stop: every stream and request ends now.
    pub fn panic(&self) {
        self.stop();
    }

    pub fn reload_knowledge(&self) {
        let mut st = self.st.lock().unwrap();
        self.reload_knowledge_locked(&mut st);
    }

    fn reload_knowledge_locked(&self, st: &mut State) {
        let chunks = self.d.db().chunks_for_profile(&self.d.profile().id);
        st.index = if chunks.is_empty() { None } else { Some(Bm25Index::new(chunks)) };
    }

    pub fn set_status(&self, key: StatusKey, s: ComponentStatus) {
        self.with_snap(|snap| *snap.status.get_mut(key) = s);
    }

    pub fn set_level(&self, source: Source, rms: f32) {
        self.st.lock().unwrap().snap.levels[source.idx()] = rms;
    }

    pub fn notice(&self, text: Option<String>) {
        self.with_snap(|snap| snap.notice = text);
    }

    pub fn on_segment(self: &Arc<Self>, seg: TranscriptSegment) {
        let mut st = self.st.lock().unwrap();
        if st.coord.is_none() || !st.snap.listening || st.snap.session_id.as_deref() != Some(seg.session_id.as_str()) {
            return;
        }
        if !seg.is_final {
            // In a call my own speech is not answered, so I don't surface its partial as the overlay's live question.
            if seg.source == Source::User && !st.answer_user_source {
                return;
            }
            st.snap.partial[seg.source.idx()] = Some(seg.text.clone());
            if matches!(st.snap.state, ConvState::Listening | ConvState::AnswerReady) {
                st.snap.state = ConvState::PartialTranscript;
            }
            self.d.emit();
            return;
        }
        st.snap.partial[seg.source.idx()] = None;
        if seg.text.trim().is_empty() {
            self.d.emit();
            return;
        }
        st.finals.push(seg.clone());
        let cutoff = seg.end_ms - 30 * 60_000;
        let drop = st.finals.iter().take_while(|f| f.end_ms < cutoff).count();
        st.finals.drain(..drop);
        st.snap.segments = st.finals[st.finals.len().saturating_sub(40)..].to_vec();
        if st.persist {
            self.d.db().add_segment(&seg);
        }
        let dec = st.coord.as_mut().unwrap().on_utterance(Utterance { source: seg.source, text: seg.text, start_ms: seg.start_ms, end_ms: seg.end_ms });
        self.handle(&mut st, dec, Via::Auto);
    }

    pub fn answer_now(self: &Arc<Self>, kind: AnswerKind, trigger: QuestionTrigger) {
        let mut st = self.st.lock().unwrap();
        let Some(coord) = st.coord.as_mut() else { return };
        if kind == AnswerKind::Deep {
            if let Some(a) = coord.active().cloned() {
                self.generate(&mut st, a, vec![AnswerKind::Deep], None);
                self.d.emit();
                return;
            }
        }
        let dec = coord.manual(trigger, None);
        self.handle(&mut st, dec, Via::Manual);
    }

    pub fn ask_typed(self: &Arc<Self>, text: &str) {
        let mut st = self.st.lock().unwrap();
        if st.coord.is_none() {
            self.start_locked(&mut st, SessionMode::Practice);
        }
        let dec = st.coord.as_mut().unwrap().manual(QuestionTrigger::Typed, Some(text));
        self.handle(&mut st, dec, Via::Manual);
    }

    pub fn confirm_candidate(self: &Arc<Self>, id: &str) {
        let mut st = self.st.lock().unwrap();
        if let Some(c) = st.coord.as_mut() {
            let dec = c.confirm_candidate(id, QuestionTrigger::Manual);
            self.handle(&mut st, dec, Via::Manual);
        }
    }

    pub fn dismiss_candidate(&self, id: &str) {
        let mut st = self.st.lock().unwrap();
        if let Some(c) = st.coord.as_mut() {
            c.dismiss_candidate(id);
        }
        self.sync_questions(&mut st);
        self.d.emit();
    }

    pub fn edit_question(self: &Arc<Self>, text: &str) {
        let mut st = self.st.lock().unwrap();
        if let Some(c) = st.coord.as_mut() {
            let dec = c.edit(text);
            self.handle(&mut st, dec, Via::Manual);
        }
    }

    pub fn next_question(&self) {
        let mut st = self.st.lock().unwrap();
        if st.coord.is_none() {
            return;
        }
        self.cancel_all(&mut st);
        st.coord.as_mut().unwrap().clear();
        st.board.set_active(None, 0);
        st.snap.state = ConvState::Listening;
        self.sync_questions(&mut st);
        self.d.emit();
    }

    pub fn approve_budget(&self) {
        self.d.budget().lock().unwrap().approve_override();
        self.with_snap(|s| s.budget_blocked = None);
    }

    pub fn screen(self: &Arc<Self>, input: ScreenInput) {
        let mut st = self.st.lock().unwrap();
        if st.coord.is_none() {
            self.start_locked(&mut st, SessionMode::Practice);
        }
        let label = match input.task {
            ScreenTask::Explain => t("Поясни умову", "Explain the task"),
            ScreenTask::Approach => t("Запропонуй підхід", "Suggest an approach"),
            ScreenTask::Review => t("Розбери код", "Review the code"),
        };
        let text = format!("[{}] {}", t("Екран", "Screen"), label);
        let Decision::New { question, .. } = st.coord.as_mut().unwrap().manual(QuestionTrigger::Manual, Some(&text)) else { return };
        self.apply_question(&mut st, &question);
        self.sync_questions(&mut st);
        self.generate(&mut st, question, vec![AnswerKind::Screen], Some(input));
        self.d.emit();
    }

    fn handle(self: &Arc<Self>, st: &mut State, dec: Decision, via: Via) {
        let s = self.d.settings();
        match dec {
            Decision::New { question: q, .. } | Decision::Revise(q) => {
                self.apply_question(st, &q);
                if via == Via::Manual || s.auto_answer {
                    self.generate(st, q, vec![AnswerKind::Say, AnswerKind::Points], None);
                } else {
                    st.snap.state = ConvState::QuestionConfirmed;
                }
            }
            Decision::Reanswer(q) => self.generate(st, q, vec![AnswerKind::Say, AnswerKind::Points], None),
            Decision::Expand(q) => self.generate(st, q, vec![AnswerKind::Deep], None),
            Decision::Candidate(_) => st.snap.state = ConvState::QuestionCandidate,
            Decision::Duplicate(of) => self.d.log("duplicate_suppressed", json!({ "of": of })),
            Decision::Ignore(reason) => {
                if via == Via::Manual {
                    st.snap.notice = (reason == "nothing_to_answer")
                        .then(|| t("Поки нема на що відповідати — питання не розпізнано", "Nothing to answer yet — no question recognized").into());
                }
                if st.snap.state == ConvState::PartialTranscript {
                    st.snap.state = if st.snap.active_question.is_some() { ConvState::AnswerReady } else { ConvState::Listening };
                }
            }
        }
        self.sync_questions(st);
        self.d.emit();
    }

    fn apply_question(&self, st: &mut State, q: &QuestionEvent) {
        let stale: Vec<String> = st.inflight.iter().filter(|(_, f)| f.question_id != q.question_id || f.revision != q.revision).map(|(k, _)| k.clone()).collect();
        for id in stale {
            if let Some(f) = st.inflight.remove(&id) {
                f.cancel.cancel();
                f.slot.release(self.d.as_ref(), None);
            }
        }
        st.board.set_active(Some(&q.question_id), q.revision);
        if q.supersedes.is_some() {
            st.snap.state = ConvState::Superseded;
        }
        if st.persist {
            self.d.db().upsert_question(q);
        }
    }

    fn sync_questions(&self, st: &mut State) {
        let c = st.coord.as_ref();
        let a = c.and_then(|c| c.active());
        st.snap.active_question = a.map(|a| ActiveQuestion {
            question_id: a.question_id.clone(),
            revision: a.revision,
            text: a.text.clone(),
            source: a.source,
            kind: a.kind,
            trigger: a.trigger,
        });
        st.snap.candidates = c.map(|c| c.candidates()).unwrap_or_default();
        let active_id = a.map(|a| a.question_id.clone());
        let mut recent: Vec<(String, String)> = c
            .map(|c| c.questions().iter().filter(|q| Some(&q.question_id) != active_id.as_ref()).map(|q| (q.question_id.clone(), q.text.clone())).collect())
            .unwrap_or_default();
        recent = recent.split_off(recent.len().saturating_sub(6));
        recent.reverse();
        st.snap.recent_questions = recent;
        st.snap.answers = st.board.answers().clone();
    }

    fn sync_cost(&self, st: &mut State) {
        st.snap.cost = self.d.budget().lock().unwrap().snapshot();
    }

    fn cancel_all(&self, st: &mut State) {
        for (_, f) in st.inflight.drain() {
            f.cancel.cancel();
            f.slot.release(self.d.as_ref(), None);
        }
    }

    fn publish(&self, st: &mut State, ev: AnswerEvent) {
        if st.board.apply(ev) {
            st.snap.answers = st.board.answers().clone();
            self.d.emit();
        }
    }

    fn finish(&self, st: &mut State, ev: AnswerEvent) {
        let (qid, rev, aid, status) = (ev.question_id.clone(), ev.revision, ev.answer_id.clone(), ev.status);
        if st.persist && status != AnswerStatus::Stale {
            self.d.db().upsert_answer(&ev);
        }
        self.publish(st, ev);
        st.inflight.remove(&aid);
        if st.board.is_active(&qid, rev) && !st.inflight.values().any(|f| f.question_id == qid) {
            st.snap.state = if matches!(status, AnswerStatus::Error | AnswerStatus::Blocked) { ConvState::Error } else { ConvState::AnswerReady };
        }
        self.d.emit();
    }

    /// Say-it first; points start once say has its first token, so the long answer never delays the short one.
    fn generate(self: &Arc<Self>, st: &mut State, q: QuestionEvent, kinds: Vec<AnswerKind>, screen: Option<ScreenInput>) {
        st.snap.state = ConvState::Generating;
        let rest: Vec<AnswerKind> = kinds[1..].to_vec();
        let then: Then = if rest.is_empty() {
            None
        } else {
            let me = self.clone();
            let q2 = q.clone();
            Some(Box::new(move |st: &mut State| {
                for k in rest {
                    me.start_request(st, &q2, k, None, None);
                }
            }))
        };
        self.start_request(st, &q, kinds[0], screen, then);
    }

    fn start_request(self: &Arc<Self>, st: &mut State, q: &QuestionEvent, kind: AnswerKind, screen: Option<ScreenInput>, then: Then) {
        let role = match kind {
            AnswerKind::Deep => ModelRole::DeepAnswer,
            AnswerKind::Screen if screen.as_ref().is_some_and(|s| s.image.is_some()) => ModelRole::Vision,
            _ => ModelRole::QuickAnswer,
        };
        let initial = self.d.resolve(role).or_else(|| if role == ModelRole::DeepAnswer { self.d.resolve(ModelRole::QuickAnswer) } else { None });
        let answer_id = self.d.new_id();
        let mut base = AnswerEvent {
            session_id: q.session_id.clone(),
            question_id: q.question_id.clone(),
            revision: q.revision,
            answer_id: answer_id.clone(),
            kind,
            provider: initial.as_ref().map(|t| t.cfg.id.clone()).unwrap_or_default(),
            model: initial.as_ref().map(|t| t.model.clone()).unwrap_or_default(),
            prompt_preset: String::new(),
            text: String::new(),
            status: AnswerStatus::Streaming,
            first_token_ms: None,
            latency_ms: None,
            usage: None,
            usage_cost_usd: None,
            error: None,
        };
        st.board.claim(kind, &answer_id);
        let fail = |me: &Self, st: &mut State, base: AnswerEvent, status: AnswerStatus, e: AppError, then: Then| {
            me.finish(st, AnswerEvent { status, error: Some(e), ..base });
            if let Some(f) = then {
                f(st);
            }
        };

        let Some(target) = initial else {
            let detail = if role == ModelRole::Vision {
                t("Модель для зображень не вибрано", "No vision model selected")
            } else {
                t("Не вибрано модель для відповідей — відкрийте «Моделі та ключі»", "No answer model selected — open “Models & keys”")
            };
            return fail(self, st, base, AnswerStatus::Error, app_error(ErrorCode::Unsupported, Some(detail), None), then);
        };
        let s = self.d.settings();
        if !target.is_local && s.offline {
            let e = app_error(ErrorCode::OfflineBlocked, Some(t("Офлайн-режим: ця модель працює в хмарі", "Offline mode: this model runs in the cloud")), None);
            return fail(self, st, base, AnswerStatus::Blocked, e, then);
        }
        if !target.is_local && !s.consent.cloud {
            let e = app_error(
                ErrorCode::Unsupported,
                Some(t(
                    "Надсилання в хмару не дозволено — увімкніть його в «Приватність» або виберіть локальну модель",
                    "Sending to the cloud is not allowed — enable it in “Privacy” or pick a local model",
                )),
                None,
            );
            return fail(self, st, base, AnswerStatus::Blocked, e, then);
        }
        let window_start = q.end_ms - s.context_window_min as i64 * 60_000;
        let knowledge = st.index.as_ref().map(|i| i.search(&q.text, 8).into_iter().map(|r| r.0).collect()).unwrap_or_default();
        let profile = self.d.profile();
        let earlier: Vec<String> = st
            .coord
            .as_ref()
            .map(|c| {
                let h: Vec<String> = c.questions().iter().filter(|h| h.question_id != q.question_id).map(|h| h.text.clone()).collect();
                h[h.len().saturating_sub(8)..].to_vec()
            })
            .unwrap_or_default();
        let prompt = build_prompt(&PromptInput {
            question: &q.text,
            kind,
            profile: &profile,
            answer_lang: s.answer_lang,
            transcript: st.finals.iter().filter(|f| f.end_ms >= window_start && f.end_ms <= q.end_ms + 1000).collect(),
            earlier_questions: earlier,
            knowledge,
            screen: screen.as_ref(),
        });
        base.prompt_preset = prompt.preset.clone();

        let Some((cancel, slot)) = self.begin_attempt(st, &target, &base) else {
            if let Some(f) = then {
                f(st);
            }
            return;
        };
        let ctx = ReqCtx { q: q.clone(), kind, role, answer_id, base, prompt, target, cancel, slot, then };
        let me = self.clone();
        self.rt.spawn(async move { me.run_request(ctx).await });
    }

    /// Budget gate + slot + in-flight registration for one attempt. On refusal the answer is finished as blocked.
    fn begin_attempt(&self, st: &mut State, target: &ResolvedTarget, base: &AnswerEvent) -> Option<(CancellationToken, Arc<Slot>)> {
        let gate = self.d.budget().lock().unwrap().can_start(target.is_local);
        let ev = AnswerEvent { provider: target.cfg.id.clone(), model: target.model.clone(), ..base.clone() };
        match gate {
            Gate::Ok => {}
            Gate::Money(reason) => {
                st.snap.budget_blocked = Some(reason.clone());
                self.finish(st, AnswerEvent { status: AnswerStatus::Blocked, error: Some(app_error(ErrorCode::BudgetBlocked, Some(&reason), None)), ..ev });
                return None;
            }
            Gate::Rate | Gate::Concurrency => {
                let msg = t("Ліміт запитів застосунку — зачекайте або збільште ліміт", "App request limit — wait or raise the limit");
                self.finish(st, AnswerEvent { status: AnswerStatus::Blocked, error: Some(app_error(ErrorCode::RateLimit, Some(msg), None)), ..ev });
                return None;
            }
        }
        self.d.budget().lock().unwrap().begin();
        let cancel = CancellationToken::new();
        let slot = Arc::new(Slot { held: AtomicBool::new(true), provider: target.cfg.id.clone(), model: target.model.clone(), is_local: target.is_local });
        st.inflight.insert(base.answer_id.clone(), Inflight { cancel: cancel.clone(), question_id: base.question_id.clone(), revision: base.revision, slot: slot.clone() });
        self.publish(st, ev);
        Some((cancel, slot))
    }

    async fn run_request(self: Arc<Self>, ctx: ReqCtx) {
        let ReqCtx { q, kind, role, answer_id, base, prompt, mut target, mut cancel, mut slot, then } = ctx;
        let mut then = then;
        let mut fell_back = false;
        loop {
            let req = ChatRequest { model: target.model.clone(), messages: prompt.messages.clone(), max_tokens: prompt.max_tokens, temperature: prompt.temperature };
            let mut text = String::new();
            let mut ev = AnswerEvent { provider: target.cfg.id.clone(), model: target.model.clone(), ..base.clone() };
            let me = self.clone();
            let result = run_stream(
                target.adapter.as_ref(),
                &req,
                &cancel,
                |e| match e {
                    RunEvent::FirstToken(ms) => {
                        ev.first_token_ms = Some(ms);
                        if let Some(f) = then.take() {
                            let mut st = me.st.lock().unwrap();
                            f(&mut st);
                        }
                        me.d.emit();
                    }
                    RunEvent::Text(delta) => {
                        text.push_str(delta);
                        ev.text = text.clone();
                        let mut st = me.st.lock().unwrap();
                        me.publish(&mut st, ev.clone());
                    }
                },
                RunOptions::new(target.cfg.timeout_ms),
            )
            .await;

            match result {
                Ok(res) => {
                    let usage = (res.usage.input_tokens, res.usage.output_tokens);
                    let cost = slot.release(self.d.as_ref(), Some(usage));
                    let persist = self.st.lock().unwrap().persist;
                    self.d.db().add_usage(&self.d.new_id(), persist.then_some(q.session_id.as_str()), &target.cfg.id, &target.model, usage.0, usage.1, cost);
                    self.d.outcome(&target.cfg.id, &target.model, true);
                    let done = AnswerEvent {
                        text: res.text,
                        status: AnswerStatus::Done,
                        latency_ms: Some(res.latency_ms),
                        first_token_ms: res.first_token_ms,
                        usage: Some(res.usage),
                        usage_cost_usd: cost,
                        ..ev
                    };
                    let mut st = self.st.lock().unwrap();
                    self.finish(&mut st, done);
                    if matches!(kind, AnswerKind::Say | AnswerKind::Screen) {
                        if let Some(c) = st.coord.as_mut() {
                            c.mark_answered(&q.question_id, q.revision);
                        }
                    }
                    self.sync_cost(&mut st);
                    self.d.emit();
                    return;
                }
                Err(e) => {
                    slot.release(self.d.as_ref(), None);
                    if e.code == ErrorCode::Cancelled {
                        let mut st = self.st.lock().unwrap();
                        let status = if st.board.is_active(&q.question_id, q.revision) { AnswerStatus::Cancelled } else { AnswerStatus::Stale };
                        self.finish(&mut st, AnswerEvent { text, status, error: Some(e), ..ev });
                        self.sync_cost(&mut st);
                        return;
                    }
                    let retryable = matches!(e.code, ErrorCode::Timeout | ErrorCode::Server | ErrorCode::RateLimit | ErrorCode::Network);
                    let candidate = if !fell_back && text.is_empty() && retryable { self.d.fallback(role, &target.cfg.id) } else { None };
                    // The cloud consent gate applies to the fallback too: a local failure must not leak CV and transcript to the cloud.
                    let now = self.d.settings();
                    let fb = candidate.filter(|c| c.is_local || (now.consent.cloud && !now.offline));
                    self.d.outcome(&target.cfg.id, &target.model, false);
                    self.d.log("answer_error", json!({ "provider": target.cfg.id, "model": target.model, "code": e.code, "status": e.status, "detail": e.detail }));
                    let mut st = self.st.lock().unwrap();
                    st.inflight.remove(&answer_id);
                    if let Some(fb) = fb {
                        st.snap.notice = Some(tr!(
                            "{}: {}. Перемкнено на {} — ви дозволили його як запасний.",
                            "{}: {}. Switched to {} — you allowed it as a fallback.",
                            target.cfg.label,
                            e.message,
                            fb.cfg.label
                        ));
                        target = fb;
                        fell_back = true;
                        match self.begin_attempt(&mut st, &target, &base) {
                            Some((c, s)) => {
                                cancel = c;
                                slot = s;
                                self.sync_cost(&mut st);
                                continue;
                            }
                            None => {
                                if let Some(f) = then.take() {
                                    f(&mut st);
                                }
                                self.sync_cost(&mut st);
                                return;
                            }
                        }
                    }
                    self.finish(&mut st, AnswerEvent { text, status: AnswerStatus::Error, error: Some(e), ..ev });
                    if let Some(f) = then.take() {
                        f(&mut st);
                    }
                    self.sync_cost(&mut st);
                    self.d.emit();
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use futures_util::future::{BoxFuture, FutureExt};
    use futures_util::StreamExt;

    use super::*;
    use crate::providers::errors::err;
    use crate::providers::{ChunkStream, ProviderAdapter};

    struct Fake {
        calls: AtomicUsize,
        delay_ms: u64,
        fail: bool,
        reply: fn(&ChatRequest) -> String,
    }

    impl ProviderAdapter for Fake {
        fn list_models(&self) -> BoxFuture<'static, Result<Vec<ModelInfo>, AppError>> {
            async { Ok(vec![]) }.boxed()
        }
        fn stream(&self, req: ChatRequest) -> BoxFuture<'static, Result<ChunkStream, AppError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return async { Err(err(ErrorCode::Network)) }.boxed();
            }
            let words: Vec<String> = (self.reply)(&req).split_inclusive(' ').map(String::from).collect();
            let delay = self.delay_ms;
            async move {
                let s = futures_util::stream::iter(words)
                    .then(move |w| async move {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        Ok(StreamChunk::Text(w))
                    })
                    .chain(futures_util::stream::iter(vec![Ok(StreamChunk::Usage { input: 10, output: 5 })]));
                Ok(Box::pin(s) as ChunkStream)
            }
            .boxed()
        }
    }

    fn reply(req: &ChatRequest) -> String {
        let user = &req.messages.last().unwrap().content;
        let q = user.split("<question>\n").nth(1).and_then(|r| r.split('\n').next()).unwrap_or("?");
        if req.max_tokens > 300 {
            format!("• point for {q}")
        } else {
            format!("answer for {q}")
        }
    }

    struct Deps {
        settings: Settings,
        db: Arc<Db>,
        target: ResolvedTarget,
        cloud: ResolvedTarget,
        budget: Mutex<BudgetManager>,
        n: AtomicUsize,
    }

    impl SessionDeps for Deps {
        fn settings(&self) -> Settings {
            self.settings.clone()
        }
        fn db(&self) -> &Db {
            &self.db
        }
        fn profile(&self) -> Profile {
            Profile { id: "default".into(), name: "Test".into(), preset: ProfilePreset::Backend, style: AnswerStyle::Natural, notes: String::new() }
        }
        fn resolve(&self, _role: ModelRole) -> Option<ResolvedTarget> {
            Some(self.target.clone())
        }
        fn fallback(&self, _role: ModelRole, _exclude: &str) -> Option<ResolvedTarget> {
            Some(self.cloud.clone())
        }
        fn budget(&self) -> &Mutex<BudgetManager> {
            &self.budget
        }
        fn emit(&self) {}
        fn log(&self, _kind: &str, _data: Value) {}
        fn outcome(&self, _p: &str, _m: &str, _ok: bool) {}
        fn new_id(&self) -> String {
            format!("id{}", self.n.fetch_add(1, Ordering::SeqCst))
        }
    }

    #[derive(Default)]
    struct Opts {
        cloud_target: bool,
        cloud_consent: bool,
        delay_ms: u64,
        fail_local: bool,
        offline: bool,
        history: bool,
    }

    fn setup(o: Opts) -> (Arc<SessionManager>, Arc<Fake>, Arc<Fake>, Arc<Db>) {
        let mut settings = Settings { auto_answer: true, history: o.history, offline: o.offline, ..Default::default() };
        settings.consent.cloud = o.cloud_consent;
        settings.consent.history = o.history;
        let cfg = ProviderConfig {
            id: "p".into(),
            label: "Fake".into(),
            preset: "custom".into(),
            protocol: ProviderProtocol::Openai,
            base_url: "http://127.0.0.1:1".into(),
            has_key: false,
            allow_fallback: false,
            timeout_ms: 5000,
            llama: None,
        };
        let local = Arc::new(Fake { calls: AtomicUsize::new(0), delay_ms: o.delay_ms.max(2), fail: o.fail_local, reply });
        let cloud = Arc::new(Fake { calls: AtomicUsize::new(0), delay_ms: 2, fail: false, reply: |_| "cloud answer".into() });
        let db = Db::memory();
        let deps = Deps {
            target: ResolvedTarget { cfg: cfg.clone(), model: "m".into(), adapter: local.clone(), is_local: !o.cloud_target },
            cloud: ResolvedTarget { cfg: ProviderConfig { id: "cloud".into(), label: "Cloud".into(), allow_fallback: true, ..cfg }, model: "c".into(), adapter: cloud.clone(), is_local: false },
            // Default limits on purpose: max_concurrent=2 is what exposed the abort/slot race.
            budget: Mutex::new(BudgetManager::new(settings.budget.clone(), BTreeMap::new(), 0.0, Box::new(now_ms))),
            settings,
            db: db.clone(),
            n: AtomicUsize::new(0),
        };
        (SessionManager::new(Arc::new(deps), tokio::runtime::Handle::current()), local, cloud, db)
    }

    async fn until(m: &SessionManager, f: impl Fn(&AppSnapshot) -> bool) {
        for _ in 0..400 {
            if f(&m.snapshot()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timeout; snapshot: {:?}", m.snapshot().answers);
    }

    fn status(s: &AppSnapshot, k: AnswerKind) -> Option<AnswerStatus> {
        s.answers.get(&k).map(|a| a.status)
    }

    fn done(s: &AppSnapshot) -> bool {
        status(s, AnswerKind::Say) == Some(AnswerStatus::Done) && status(s, AnswerKind::Points) == Some(AnswerStatus::Done)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn typed_question_yields_say_and_points() {
        let (m, ..) = setup(Opts::default());
        m.start(SessionMode::Practice);
        m.ask_typed("Що таке індекс у базі даних?");
        until(&m, done).await;
        let s = m.snapshot();
        assert_eq!(s.answers[&AnswerKind::Say].text, "answer for Що таке індекс у базі даних?");
        assert!(s.answers[&AnswerKind::Points].text.contains("point for"));
        assert_eq!(s.state, ConvState::AnswerReady);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cloud_without_consent_blocked_and_not_generating() {
        let (m, local, ..) = setup(Opts { cloud_target: true, ..Default::default() });
        m.start(SessionMode::Practice);
        m.ask_typed("Чому ви пішли з минулої роботи?");
        until(&m, |s| status(s, AnswerKind::Say) == Some(AnswerStatus::Blocked)).await;
        assert_eq!(local.calls.load(Ordering::SeqCst), 0);
        assert_ne!(m.snapshot().state, ConvState::Generating);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn newer_question_replaces_streaming_answer() {
        let (m, ..) = setup(Opts { delay_ms: 15, ..Default::default() });
        m.start(SessionMode::Practice);
        m.ask_typed("Перше питання про черги повідомлень?");
        until(&m, |s| s.answers.get(&AnswerKind::Say).is_some_and(|a| !a.text.is_empty())).await;
        m.ask_typed("Друге питання про індекси в PostgreSQL?");
        until(&m, done).await;
        let s = m.snapshot();
        assert!(s.answers[&AnswerKind::Say].text.contains("Друге"));
        assert!(!s.answers[&AnswerKind::Say].text.contains("Перше"));
        assert!(s.active_question.unwrap().text.contains("Друге"));
        assert_eq!(s.state, ConvState::AnswerReady);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fallback_respects_cloud_consent() {
        let (m, _, cloud, _) = setup(Opts { fail_local: true, ..Default::default() });
        m.start(SessionMode::Practice);
        m.ask_typed("Розкажіть про себе?");
        until(&m, |s| status(s, AnswerKind::Say) == Some(AnswerStatus::Error)).await;
        assert_eq!(cloud.calls.load(Ordering::SeqCst), 0);
        assert_eq!(m.snapshot().state, ConvState::Error);

        let (m, _, cloud, _) = setup(Opts { fail_local: true, cloud_consent: true, ..Default::default() });
        m.start(SessionMode::Practice);
        m.ask_typed("Розкажіть про себе?");
        until(&m, |s| status(s, AnswerKind::Say) == Some(AnswerStatus::Done)).await;
        assert!(cloud.calls.load(Ordering::SeqCst) > 0);
        assert_eq!(m.snapshot().answers[&AnswerKind::Say].text, "cloud answer");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_blocks_cloud_model() {
        let (m, local, ..) = setup(Opts { cloud_target: true, cloud_consent: true, offline: true, ..Default::default() });
        m.start(SessionMode::Practice);
        m.ask_typed("Що таке CAP-теорема?");
        until(&m, |s| status(s, AnswerKind::Say) == Some(AnswerStatus::Blocked)).await;
        assert_eq!(m.snapshot().answers[&AnswerKind::Say].error.as_ref().unwrap().code, ErrorCode::OfflineBlocked);
        assert_eq!(local.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn own_partial_hidden_in_call_shown_in_practice() {
        let (m, ..) = setup(Opts::default());
        let id = m.start(SessionMode::Live);
        let seg = |source, text: &str| TranscriptSegment {
            session_id: id.clone(),
            segment_id: format!("s-{text}"),
            source,
            start_ms: 0,
            end_ms: 1000,
            text: text.into(),
            is_final: false,
            lang: None,
        };
        m.on_segment(seg(Source::User, "це я проговорюю питання вголос"));
        assert!(m.snapshot().partial[0].is_none());
        m.on_segment(seg(Source::Remote, "розкажіть про CAP теорему"));
        assert_eq!(m.snapshot().partial[1].as_deref(), Some("розкажіть про CAP теорему"));

        let id = m.start(SessionMode::Practice);
        m.on_segment(TranscriptSegment { session_id: id, ..seg(Source::User, "що таке транзакція") });
        assert_eq!(m.snapshot().partial[0].as_deref(), Some("що таке транзакція"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn continues_previous_session_with_rebased_transcript() {
        let (m, _, _, db) = setup(Opts { history: true, ..Default::default() });
        let id = m.start(SessionMode::Live);
        m.on_segment(TranscriptSegment {
            session_id: id.clone(),
            segment_id: "s1".into(),
            source: Source::Remote,
            start_ms: 1000,
            end_ms: 3000,
            text: "що таке транзакція в базі даних".into(),
            is_final: true,
            lang: Some("uk".into()),
        });
        m.stop();
        let r = db.last_resumable().expect("resumable");
        assert_eq!(r.id, id);
        assert_eq!(r.segments, 1);
        let new_id = m.continue_previous(db.session_segments(&r.id), r.mode);
        assert_ne!(new_id, id);
        let s = m.snapshot();
        assert_eq!(s.session_id.as_deref(), Some(new_id.as_str()));
        assert!(s.segments.iter().any(|x| x.text.contains("транзакція")));
        assert!(s.segments.last().unwrap().end_ms <= 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rapid_reasking_never_blocks_newest() {
        let (m, ..) = setup(Opts { delay_ms: 10, ..Default::default() });
        m.start(SessionMode::Practice);
        for q in ["Питання один про кеш?", "Питання два про черги?", "Питання три про індекси?", "Питання чотири про шардинг?"] {
            m.ask_typed(q);
            until(&m, |s| s.answers.get(&AnswerKind::Say).is_some_and(|a| !a.text.is_empty())).await;
        }
        until(&m, done).await;
        let s = m.snapshot();
        assert!(s.answers[&AnswerKind::Say].text.contains("чотири"));
        assert_eq!(s.state, ConvState::AnswerReady);
    }
}
