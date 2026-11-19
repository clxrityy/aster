//! Packs: plain-text bundles of meanings, replies, and synonyms that give a
//! mind a starting vocabulary. The built-in starter pack loads into new minds
//! (and once into existing ones); more can be written by hand and loaded with
//! `/ingest something.aster`.
//!
//! ```text
//! # comments start with #
//! [meanings]
//! nebula: a cloud of gas and dust in space
//! [replies]
//! hello => Hi! Good to see you.
//! [synonyms]
//! hello, hi, hey, good morning
//! ```

/// The built-in starter pack. Bump the id when its content changes so
/// existing minds pick up the new version (meanings and replies the owner has
/// taught are never overridden).
pub const STARTER_ID: &str = "starter@1";
pub const STARTER: &str = include_str!("../packs/starter.aster");

#[derive(Debug, Default, PartialEq)]
pub struct Pack {
    pub meanings: Vec<(String, String)>,
    pub replies: Vec<(String, String)>,
    pub synonyms: Vec<Vec<String>>,
}

#[derive(Clone, Copy)]
enum Section {
    None,
    Meanings,
    Replies,
    Synonyms,
}

impl Pack {
    /// Lenient: lines that don't fit their section are skipped.
    pub fn parse(text: &str) -> Self {
        let mut pack = Pack::default();
        let mut section = Section::None;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = match name.trim().to_ascii_lowercase().as_str() {
                    "meanings" => Section::Meanings,
                    "replies" => Section::Replies,
                    "synonyms" => Section::Synonyms,
                    _ => Section::None,
                };
                continue;
            }
            match section {
                Section::Meanings => {
                    if let Some((subject, meaning)) = line.split_once(':')
                        && !subject.trim().is_empty()
                        && !meaning.trim().is_empty()
                    {
                        pack.meanings
                            .push((subject.trim().into(), meaning.trim().into()));
                    }
                }
                Section::Replies => {
                    if let Some((prompt, reply)) = line.split_once("=>")
                        && !prompt.trim().is_empty()
                        && !reply.trim().is_empty()
                    {
                        pack.replies
                            .push((prompt.trim().into(), reply.trim().into()));
                    }
                }
                Section::Synonyms => {
                    let group: Vec<String> = line
                        .split(',')
                        .map(str::trim)
                        .filter(|m| !m.is_empty())
                        .map(String::from)
                        .collect();
                    if group.len() > 1 {
                        pack.synonyms.push(group);
                    }
                }
                Section::None => {}
            }
        }
        pack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_skips_noise() {
        let pack = Pack::parse(
            "# a pack\n[meanings]\nnebula: a cloud: of gas\nbroken line\n\
             [replies]\nhello => Hi!\nno arrow\n[synonyms]\nhello, hi , hey\nalone\n[other]\nx: y",
        );
        assert_eq!(pack.meanings, [("nebula".into(), "a cloud: of gas".into())]);
        assert_eq!(pack.replies, [("hello".into(), "Hi!".into())]);
        assert_eq!(pack.synonyms, [vec!["hello", "hi", "hey"]]);
    }

    #[test]
    fn starter_pack_is_well_formed() {
        let pack = Pack::parse(STARTER);
        assert!(pack.meanings.len() >= 150, "{}", pack.meanings.len());
        assert!(pack.replies.len() >= 80, "{}", pack.replies.len());
        assert!(pack.synonyms.len() >= 30, "{}", pack.synonyms.len());
    }
}
