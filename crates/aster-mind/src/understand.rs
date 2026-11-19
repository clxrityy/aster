//! A first reading of each message, before any reply is scored: is the owner
//! asking what something means, telling Aster what something means, or just
//! talking? Meanings and replies are learned separately, so knowing what a word
//! means never turns into saying that meaning whenever the word comes up.

use crate::text::{tokens, words};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// "what is X", "what does X mean", "define X", "who is X", "tell me about X".
    AskMeaning { subject: String },
    /// "X means Y" (`means: true`) or "X is Y". The subject may be a pronoun
    /// ("it", "that") for the mind to resolve against what it just asked.
    State {
        subject: String,
        meaning: String,
        means: bool,
    },
    /// Anything else: answered from learned replies.
    Talk,
}

const ARTICLES: [&str; 3] = ["a", "an", "the"];
const PRONOUNS: [&str; 3] = ["it", "that", "this"];
const PERSONAL: &[&str] = &[
    "i",
    "me",
    "my",
    "mine",
    "myself",
    "you",
    "your",
    "yours",
    "yourself",
    "we",
    "us",
    "our",
    "he",
    "she",
    "him",
    "her",
    "they",
    "them",
    "their",
    "these",
    "those",
    "there",
    "here",
    "it",
    "that",
    "this",
    "up",
    "going",
    "happening",
];
const QUESTION_WORDS: &[&str] = &[
    "what", "who", "how", "why", "where", "when", "which", "do", "does", "did", "is", "are", "can",
    "could", "would", "should", "will",
];
const MAX_SUBJECT_WORDS: usize = 4;

pub fn read(text: &str) -> Intent {
    let w = words(&tokens(text));
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    if let Some(subject) = asked_subject(&w) {
        return Intent::AskMeaning { subject };
    }
    if let Some((subject, meaning, means)) = statement(text) {
        return Intent::State {
            subject,
            meaning,
            means,
        };
    }
    Intent::Talk
}

/// The lookup key for a subject: lowercase words, leading articles dropped.
pub fn key(subject: &str) -> String {
    let w = words(&tokens(subject));
    let mut w: &[String] = &w;
    while let [first, rest @ ..] = w
        && ARTICLES.contains(&first.as_str())
    {
        w = rest;
    }
    w.join(" ")
}

pub fn is_pronoun(subject: &str) -> bool {
    PRONOUNS.contains(&subject)
}

