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
    for doc in mind.recall_index().docs() {
        out.push_str(doc.text.trim());
        out.push_str("\n\n");
    }
    out
}
