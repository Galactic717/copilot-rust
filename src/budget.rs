use std::collections::{BTreeMap, VecDeque};

use chrono::{Datelike, TimeZone};

use crate::types::{BudgetLimits, ModelPrice, SessionCost};

#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    Ok,
    Concurrency,
    Rate,
    /// Money limit reached; carries the user-facing reason.
    Money(String),
}

/// I apply money limits only to paid (non-loopback) providers with a known price.
/// With an unknown price, cost stays None and the UI warns that $-limits cannot be enforced for that model.
pub struct BudgetManager {
    limits: BudgetLimits,
    pricing: BTreeMap<String, ModelPrice>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    session_usd: f64,
    day_usd: f64,
    unknown_priced: u32,
    requests: u32,
    recent: VecDeque<i64>,
    inflight: u32,
    override_session: bool,
    override_day: bool,
    day: (i32, u32, u32),
}

fn day_key(ms: i64) -> (i32, u32, u32) {
    let d = chrono::Local.timestamp_millis_opt(ms).single().unwrap_or_else(chrono::Local::now);
    (d.year(), d.month(), d.day())
}

impl BudgetManager {
    pub fn new(limits: BudgetLimits, pricing: BTreeMap<String, ModelPrice>, spent_today_before: f64, now: Box<dyn Fn() -> i64 + Send + Sync>) -> Self {
        let day = day_key(now());
        Self {
            limits,
            pricing,
            now,
            session_usd: 0.0,
            day_usd: spent_today_before,
            unknown_priced: 0,
            requests: 0,
            recent: VecDeque::new(),
            inflight: 0,
            override_session: false,
            override_day: false,
            day,
        }
    }

    // The app may stay open across midnight, so I restart the daily limit from zero.
    fn rollover(&mut self) {
        let d = day_key((self.now)());
        if d != self.day {
            self.day = d;
            self.day_usd = 0.0;
            self.override_day = false;
        }
    }

    pub fn update(&mut self, limits: BudgetLimits, pricing: BTreeMap<String, ModelPrice>) {
        self.limits = limits;
        self.pricing = pricing;
    }

    pub fn reset_session(&mut self) {
        self.session_usd = 0.0;
        self.unknown_priced = 0;
        self.requests = 0;
        self.override_session = false;
    }

    pub fn can_start(&mut self, is_local: bool) -> Gate {
        self.rollover();
        let now = (self.now)();
        while self.recent.front().is_some_and(|t| now - t >= 60_000) {
            self.recent.pop_front();
        }
        if self.inflight >= self.limits.max_concurrent.max(1) {
            return Gate::Concurrency;
        }
        if self.recent.len() as u32 >= self.limits.requests_per_minute.max(1) {
            return Gate::Rate;
        }
        if is_local {
            return Gate::Ok;
        }
        if let Some(lim) = self.limits.per_session_usd {
            if !self.override_session && self.session_usd >= lim {
                return Gate::Money(crate::tr!("Досягнуто ліміту ${} на сесію", "Session limit of ${} reached", lim));
            }
        }
        if let Some(lim) = self.limits.per_day_usd {
            if !self.override_day && self.day_usd >= lim {
                return Gate::Money(crate::tr!("Досягнуто ліміту ${} на день", "Daily limit of ${} reached", lim));
            }
        }
        Gate::Ok
    }

    pub fn begin(&mut self) {
        self.inflight += 1;
        self.requests += 1;
        let now = (self.now)();
        self.recent.push_back(now);
    }

    /// Returns cost in USD, or None when the price is unknown (or there was no usage).
    pub fn end(&mut self, provider_id: &str, model: &str, is_local: bool, usage: Option<(u64, u64)>) -> Option<f64> {
        self.inflight = self.inflight.saturating_sub(1);
        let (input, output) = usage?;
        if is_local {
            return Some(0.0);
        }
        let Some(price) = self.pricing.get(&format!("{provider_id}/{model}")).copied() else {
            self.unknown_priced += 1;
            return None;
        };
        self.rollover();
        let cost = (input as f64 * price.input_per_m_tok + output as f64 * price.output_per_m_tok) / 1_000_000.0;
        self.session_usd += cost;
        self.day_usd += cost;
        Some(cost)
    }

    pub fn approve_override(&mut self) {
        if self.limits.per_session_usd.is_some_and(|l| self.session_usd >= l) {
            self.override_session = true;
        }
        if self.limits.per_day_usd.is_some_and(|l| self.day_usd >= l) {
            self.override_day = true;
        }
    }

    pub fn snapshot(&self) -> SessionCost {
        SessionCost { usd: Some(if self.requests == 0 { 0.0 } else { self.session_usd }), unknown_priced: self.unknown_priced, requests: self.requests }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn daily_budget_restarts_after_midnight() {
        let at = |d: u32, h: u32, m: u32| chrono::Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().timestamp_millis();
        let now = Arc::new(AtomicI64::new(at(28, 23, 59)));
        let n2 = now.clone();
        let mut pricing = BTreeMap::new();
        pricing.insert("p/m".to_string(), ModelPrice { input_per_m_tok: 1_000_000.0, output_per_m_tok: 0.0 });
        let mut b = BudgetManager::new(
            BudgetLimits { per_session_usd: None, per_day_usd: Some(1.0), requests_per_minute: 100, max_concurrent: 5 },
            pricing,
            0.0,
            Box::new(move || n2.load(Ordering::SeqCst)),
        );
        b.begin();
        b.end("p", "m", false, Some((2, 0)));
        assert!(matches!(b.can_start(false), Gate::Money(_)));
        now.store(at(29, 0, 1), Ordering::SeqCst);
        assert_eq!(b.can_start(false), Gate::Ok);
    }
}
