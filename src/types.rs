use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    User,
    Remote,
}

impl Source {
    pub fn idx(self) -> usize {
        match self {
            Source::User => 0,
            Source::Remote => 1,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Source::User => "user",
            Source::Remote => "remote",
        }
    }
    pub fn parse(s: &str) -> Source {
        if s == "remote" {
            Source::Remote
        } else {
            Source::User
        }
    }
}

/// Supported languages. Settings written by the old build may still say "ru": it is read as Ukrainian.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    #[serde(alias = "ru")]
    Uk,
    En,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LangPref {
    #[default]
    Auto,
    #[serde(alias = "ru")]
    Uk,
    En,
}

impl LangPref {
    pub fn code(self) -> &'static str {
        match self {
            LangPref::Auto => "auto",
            LangPref::Uk => "uk",
            LangPref::En => "en",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSegment {
    pub session_id: String,
    pub segment_id: String,
    pub source: Source,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub is_final: bool,
    pub lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UtteranceKind {
    Question,
    FollowUp,
    Statement,
    SmallTalk,
    CodingTask,
    ScreenReference,
    Unknown,
}

impl UtteranceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UtteranceKind::Question => "question",
            UtteranceKind::FollowUp => "follow_up",
            UtteranceKind::Statement => "statement",
            UtteranceKind::SmallTalk => "small_talk",
            UtteranceKind::CodingTask => "coding_task",
            UtteranceKind::ScreenReference => "screen_reference",
            UtteranceKind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvState {
    Idle,
    Listening,
    PartialTranscript,
    QuestionCandidate,
    QuestionConfirmed,
    Generating,
    AnswerReady,
    Superseded,
    Paused,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionTrigger {
    Auto,
    Manual,
    Hotkey,
    Typed,
}

impl QuestionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionTrigger::Auto => "auto",
            QuestionTrigger::Manual => "manual",
            QuestionTrigger::Hotkey => "hotkey",
            QuestionTrigger::Typed => "typed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionStatus {
    Confirmed,
    Superseded,
    Answered,
}

impl QuestionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionStatus::Confirmed => "confirmed",
            QuestionStatus::Superseded => "superseded",
            QuestionStatus::Answered => "answered",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestionEvent {
    pub session_id: String,
    pub question_id: String,
    pub revision: u32,
    pub source: Source,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: UtteranceKind,
    pub status: QuestionStatus,
    pub supersedes: Option<String>,
    pub trigger: QuestionTrigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnswerKind {
    Say,
    Points,
    Deep,
    Screen,
}

impl AnswerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerKind::Say => "say",
            AnswerKind::Points => "points",
            AnswerKind::Deep => "deep",
            AnswerKind::Screen => "screen",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenTask {
    Explain,
    Approach,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerStatus {
    Streaming,
    Done,
    Cancelled,
    Error,
    Stale,
    Blocked,
}

impl AnswerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerStatus::Streaming => "streaming",
            AnswerStatus::Done => "done",
            AnswerStatus::Cancelled => "cancelled",
            AnswerStatus::Error => "error",
            AnswerStatus::Stale => "stale",
            AnswerStatus::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnswerEvent {
    pub session_id: String,
    pub question_id: String,
    pub revision: u32,
    pub answer_id: String,
    pub kind: AnswerKind,
    pub provider: String,
    pub model: String,
    pub prompt_preset: String,
    pub text: String,
    pub status: AnswerStatus,
    pub first_token_ms: Option<u64>,
    pub latency_ms: Option<u64>,
    pub usage: Option<Usage>,
    pub usage_cost_usd: Option<f64>,
    pub error: Option<AppError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Auth,
    Quota,
    RateLimit,
    Server,
    Timeout,
    Network,
    OfflineBlocked,
    BudgetBlocked,
    BadRequest,
    NotFound,
    Cancelled,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<String>,
    pub status: Option<u16>,
    /// From a Retry-After header, already clamped by the runner.
    pub retry_after_ms: Option<u64>,
}

impl AppError {
    /// "message — detail" for one-line display.
    pub fn line(&self) -> String {
        match &self.detail {
            Some(d) if !d.is_empty() => format!("{} — {}", self.message, d),
            _ => self.message.clone(),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line())
    }
}

impl std::error::Error for AppError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProtocol {
    #[default]
    Openai,
    Anthropic,
    Gemini,
}

/// How to start llama-server from the app (only for the llama.cpp preset). The port comes from the base URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LlamaLaunch {
    pub server_path: String,
    pub model_path: String,
    pub ctx_size: u32,
    pub gpu_layers: i32,
    pub auto_start: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    pub label: String,
    /// Catalog id from `providers::presets` (openai, openrouter, llamacpp, custom…).
    pub preset: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    #[serde(default)]
    pub has_key: bool,
    #[serde(default)]
    pub allow_fallback: bool,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llama: Option<LlamaLaunch>,
}

fn default_timeout() -> u64 {
    12_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    QuickAnswer,
    DeepAnswer,
    Vision,
    Classification,
    PostSessionSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleTarget {
    pub provider_id: String,
    pub model: String,
}

pub type RoleMap = BTreeMap<ModelRole, Option<RoleTarget>>;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub label: Option<String>,
    pub vision: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatImage {
    pub mime: String,
    pub base64: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub images: Vec<ChatImage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    Text(String),
    Usage { input: u64, output: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BudgetLimits {
    pub per_session_usd: Option<f64>,
    pub per_day_usd: Option<f64>,
    pub requests_per_minute: u32,
    pub max_concurrent: u32,
}

impl Default for BudgetLimits {
    fn default() -> Self {
        Self { per_session_usd: None, per_day_usd: None, requests_per_minute: 20, max_concurrent: 2 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelPrice {
    pub input_per_m_tok: f64,
    pub output_per_m_tok: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePreset {
    Hr,
    #[default]
    Backend,
    Frontend,
    Devops,
    SystemDesign,
    Coding,
    Meeting,
}

impl ProfilePreset {
    pub const ALL: [ProfilePreset; 7] = [
        ProfilePreset::Hr,
        ProfilePreset::Backend,
        ProfilePreset::Frontend,
        ProfilePreset::Devops,
        ProfilePreset::SystemDesign,
        ProfilePreset::Coding,
        ProfilePreset::Meeting,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            ProfilePreset::Hr => "hr",
            ProfilePreset::Backend => "backend",
            ProfilePreset::Frontend => "frontend",
            ProfilePreset::Devops => "devops",
            ProfilePreset::SystemDesign => "system_design",
            ProfilePreset::Coding => "coding",
            ProfilePreset::Meeting => "meeting",
        }
    }
    pub fn parse(s: &str) -> Self {
        Self::ALL.into_iter().find(|p| p.as_str() == s).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnswerStyle {
    Short,
    #[default]
    Natural,
    Academic,
    EnglishB2,
}

impl AnswerStyle {
    pub const ALL: [AnswerStyle; 4] = [AnswerStyle::Short, AnswerStyle::Natural, AnswerStyle::Academic, AnswerStyle::EnglishB2];
    pub fn as_str(self) -> &'static str {
        match self {
            AnswerStyle::Short => "short",
            AnswerStyle::Natural => "natural",
            AnswerStyle::Academic => "academic",
            AnswerStyle::EnglishB2 => "english_b2",
        }
    }
    pub fn parse(s: &str) -> Self {
        Self::ALL.into_iter().find(|p| p.as_str() == s).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub preset: ProfilePreset,
    pub style: AnswerStyle,
    pub notes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Cv,
    Jd,
    Notes,
}

impl DocKind {
    pub const ALL: [DocKind; 3] = [DocKind::Cv, DocKind::Jd, DocKind::Notes];
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::Cv => "cv",
            DocKind::Jd => "jd",
            DocKind::Notes => "notes",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "jd" => DocKind::Jd,
            "notes" => DocKind::Notes,
            _ => DocKind::Cv,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeDoc {
    pub id: String,
    pub profile_id: String,
    pub name: String,
    pub kind: DocKind,
    pub chunks: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutAction {
    ToggleOverlay,
    ToggleListen,
    AnswerNow,
    SnapRegion,
    ScrollUp,
    ScrollDown,
    Expand,
    Panic,
}

impl ShortcutAction {
    pub const ALL: [ShortcutAction; 8] = [
        ShortcutAction::ToggleOverlay,
        ShortcutAction::ToggleListen,
        ShortcutAction::AnswerNow,
        ShortcutAction::SnapRegion,
        ShortcutAction::ScrollUp,
        ShortcutAction::ScrollDown,
        ShortcutAction::Expand,
        ShortcutAction::Panic,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Shortcuts {
    pub toggle_overlay: String,
    pub toggle_listen: String,
    pub answer_now: String,
    pub snap_region: String,
    pub scroll_up: String,
    pub scroll_down: String,
    pub expand: String,
    pub panic: String,
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self {
            toggle_overlay: "Control+Alt+H".into(),
            toggle_listen: "Control+Alt+L".into(),
            answer_now: "Control+Alt+Enter".into(),
            snap_region: "Control+Alt+S".into(),
            scroll_up: "Control+Alt+Up".into(),
            scroll_down: "Control+Alt+Down".into(),
            expand: "Control+Alt+E".into(),
            panic: "Control+Alt+Backspace".into(),
        }
    }
}

impl Shortcuts {
    pub fn get(&self, a: ShortcutAction) -> &str {
        match a {
            ShortcutAction::ToggleOverlay => &self.toggle_overlay,
            ShortcutAction::ToggleListen => &self.toggle_listen,
            ShortcutAction::AnswerNow => &self.answer_now,
            ShortcutAction::SnapRegion => &self.snap_region,
            ShortcutAction::ScrollUp => &self.scroll_up,
            ShortcutAction::ScrollDown => &self.scroll_down,
            ShortcutAction::Expand => &self.expand,
            ShortcutAction::Panic => &self.panic,
        }
    }
    pub fn set(&mut self, a: ShortcutAction, v: String) {
        let slot = match a {
            ShortcutAction::ToggleOverlay => &mut self.toggle_overlay,
            ShortcutAction::ToggleListen => &mut self.toggle_listen,
            ShortcutAction::AnswerNow => &mut self.answer_now,
            ShortcutAction::SnapRegion => &mut self.snap_region,
            ShortcutAction::ScrollUp => &mut self.scroll_up,
            ShortcutAction::ScrollDown => &mut self.scroll_down,
            ShortcutAction::Expand => &mut self.expand,
            ShortcutAction::Panic => &mut self.panic,
        };
        *slot = v;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OverlaySettings {
    pub bounds: Option<Bounds>,
    pub display_id: Option<i64>,
    pub opacity: f32,
    pub font_scale: f32,
    pub expanded: bool,
    pub theme: Theme,
    pub protect: bool,
    pub pinned: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self { bounds: None, display_id: None, opacity: 0.94, font_scale: 1.0, expanded: false, theme: Theme::Dark, protect: true, pinned: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Consent {
    pub mic: bool,
    pub system: bool,
    pub screen: bool,
    pub cloud: bool,
    pub history: bool,
    pub participants: bool,
    pub accepted_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AsrBackend {
    #[default]
    Cpu,
    Cuda,
}

impl AsrBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            AsrBackend::Cpu => "cpu",
            AsrBackend::Cuda => "cuda",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AsrSettings {
    pub backend: AsrBackend,
    pub model: String,
    pub language: LangPref,
    pub vocabulary: Vec<String>,
    pub threads: u32,
}

impl Default for AsrSettings {
    fn default() -> Self {
        Self {
            backend: AsrBackend::Cpu,
            model: "base".into(),
            language: LangPref::Uk,
            vocabulary: ["PostgreSQL", "FastAPI", "Kubernetes", "Docker", "React", "TypeScript"].map(String::from).to_vec(),
            threads: 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioSettings {
    pub mic_device_id: Option<String>,
    pub capture_mic: bool,
    pub capture_system: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { mic_device_id: None, capture_mic: true, capture_system: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowSettings {
    /// Close/minimize hides the main window to the tray instead of quitting; the session keeps running.
    pub minimize_to_tray: bool,
    /// Apply WDA_EXCLUDEFROMCAPTURE to the main window and region picker too, so a shared screen does not reveal them.
    pub protect_from_capture: bool,
    /// Emergency stop fully quits the app (process gone) instead of only stopping capture and hiding the overlay.
    pub panic_quits: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self { minimize_to_tray: true, protect_from_capture: true, panic_quits: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub onboarded: bool,
    pub ui_lang: Lang,
    pub answer_lang: LangPref,
    pub offline: bool,
    pub auto_answer: bool,
    pub asr: AsrSettings,
    pub audio: AudioSettings,
    pub context_window_min: u32,
    pub overlay: OverlaySettings,
    pub shortcuts: Shortcuts,
    pub history: bool,
    pub budget: BudgetLimits,
    pub pricing: BTreeMap<String, ModelPrice>,
    pub roles: RoleMap,
    pub active_profile_id: String,
    pub consent: Consent,
    pub window: WindowSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            onboarded: false,
            ui_lang: Lang::Uk,
            answer_lang: LangPref::Auto,
            offline: false,
            auto_answer: false,
            asr: AsrSettings::default(),
            audio: AudioSettings::default(),
            context_window_min: 3,
            overlay: OverlaySettings::default(),
            shortcuts: Shortcuts::default(),
            history: true,
            budget: BudgetLimits::default(),
            pricing: BTreeMap::new(),
            roles: BTreeMap::new(),
            active_profile_id: "default".into(),
            consent: Consent::default(),
            window: WindowSettings::default(),
        }
    }
}

impl Settings {
    pub fn role(&self, r: ModelRole) -> Option<&RoleTarget> {
        self.roles.get(&r).and_then(|t| t.as_ref()).filter(|t| !t.model.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentState {
    Ok,
    Off,
    Warn,
    Error,
    Busy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComponentStatus {
    pub state: ComponentState,
    pub label: String,
}

impl ComponentStatus {
    pub fn new(state: ComponentState, label: impl Into<String>) -> Self {
        Self { state, label: label.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKey {
    Mic,
    System,
    Asr,
    Overlay,
    Protection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statuses {
    pub mic: ComponentStatus,
    pub system: ComponentStatus,
    pub asr: ComponentStatus,
    pub llm: ComponentStatus,
    pub vision: ComponentStatus,
    pub overlay: ComponentStatus,
    pub protection: ComponentStatus,
    pub network: ComponentStatus,
}

impl Statuses {
    pub fn get_mut(&mut self, k: StatusKey) -> &mut ComponentStatus {
        match k {
            StatusKey::Mic => &mut self.mic,
            StatusKey::System => &mut self.system,
            StatusKey::Asr => &mut self.asr,
            StatusKey::Overlay => &mut self.overlay,
            StatusKey::Protection => &mut self.protection,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveQuestion {
    pub question_id: String,
    pub revision: u32,
    pub text: String,
    pub source: Source,
    pub kind: UtteranceKind,
    pub trigger: QuestionTrigger,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub question_id: String,
    pub text: String,
    pub kind: UtteranceKind,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SessionCost {
    pub usd: Option<f64>,
    pub unknown_priced: u32,
    pub requests: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppSnapshot {
    pub session_id: Option<String>,
    pub state: ConvState,
    pub listening: bool,
    pub active_question: Option<ActiveQuestion>,
    pub candidates: Vec<Candidate>,
    pub answers: BTreeMap<AnswerKind, AnswerEvent>,
    pub recent_questions: Vec<(String, String)>,
    /// Indexed by `Source::idx()`.
    pub partial: [Option<String>; 2],
    pub segments: Vec<TranscriptSegment>,
    pub status: Statuses,
    pub cost: SessionCost,
    pub budget_blocked: Option<String>,
    pub levels: [f32; 2],
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Practice,
    Live,
    Meeting,
}

impl SessionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionMode::Practice => "practice",
            SessionMode::Live => "live",
            SessionMode::Meeting => "meeting",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "practice" => SessionMode::Practice,
            "meeting" => SessionMode::Meeting,
            _ => SessionMode::Live,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagCheck {
    pub id: String,
    pub label: String,
    pub ok: Option<bool>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: String,
    pub title: String,
    pub questions: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResumableSession {
    pub id: String,
    pub title: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub mode: SessionMode,
    pub interrupted: bool,
    pub segments: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub name: String,
    pub received: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsrStatus {
    pub binary: bool,
    pub model: String,
    pub model_present: bool,
    pub running: bool,
    pub last_latency_ms: Option<u64>,
    pub download: Option<Download>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recipient {
    pub label: String,
    pub base_url: String,
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingPreview {
    pub recipients: Vec<Recipient>,
    pub items: Vec<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
