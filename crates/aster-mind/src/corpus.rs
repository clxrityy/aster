//! Export everything the mind has learned from as plain text, one passage per
//! line block. This is the training corpus for the experimental neural model.

use crate::mind::StatisticalMind;

pub fn export(mind: &StatisticalMind) -> String {
    let mut out = String::new();
    for association in mind.associations() {
        out.push_str(&association.prompt);
        out.push('\n');
        out.push_str(&association.reply);
        out.push_str("\n\n");
    }
    for (subject, meaning) in mind.lexicon() {
        out.push_str(&crate::understand::phrase(subject, meaning));
        out.push_str("\n\n");
    }
    for doc in mind.recall_index().docs() {
        out.push_str(doc.text.trim());
        out.push_str("\n\n");
    }
    out
}

/// Most bytes the neural engine trains on; replies come first, so recall text is
/// what gets cut.
pub const MAX_TRAINING_BYTES: usize = 2 * 1024 * 1024;

/// The neural engine's training text: one `prompt<TAB>reply<LF>` line per
/// reply Aster has learned (stronger ones repeated, up to 4×), the meanings as
/// "what is …" pairs, then recall passages as plain lines.
pub fn training_text(mind: &StatisticalMind) -> Vec<u8> {
    let flat = |text: &str| text.replace(['\t', '\n', '\r'], " ").trim().to_string();
    let mut out = Vec::new();
    let push = |out: &mut Vec<u8>, line: String| {
        if out.len() + line.len() < MAX_TRAINING_BYTES {
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
    };
    for association in mind.associations() {
        if association.weight <= 0.0 {
            continue;
        }
        let line = format!(
            "{}\t{}",
            flat(&association.prompt),
            flat(&association.reply)
        );
        let repeats = (association.weight * 2.0).round().clamp(1.0, 4.0) as usize;
        for _ in 0..repeats {
            push(&mut out, line.clone());
        }
    }
    for (subject, meaning) in mind.lexicon() {
        let phrase = crate::understand::phrase(subject, meaning);
        push(
            &mut out,
            format!("what is {}\t{}", flat(subject), flat(&phrase)),
        );
    }
    for doc in mind.recall_index().docs() {
        for line in doc.text.lines().map(flat).filter(|l| !l.is_empty()) {
            push(&mut out, line);
        }
    }
    out
}