/// Strip the framing people naturally put around a meaning ("it means …",
/// "hello is …") so only the meaning itself is stored.
pub fn clean_meaning(subject: &str, meaning: &str) -> String {
    let trimmed = meaning.trim();
    let lower = trimmed.to_ascii_lowercase();
    let subject = subject.to_ascii_lowercase();
    let prefixes = [
        format!("{subject} means "),
        format!("{subject} is "),
        format!("{subject} are "),
        "it means ".into(),
        "that means ".into(),
        "it's ".into(),
        "it’s ".into(),
        "it is ".into(),
        "means ".into(),
    ];
    for prefix in &prefixes {
        if lower.starts_with(prefix.as_str()) {
            let rest = trimmed[prefix.len()..].trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Say a stored meaning back as a sentence.
pub fn phrase(subject: &str, meaning: &str) -> String {
    let meaning = meaning.trim();
    let mut out = if meaning.to_lowercase().starts_with(&subject.to_lowercase()) {
        let mut chars = meaning.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    } else {
        format!("“{subject}” means {meaning}")
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

fn asked_subject(w: &[&str]) -> Option<String> {
    let rest: &[&str] = match w {
        ["what", "does" | "do", rest @ .., "mean"] => rest,
        [
            "do",
            "you",
            "know",
            "what",
            rest @ ..,
            "is" | "are" | "means" | "mean",
        ] => rest,
        [
            "what" | "who",
            "is" | "are",
            "the",
            "meaning" | "definition",
            "of",
            rest @ ..,
        ] => rest,
        ["meaning" | "definition", "of", rest @ ..] => rest,
        ["what" | "who", "is" | "are", rest @ ..] => rest,
        ["what's" | "whats" | "who's", rest @ ..] => rest,
        ["define", rest @ ..] => rest,
        ["tell", "me", "about", rest @ ..] => rest,
        _ => return None,
    };
    let mut rest = rest;
    while let [first, tail @ ..] = rest
        && ARTICLES.contains(first)
    {
        rest = tail;
    }
    if rest.is_empty()
        || rest.len() > MAX_SUBJECT_WORDS
        || rest.iter().any(|w| PERSONAL.contains(w))
    {
        return None;
    }
    Some(rest.join(" "))
}

fn statement(text: &str) -> Option<(String, String, bool)> {
    let trimmed = text.trim();
    if trimmed.ends_with('?') {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let (subject, meaning, means) = if let Some(rest) = lower
        .strip_prefix("it's ")
        .or_else(|| lower.strip_prefix("it’s "))
    {
        ("it", &trimmed[trimmed.len() - rest.len()..], false)
    } else if let Some(i) = lower.find(" means ") {
        (&trimmed[..i], &trimmed[i + " means ".len()..], true)
    } else {
        let i = lower.find(" is ").or_else(|| lower.find(" are "))?;
        let verb = if lower[i..].starts_with(" is ") { 4 } else { 5 };
        (&trimmed[..i], &trimmed[i + verb..], false)
    };
    let meaning = meaning.trim();
    let subject = key(subject);
    let subject_words: Vec<&str> = subject.split(' ').collect();
    if meaning.is_empty()
        || subject.is_empty()
        || subject_words.len() > MAX_SUBJECT_WORDS
        || QUESTION_WORDS.contains(&subject_words[0])
    {
        return None;
    }
    let personal = subject_words.iter().any(|w| PERSONAL.contains(w));
    if personal && !is_pronoun(&subject) {
        return None;
    }
    Some((subject, meaning.to_string(), means))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asks(text: &str) -> Option<String> {
        match read(text) {
            Intent::AskMeaning { subject } => Some(subject),
            _ => None,
        }
    }

    #[test]
    fn recognizes_questions_about_meaning() {
        assert_eq!(asks("What is a nebula?").as_deref(), Some("nebula"));
        assert_eq!(asks("what's hello").as_deref(), Some("hello"));
        assert_eq!(asks("What does hello mean?").as_deref(), Some("hello"));
        assert_eq!(asks("define black hole").as_deref(), Some("black hole"));
        assert_eq!(
            asks("what is the meaning of entropy").as_deref(),
            Some("entropy")
        );
        assert_eq!(asks("tell me about Rust").as_deref(), Some("rust"));
        assert_eq!(asks("what is your name"), None);
        assert_eq!(asks("what is up"), None);
        assert_eq!(asks("hello"), None);
    }

    #[test]
    fn recognizes_statements_of_meaning() {
        assert_eq!(
            read("A nebula is a cloud of gas."),
            Intent::State {
                subject: "nebula".into(),
                meaning: "a cloud of gas.".into(),
                means: false
            }
        );
        assert_eq!(
            read("hello means a friendly greeting"),
            Intent::State {
                subject: "hello".into(),
                meaning: "a friendly greeting".into(),
                means: true
            }
        );
        assert_eq!(
            read("it means a greeting"),
            Intent::State {
                subject: "it".into(),
                meaning: "a greeting".into(),
                means: true
            }
        );
        assert_eq!(read("how is it going"), Intent::Talk);
        assert_eq!(read("you are great"), Intent::Talk);
        assert_eq!(read("is it raining?"), Intent::Talk);
    }

    #[test]
    fn cleans_and_phrases_meanings() {
        assert_eq!(clean_meaning("hello", "it means a greeting"), "a greeting");
        assert_eq!(clean_meaning("hello", "Hello is a greeting"), "a greeting");
        assert_eq!(clean_meaning("hello", "a greeting"), "a greeting");
        assert_eq!(phrase("hello", "a greeting"), "“hello” means a greeting.");
        assert_eq!(
            phrase("hello", "hello is a greeting."),
            "Hello is a greeting."
        );
        assert_eq!(key("The Black Hole"), "black hole");
    }
}
