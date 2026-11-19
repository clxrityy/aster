//! Word n-gram language model with stupid-backoff scoring and temperature
//! sampling. Trained only on text the owner provides or teaches.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

pub const BOS: &str = "<s>";
pub const EOS: &str = "</s>";
const SEP: char = '\u{1f}';
const BACKOFF: f64 = 0.4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NgramModel {
    /// context (joined by U+001F; "" for unigrams) -> next token -> count
    counts: HashMap<String, HashMap<String, u32>>,
    totals: HashMap<String, u64>,
    pub tokens_seen: u64,
    pub trained_order: usize,
}

fn key(context: &[String]) -> String {
    let mut out = String::new();
    for (i, token) in context.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.push_str(token);
    }
    out
}

impl NgramModel {
    pub fn train(&mut self, tokens: &[String], order: usize) {
        if tokens.is_empty() {
            return;
        }
        let order = order.max(1);
        self.trained_order = self.trained_order.max(order);
        let mut padded = vec![BOS.to_string(); order - 1];
        padded.extend(tokens.iter().cloned());
        padded.push(EOS.to_string());
        for i in (order - 1)..padded.len() {
            for n in 1..=order {
                let context = key(&padded[i + 1 - n..i]);
                *self
                    .counts
                    .entry(context.clone())
                    .or_default()
                    .entry(padded[i].clone())
                    .or_default() += 1;
                *self.totals.entry(context).or_default() += 1;
            }
        }
        self.tokens_seen += tokens.len() as u64;
    }

    fn unigram_vocab(&self) -> usize {
        self.counts.get("").map_or(0, HashMap::len)
    }

    /// Stupid-backoff probability-like score of `word` after `context`.
    pub fn score(&self, context: &[String], word: &str, order: usize) -> f64 {
        let max_n = order.min(context.len() + 1).max(1);
        let mut factor = 1.0;
        for n in (2..=max_n).rev() {
            let ctx = key(&context[context.len() + 1 - n..]);
            if let Some(next) = self.counts.get(&ctx)
                && let Some(count) = next.get(word)
            {
                let total = self.totals.get(&ctx).copied().unwrap_or(1).max(1);
                return factor * f64::from(*count) / total as f64;
            }
            factor *= BACKOFF;
        }
        let count = self
            .counts
            .get("")
            .and_then(|next| next.get(word))
            .copied()
            .unwrap_or(0);
        let total = self.totals.get("").copied().unwrap_or(0);
        factor * (f64::from(count) + 1.0) / (total as f64 + self.unigram_vocab() as f64 + 1.0)
    }

    /// Geometric-mean score of a token sequence in 0..=1.
    pub fn fluency(&self, tokens: &[String], order: usize) -> f64 {
        if tokens.is_empty() {
            return 0.0;
        }
        let mut context = vec![BOS.to_string(); order.saturating_sub(1)];
        let mut log_sum = 0.0;
        for token in tokens {
            log_sum += self.score(&context, token, order).max(1e-12).ln();
            context.push(token.clone());
        }
        (log_sum / tokens.len() as f64).exp()
    }

    /// Continue `seed` until end-of-sentence or `max_tokens`.
    pub fn generate(
        &self,
        seed: &[String],
        max_tokens: usize,
        order: usize,
        temperature: f64,
        rng: &mut Rng,
    ) -> Vec<String> {
        let order = order.max(1);
        let mut context = vec![BOS.to_string(); order - 1];
        context.extend(seed.iter().cloned());
        let mut out: Vec<String> = seed.to_vec();
        while out.len() < max_tokens {
            let mut next = None;
            for n in (1..=order.min(context.len() + 1)).rev() {
                if let Some(candidates) = self.counts.get(&key(&context[context.len() + 1 - n..]))
                    && !candidates.is_empty()
                {
                    next = Some(candidates);
                    break;
                }
            }
            let Some(candidates) = next else { break };
            let mut options: Vec<(&String, f64)> = candidates
                .iter()
                .filter(|(token, _)| token.as_str() != BOS)
                .map(|(token, count)| (token, f64::from(*count).powf(1.0 / temperature.max(0.05))))
                .collect();
            // HashMap order is unspecified; sort for seed-reproducible sampling.
            options.sort_by(|a, b| a.0.cmp(b.0));
            let total: f64 = options.iter().map(|(_, w)| w).sum();
            if total <= 0.0 {
                break;
            }
            let mut pick = rng.next_f64() * total;
            let mut chosen = options[options.len() - 1].0;
            for (token, weight) in &options {
                if pick < *weight {
                    chosen = token;
                    break;
                }
                pick -= weight;
            }
            if chosen == EOS {
                break;
            }
            out.push(chosen.clone());
            context.push(chosen.clone());
        }
        out
    }

    pub fn knows(&self, token: &str) -> bool {
        self.counts
            .get("")
            .is_some_and(|next| next.contains_key(token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tokens;

    #[test]
    fn learns_and_generates_seen_sequences() {
        let mut model = NgramModel::default();
        for _ in 0..3 {
            model.train(&tokens("the sky is blue ."), 3);
        }
        let mut rng = Rng::new(Some(7));
        let out = model.generate(&tokens("sky"), 10, 3, 0.5, &mut rng);
        assert_eq!(out, tokens("sky is blue ."));
        assert!(
            model.fluency(&tokens("the sky is blue ."), 3)
                > model.fluency(&tokens("blue the is sky ."), 3)
        );
    }
}
