//! BM25 recall over memories, answers, and ingested passages.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::text::{content_terms, tokens};

const K1: f64 = 1.2;
const B: f64 = 0.75;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doc {
    pub id: String,
    pub source: String,
    pub text: String,
    tf: HashMap<String, u32>,
    len: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecallIndex {
    docs: Vec<Doc>,
    df: HashMap<String, u32>,
    total_len: u64,
}

impl RecallIndex {
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    pub fn docs(&self) -> &[Doc] {
        &self.docs
    }

    pub fn add(&mut self, id: &str, source: &str, text: &str) {
        self.remove(id);
        let terms = content_terms(&tokens(text));
        let mut tf: HashMap<String, u32> = HashMap::new();
        let all = tokens(text);
        for token in all.iter().filter(|t| terms.contains(t)) {
            *tf.entry(token.clone()).or_default() += 1;
        }
        for term in tf.keys() {
            *self.df.entry(term.clone()).or_default() += 1;
        }
        let len = tf.values().sum::<u32>().max(1);
        self.total_len += u64::from(len);
        self.docs.push(Doc {
            id: id.into(),
            source: source.into(),
            text: text.into(),
            tf,
            len,
        });
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let Some(position) = self.docs.iter().position(|doc| doc.id == id) else {
            return false;
        };
        let doc = self.docs.remove(position);
        for term in doc.tf.keys() {
            if let Some(count) = self.df.get_mut(term) {
                *count -= 1;
                if *count == 0 {
                    self.df.remove(term);
                }
            }
        }
        self.total_len -= u64::from(doc.len);
        true
    }

    pub fn idf(&self, term: &str) -> f64 {
        let n = self.docs.len() as f64;
        let df = f64::from(self.df.get(term).copied().unwrap_or(0));
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// Top `k` documents for `terms`, best first, with positive scores only.
    pub fn search(&self, terms: &[String], k: usize) -> Vec<(&Doc, f64)> {
        if self.docs.is_empty() || terms.is_empty() {
            return Vec::new();
        }
        let average = self.total_len as f64 / self.docs.len() as f64;
        let mut scored: Vec<(&Doc, f64)> = self
            .docs
            .iter()
            .map(|doc| {
                let score = terms
                    .iter()
                    .map(|term| {
                        let tf = f64::from(doc.tf.get(term).copied().unwrap_or(0));
                        if tf == 0.0 {
                            return 0.0;
                        }
                        let norm = K1 * (1.0 - B + B * f64::from(doc.len) / average);
                        self.idf(term) * tf * (K1 + 1.0) / (tf + norm)
                    })
                    .sum::<f64>();
                (doc, score)
            })
            .filter(|(_, score)| *score > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(k);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_documents_and_forgets() {
        let mut index = RecallIndex::default();
        index.add(
            "a",
            "memory",
            "Nebulae are clouds of gas and dust in space.",
        );
        index.add("b", "memory", "Rust is a systems programming language.");
        let hits = index.search(&tokens("what is a nebulae"), 5);
        assert_eq!(hits[0].0.id, "a");
        assert!(index.remove("a"));
        assert!(index.search(&tokens("nebulae"), 5).is_empty());
        assert!(!index.remove("a"));
    }
}
