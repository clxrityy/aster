//! Tokenization and rendering. Words are lowercased; `. , ? !` are kept as
//! separate tokens so the language model can learn sentence shape.

const PUNCTUATION: [char; 4] = ['.', ',', '?', '!'];

const STOPWORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "all", "am", "an", "and", "any", "are", "as", "at",
    "be", "because", "been", "before", "being", "but", "by", "can", "could", "did", "do", "does",
    "doing", "for", "from", "had", "has", "have", "having", "he", "her", "here", "hers", "him",
    "his", "how", "i", "i'm", "if", "in", "into", "is", "it", "it's", "its", "just", "me", "more",
    "most", "my", "no", "not", "now", "of", "on", "once", "only", "or", "other", "our", "out",
    "over", "own", "so", "some", "such", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "this", "those", "through", "to", "too", "under", "until", "up", "very",
    "was", "we", "were", "what", "when", "where", "which", "while", "who", "whom", "why", "will",
    "with", "would", "you", "your", "yours", "yes", "ok", "okay",
];

pub fn is_stopword(word: &str) -> bool {
    STOPWORDS.contains(&word)
}

pub fn is_punctuation(token: &str) -> bool {
    token.len() == 1 && token.chars().all(|c| PUNCTUATION.contains(&c))
}

pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || (c == '\'' && !current.is_empty()) {
            current.extend(c.to_lowercase());
        } else {
            if !current.is_empty() {
                out.push(
                    std::mem::take(&mut current)
                        .trim_end_matches('\'')
                        .to_string(),
                );
            }
            if PUNCTUATION.contains(&c) {
                out.push(c.to_string());
            }
        }
    }
    if !current.is_empty() {
        out.push(current.trim_end_matches('\'').to_string());
    }
    out.retain(|token| !token.is_empty());
    out
}

pub fn words(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .filter(|token| !is_punctuation(token))
        .cloned()
        .collect()
}

/// Meaningful terms: non-stopwords containing a letter, deduplicated in order.
pub fn content_terms(tokens: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in tokens {
        if is_punctuation(token)
            || is_stopword(token)
            || token.chars().count() < 2
            || !token.chars().any(char::is_alphabetic)
        {
            continue;
        }
        if !out.contains(token) {
            out.push(token.clone());
        }
    }
    out
}

pub fn render(tokens: &[String]) -> String {
    let mut out = String::new();
    for token in tokens {
        if !out.is_empty() && !is_punctuation(token) {
            out.push(' ');
        }
        out.push_str(token);
    }
    let mut chars = out.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => out,
    }
}

pub fn normalize(text: &str) -> String {
    words(&tokens(text)).join(" ")
}

pub fn excerpt(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_words_and_punctuation() {
        assert_eq!(
            tokens("Hello, Aster! What's up?"),
            ["hello", ",", "aster", "!", "what's", "up", "?"]
        );
        assert_eq!(
            content_terms(&tokens("What is a nebula, the nebula?")),
            ["nebula"]
        );
    }

    #[test]
    fn renders_readably() {
        let t = tokens("hello , there . how are you ?");
        assert_eq!(render(&t), "Hello, there. how are you?");
    }
}
