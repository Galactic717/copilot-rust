use std::collections::BTreeMap;

use crate::types::{AnswerEvent, AnswerKind};

/// The only path by which answers reach the UI snapshot. I reject any answer for a (questionId, revision)
/// other than the active one — or from a request that no longer owns its slot —
/// so a late stream can never replace the current answer.
#[derive(Default)]
pub struct AnswerBoard {
    key: Option<(String, u32)>,
    owners: BTreeMap<AnswerKind, String>,
    items: BTreeMap<AnswerKind, AnswerEvent>,
}

impl AnswerBoard {
    pub fn set_active(&mut self, question_id: Option<&str>, revision: u32) {
        let next = question_id.map(|q| (q.to_string(), revision));
        if next == self.key {
            return;
        }
        self.key = next;
        self.items.clear();
        self.owners.clear();
    }

    pub fn is_active(&self, question_id: &str, revision: u32) -> bool {
        self.key.as_ref().is_some_and(|(q, r)| q == question_id && *r == revision)
    }

    /// I reserve the slot for a new request; any previous owner's events are dropped from now on.
    pub fn claim(&mut self, kind: AnswerKind, answer_id: &str) {
        self.owners.insert(kind, answer_id.to_string());
    }

    pub fn apply(&mut self, ev: AnswerEvent) -> bool {
        if !self.is_active(&ev.question_id, ev.revision) || self.owners.get(&ev.kind) != Some(&ev.answer_id) {
            return false;
        }
        self.items.insert(ev.kind, ev);
        true
    }

    pub fn answers(&self) -> &BTreeMap<AnswerKind, AnswerEvent> {
        &self.items
    }
}
