//! Words that mean the same thing are matched as one concept: plural and
//! possessive forms fold to their base ("nebulas" → "nebula"), and synonym
//! groups ("hi", "hey", "hello") fold to the group's first member. Matching
//! replies and meanings goes through this, so one taught reply covers many
//! phrasings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::text::{is_stopword, tokens, words};

/// Longest synonym phrase considered, in words ("thank you very much").
const MAX_PHRASE: usize = 4;

/// Words ending in "s" that are not plurals.
const NOT_PLURAL: &[&str] = &[
    "always",
    "perhaps",
    "news",
    "series",
    "species",
    "lens",
    "bus",
    "gas",
    "yes",
    "this",
    "thus",
    "its",
    "his",
    "has",
    "was",
    "does",
    "is",
    "us",
    "plus",
    "chaos",
    "bias",
    "atlas",
    "canvas",
    "physics",
    "mathematics",
    "politics",
    "aster's",
];

/// Fold plural and possessive forms to a base form. Deliberately light: it
/// only removes endings that almost never change a word's meaning.
pub fn stem(word: &str) -> String {
    let word = word.strip_suffix("'s").unwrap_or(word);
    let len = word.chars().count();
    if len <= 3 || is_stopword(word) || NOT_PLURAL.contains(&word) {
        return word.to_string();
    }
    if let Some(base) = word.strip_suffix("ies")
        && len > 4
    {
        return format!("{base}y");
    }
    if word.ends_with("sses") {
        return word[..word.len() - 2].to_string();
    }
    for ending in ["xes", "ches", "shes", "zzes"] {
        if word.ends_with(ending) {
            return word[..word.len() - 2].to_string();
        }
    }
    if word.ends_with('s')
        && !word.ends_with("ss")
        && !word.ends_with("us")
        && !word.ends_with("is")
    {
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

fn stem_phrase(phrase: &str) -> String {
    phrase.split(' ').map(stem).collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Synonyms {
    /// Member phrase (lowercase words, also its stemmed form) → canonical concept.
    map: BTreeMap<String, String>,
}

impl Synonyms {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, word: &str) -> bool {
        self.map.contains_key(word) || self.map.contains_key(&stem(word))
    }

    /// Join phrases into one concept. If any member already belongs to a
    /// concept, the whole group joins that concept.
    pub fn add_group<S: AsRef<str>>(&mut self, members: &[S]) -> usize {
        let members: Vec<String> = members
            .iter()
            .map(|m| words(&tokens(m.as_ref())).join(" "))
            .filter(|m| !m.is_empty() && m.split(' ').count() <= MAX_PHRASE)
            .collect();
        let Some(first) = members.first() else {
            return 0;
        };
        let canonical = members
            .iter()
            .find_map(|m| self.lookup(m))
            .unwrap_or_else(|| stem_phrase(first));
        for member in &members {
            let previous = self.lookup(member);
            self.map.insert(member.clone(), canonical.clone());
            self.map.insert(stem_phrase(member), canonical.clone());
            // A member that led another concept brings that concept along.
            if let Some(previous) = previous.filter(|p| *p != canonical) {
                for value in self.map.values_mut() {
                    if *value == previous {
                        *value = canonical.clone();
                    }
                }
            }
        }
        members.len()
    }

    fn lookup(&self, phrase: &str) -> Option<String> {
        self.map
            .get(phrase)
            .or_else(|| self.map.get(&stem_phrase(phrase)))
            .cloned()
    }

    /// Words → concepts: the longest synonym phrase at each position wins;
    /// other words are stemmed.
    pub fn canonical(&self, words: &[String]) -> Vec<String> {
        let mut out = Vec::with_capacity(words.len());
        let mut i = 0;
        'outer: while i < words.len() {
            for n in (1..=MAX_PHRASE.min(words.len() - i)).rev() {
                if let Some(concept) = self.lookup(&words[i..i + n].join(" ")) {
                    out.push(concept);
                    i += n;
                    continue 'outer;
                }
            }
            out.push(stem(&words[i]));
            i += 1;
        }
        out
    }

    /// One phrase → its concept (stemmed when it has no synonyms).
    pub fn concept(&self, phrase: &str) -> String {
        self.lookup(phrase).unwrap_or_else(|| stem_phrase(phrase))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str) -> Vec<String> {
        words(&tokens(text))
    }

    #[test]
    fn folds_plurals_and_possessives_only() {
        for (word, base) in [
            ("nebulas", "nebula"),
            ("stories", "story"),
            ("boxes", "box"),
            ("classes", "class"),
            ("friend's", "friend"),
            ("news", "news"),
            ("this", "this"),
            ("glass", "glass"),
            ("status", "status"),
            ("is", "is"),
            ("cats", "cat"),
        ] {
            assert_eq!(stem(word), base, "{word}");
        }
    }

    #[test]
    fn synonym_groups_fold_to_one_concept() {
        let mut synonyms = Synonyms::default();
        synonyms.add_group(&["hello", "hi", "hey", "good morning"]);
        synonyms.add_group(&["thanks", "thank you"]);
        assert_eq!(synonyms.canonical(&w("Hey there")), ["hello", "there"]);
        assert_eq!(
            synonyms.canonical(&w("good morning aster")),
            ["hello", "aster"]
        );
        assert_eq!(
            synonyms.canonical(&w("thank you so much")),
            ["thank", "so", "much"]
        );
        assert_eq!(synonyms.canonical(&w("many stars")), ["many", "star"]);
        // Joining a member of an existing group extends that group.
        synonyms.add_group(&["howdy", "hey"]);
        assert_eq!(synonyms.concept("howdy"), "hello");
        // Merging two groups moves every member.
        synonyms.add_group(&["thanks", "hi"]);
        assert_eq!(synonyms.concept("thank you"), synonyms.concept("hello"));
    }
}
