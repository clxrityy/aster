//! The statistical mind: a lexicon of meanings, reply associations reinforced
//! by owner feedback, BM25 recall, and an n-gram language model. Each message is
//! first read for intent (asking a meaning, stating one, or talking), so what a
//! word *means* and what to *say* are learned and used separately. Every reply
//! is chosen by explicit scores recorded in the trace; when nothing scores high
//! enough, Aster asks.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use aster_core::{
    Action, Candidate, CandidateSource, Cognition, CognitionParams, Decision, LearnOutcome,
    LearningSignal, OutboundDraft, OutboundKind, Recalled, RegulationEvent, RegulatoryState,
    RequestEffect, Response, SnapshotError, ThoughtTrace, TurnContext,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ngram::NgramModel,
    recall::RecallIndex,
    rng::Rng,
    text::{content_terms, excerpt, is_stopword, normalize, render, tokens, words},
    understand::{self, Intent},
};

const ENGINE: &str = "statistical";
const FORMAT_VERSION: u32 = 2;
const RECENT_TURNS: usize = 128;
const CURIOSITY_QUEUE: usize = 32;
const MAX_WEIGHT: f64 = 3.0;
const MIN_SIMILARITY: f64 = 0.2;
const MIN_GENERATION_TOKENS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationSource {
    Taught,
    Corrected,
    Answered,
    Reinforced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Association {
    pub prompt: String,
    pub words: Vec<String>,
    pub reply: String,
    pub weight: f64,
    pub uses: u32,
    pub source: AssociationSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Chosen {
    Association(usize),
    Text(String),
    Question,
    /// Said or learned the meaning of this lexicon key.
    Definition(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecentTurn {
    turn_id: Uuid,
    prompt: String,
    chosen: Chosen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatisticalMind {
    format_version: u32,
    vocab: HashMap<String, u32>,
    ngram: NgramModel,
    recall: RecallIndex,
    associations: Vec<Association>,
    /// What things mean: lexicon key → meaning. Used only to answer questions
    /// about meaning, never as a reply to the word itself.
    #[serde(default)]
    lexicon: BTreeMap<String, String>,
    /// The subject of a meaning question Aster asked on the previous turn, so
    /// "it means …" can be understood.
    #[serde(default)]
    last_asked: Option<String>,
    recent: VecDeque<RecentTurn>,
    curious_about: VecDeque<String>,
    asked: HashSet<String>,
    /// Exponential moving average of owner ratings in -1..=1.
    reward_ema: f64,
    /// Self-regulated within the owner's exploration bounds (as a 0..=1 position).
    exploration: f64,
    /// Self-regulated within the owner's answer-confidence bounds (0..=1 position).
    caution: f64,
    updates: u64,
    asked_for_first_lesson: bool,
    #[serde(skip)]
    rng: Rng,
}

impl StatisticalMind {
    pub fn new(params: &CognitionParams) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            vocab: HashMap::new(),
            ngram: NgramModel::default(),
            recall: RecallIndex::default(),
            associations: Vec::new(),
            lexicon: BTreeMap::new(),
            last_asked: None,
            recent: VecDeque::new(),
            curious_about: VecDeque::new(),
            asked: HashSet::new(),
            reward_ema: 0.0,
            exploration: 0.5,
            caution: 0.5,
            updates: 0,
            asked_for_first_lesson: false,
            rng: Rng::new(params.rng_seed),
        }
    }

    pub fn restore(bytes: &[u8], params: &CognitionParams) -> Result<Self, SnapshotError> {
        let mut mind: Self = serde_json::from_slice(bytes).map_err(|_| SnapshotError)?;
        match mind.format_version {
            FORMAT_VERSION => {}
            1 => mind.migrate_v1(),
            _ => return Err(SnapshotError),
        }
        mind.rng = Rng::new(params.rng_seed);
        Ok(mind)
    }

    pub fn associations(&self) -> &[Association] {
        &self.associations
    }

    pub fn recall_index(&self) -> &RecallIndex {
        &self.recall
    }

    pub fn lexicon(&self) -> &BTreeMap<String, String> {
        &self.lexicon
    }

    /// Version 1 stored answered meanings as replies to "what is X" and as
    /// recall documents, so the meaning came back whenever the word did. Move
    /// them into the lexicon.
    fn migrate_v1(&mut self) {
        let mut remap = Vec::with_capacity(self.associations.len());
        let mut kept = Vec::new();
        for association in std::mem::take(&mut self.associations) {
            let prompt = normalize(&association.prompt);
            match prompt.strip_prefix("what is ") {
                Some(subject) if association.source == AssociationSource::Answered => {
                    let subject = understand::key(subject);
                    self.lexicon.insert(subject.clone(), association.reply);
                    remap.push(Err(subject));
                }
                _ => {
                    remap.push(Ok(kept.len()));
                    kept.push(association);
                }
            }
        }
        self.associations = kept;
        for turn in &mut self.recent {
            if let Chosen::Association(i) = turn.chosen {
                turn.chosen = match remap.get(i) {
                    Some(Ok(new)) => Chosen::Association(*new),
                    Some(Err(subject)) => Chosen::Definition(subject.clone()),
                    None => Chosen::Question,
                };
            }
        }
        let stale: Vec<String> = self
            .recall
            .docs()
            .iter()
            .filter(|doc| {
                doc.id
                    .strip_prefix("answer:")
                    .is_some_and(|rest| !rest.chars().all(|c| c.is_ascii_digit()))
            })
            .map(|doc| doc.id.clone())
            .collect();
        for id in stale {
            self.recall.remove(&id);
        }
        self.format_version = FORMAT_VERSION;
    }

    /// Store a meaning; returns a note for the trace.
    fn define(&mut self, subject: &str, meaning: &str) -> String {
        let key = understand::key(subject);
        let meaning = understand::clean_meaning(&key, meaning);
        for word in key.split(' ') {
            *self.vocab.entry(word.to_string()).or_default() += 1;
        }
        self.asked.insert(key.clone());
        self.curious_about.retain(|t| *t != key);
        let note = if self.lexicon.contains_key(&key) {
            format!("updated what “{key}” means")
        } else {
            format!("learned what “{key}” means")
        };
        self.lexicon.insert(key, meaning);
        note
    }

    /// Find a meaning for a subject: exact, singular, then its last word.
    fn lookup(&self, subject: &str) -> Option<(String, String)> {
        let key = understand::key(subject);
        let mut tries = vec![key.clone()];
        if let Some(singular) = key.strip_suffix('s')
            && singular.len() > 2
        {
            tries.push(singular.to_string());
        }
        if let Some(last) = key.rsplit(' ').next()
            && last != key
        {
            tries.push(last.to_string());
        }
        tries.into_iter().find_map(|k| {
            let meaning = self.lexicon.get(&k)?.clone();
            Some((k, meaning))
        })
    }

    /// A statement of meaning is accepted when it says "means", or when it is
    /// about something Aster asked about. "It"/"that" refer to the last question.
    fn accept_statement(&self, subject: &str, means: bool) -> Option<String> {
        if understand::is_pronoun(subject) {
            return self.last_asked.clone();
        }
        let asked_about = self.last_asked.as_deref() == Some(subject)
            || self.asked.contains(subject)
            || self.curious_about.iter().any(|t| t == subject);
        (means || asked_about).then(|| subject.to_string())
    }

    fn observe(&mut self, text: &str, order: usize) {
        let toks = tokens(text);
        for word in words(&toks) {
            *self.vocab.entry(word).or_default() += 1;
        }
        // Train per line so sentence boundaries stay meaningful.
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            self.ngram.train(&tokens(line), order);
        }
    }

    fn similarity(a: &[String], b: &[String]) -> f64 {
        let weight = |w: &String| if is_stopword(w) { 0.25 } else { 1.0 };
        let set_a: HashSet<&String> = a.iter().collect();
        let set_b: HashSet<&String> = b.iter().collect();
        let mass_a: f64 = set_a.iter().map(|w| weight(w)).sum();
        let mass_b: f64 = set_b.iter().map(|w| weight(w)).sum();
        if mass_a == 0.0 || mass_b == 0.0 {
            return 0.0;
        }
        let shared: f64 = set_a.intersection(&set_b).map(|w| weight(w)).sum();
        shared / (mass_a * mass_b).sqrt()
    }

    fn add_association(
        &mut self,
        prompt: &str,
        reply: &str,
        source: AssociationSource,
        weight: f64,
        learning_rate: f64,
    ) -> String {
        let normalized_prompt = normalize(prompt);
        let normalized_reply = normalize(reply);
        if let Some(existing) = self.associations.iter_mut().find(|a| {
            normalize(&a.prompt) == normalized_prompt && normalize(&a.reply) == normalized_reply
        }) {
            existing.weight = (existing.weight + learning_rate).min(MAX_WEIGHT);
            return format!("reinforced association (weight {:.2})", existing.weight);
        }
        self.associations.push(Association {
            prompt: prompt.trim().into(),
            words: words(&tokens(prompt)),
            reply: reply.trim().into(),
            weight,
            uses: 0,
            source,
        });
        format!("new association #{}", self.associations.len() - 1)
    }

    fn remember_turn(&mut self, turn_id: Uuid, prompt: &str, chosen: Chosen) {
        self.recent.push_back(RecentTurn {
            turn_id,
            prompt: prompt.into(),
            chosen,
        });
        while self.recent.len() > RECENT_TURNS {
            self.recent.pop_front();
        }
    }

    fn queue_curiosity(&mut self, terms: &[String]) {
        for term in terms {
            if !self.curious_about.contains(term) && !self.asked.contains(term) {
                self.curious_about.push_back(term.clone());
            }
        }
        while self.curious_about.len() > CURIOSITY_QUEUE {
            self.curious_about.pop_front();
        }
    }

    fn self_regulate(&mut self, rating: i8) {
        self.reward_ema = 0.8 * self.reward_ema + 0.2 * f64::from(rating.signum());
        // Poor feedback → explore more; good feedback → exploit what works.
        self.exploration = ((1.0 - self.reward_ema) / 2.0).clamp(0.0, 1.0);
        let step = if rating < 0 { 0.05 } else { -0.03 };
        self.caution = (self.caution + step).clamp(0.0, 1.0);
    }

    fn lerp(min: f64, max: f64, position: f64) -> f64 {
        min + (max - min) * position.clamp(0.0, 1.0)
    }
}

impl Cognition for StatisticalMind {
    fn engine(&self) -> &'static str {
        ENGINE
    }

    fn respond(&mut self, context: &TurnContext<'_>) -> Response {
        let params = context.params;
        let levels = context.levels;
        let toks = tokens(context.text);
        let prompt_words = words(&toks);
        let terms = content_terms(&toks);
        let unknown: Vec<String> = terms
            .iter()
            .filter(|term| !self.vocab.contains_key(*term))
            .cloned()
            .collect();
        let mut notes = Vec::new();
        let level = |value: aster_core::Level| value.get() / 100.0;

        // Read intent first: meanings are answered from the lexicon, statements
        // of meaning are learned, everything else goes to the reply candidates.
        let intent = understand::read(context.text);
        let mut asked_subject = None;
        let mut events_extra = Vec::new();
        let direct: Option<(Candidate, Chosen)> = match &intent {
            Intent::State {
                subject,
                meaning,
                means,
            } => self.accept_statement(subject, *means).map(|subject| {
                let resolved = self.last_asked.as_deref() == Some(subject.as_str());
                notes.push(self.define(&subject, meaning));
                events_extra.push(if resolved {
                    RegulationEvent::QuestionAnswered
                } else {
                    RegulationEvent::Taught
                });
                let stored = self.lexicon[&understand::key(&subject)].clone();
                (
                    Candidate {
                        text: format!("Got it. {}", understand::phrase(&subject, &stored)),
                        source: CandidateSource::Definition,
                        score: 1.0,
                        components: BTreeMap::new(),
                    },
                    Chosen::Definition(understand::key(&subject)),
                )
            }),
            Intent::AskMeaning { subject } => {
                asked_subject = Some(understand::key(subject));
                self.lookup(subject).map(|(key, meaning)| {
                    notes.push(format!("asked what “{subject}” means; found “{key}”"));
                    (
                        Candidate {
                            text: understand::phrase(&key, &meaning),
                            source: CandidateSource::Definition,
                            score: 0.9,
                            components: BTreeMap::new(),
                        },
                        Chosen::Definition(key),
                    )
                })
            }
            Intent::Talk => None,
        };
        self.last_asked = None;

        // Candidates from associations.
        let mut scored: Vec<(Candidate, Chosen)> = Vec::new();
        for (index, association) in self.associations.iter().enumerate() {
            let similarity = Self::similarity(&prompt_words, &association.words);
            if similarity < MIN_SIMILARITY {
                continue;
            }
            let weight_factor = association.weight / (association.weight + 0.5);
            let score = similarity * weight_factor;
            scored.push((
                Candidate {
                    text: association.reply.clone(),
                    source: CandidateSource::Association,
                    score,
                    components: BTreeMap::from([
                        ("similarity".into(), similarity),
                        ("weight".into(), association.weight),
                    ]),
                },
                Chosen::Association(index),
            ));
        }

        // Candidates from recall.
        let query = if terms.is_empty() {
            prompt_words.clone()
        } else {
            terms.clone()
        };
        let mut recalled = Vec::new();
        for (doc, bm25) in self.recall.search(&query, params.recall_top_k) {
            let text = excerpt(&doc.text, 280);
            recalled.push(Recalled {
                source: doc.id.clone(),
                score: bm25,
                excerpt: excerpt(&doc.text, 120),
            });
            let score = bm25 / (bm25 + 3.0) * 0.7;
            scored.push((
                Candidate {
                    text: text.clone(),
                    source: CandidateSource::Recall,
                    score,
                    components: BTreeMap::from([("bm25".into(), bm25)]),
                },
                Chosen::Text(text),
            ));
        }

        // Shorter replies under high cognitive load.
        let max_tokens = ((params.max_reply_tokens as f64)
            * (1.0 - level(levels.cognitive_load) * 0.5))
            .round()
            .max(4.0) as usize;
        let exploration = Self::lerp(
            params.exploration_min,
            params.exploration_max,
            self.exploration,
        );

        // A generated candidate when the language model has enough data.
        if self.ngram.tokens_seen >= MIN_GENERATION_TOKENS {
            let seed = terms
                .iter()
                .filter(|term| self.ngram.knows(term))
                .max_by(|a, b| self.recall.idf(a).total_cmp(&self.recall.idf(b)))
                .map(|term| vec![term.clone()])
                .unwrap_or_default();
            let temperature = 0.5 + exploration;
            let generated = self.ngram.generate(
                &seed,
                max_tokens,
                params.ngram_order,
                temperature,
                &mut self.rng,
            );
            if generated.len() > seed.len() + 1 {
                let fluency = self.ngram.fluency(&generated, params.ngram_order);
                let score = fluency.powf(0.25) * 0.35;
                let text = render(&generated);
                scored.push((
                    Candidate {
                        text: text.clone(),
                        source: CandidateSource::Generated,
                        score,
                        components: BTreeMap::from([
                            ("fluency".into(), fluency),
                            ("temperature".into(), temperature),
                        ]),
                    },
                    Chosen::Text(text),
                ));
            }
        }

        scored.sort_by(|a, b| b.0.score.total_cmp(&a.0.score));
        scored.truncate(8);
        let has_direct = direct.is_some();
        if let Some(direct) = direct {
            scored.insert(0, direct);
        }

        // Level-adjusted threshold within the owner's bounds.
        let base = Self::lerp(
            params.answer_confidence_min,
            params.answer_confidence_max,
            self.caution,
        );
        let threshold = (base + 0.1 * (level(levels.stress) - 0.1)
            - 0.1 * (level(levels.confidence) - 0.5)
            + 0.05 * (level(levels.curiosity) - 0.6))
            .clamp(params.answer_confidence_min, params.answer_confidence_max);

        let mut chosen_index = None;
        let mut explored = false;
        if has_direct {
            chosen_index = Some(0);
        } else if let Some((best, _)) = scored.first()
            && best.score >= threshold
        {
            chosen_index = Some(0);
            if scored.len() > 1
                && scored[1].0.score >= threshold
                && self.rng.next_f64() < exploration
            {
                chosen_index = Some(1);
                explored = true;
                notes.push("explored the second-best candidate".into());
            }
        }
        let confidence = scored.first().map_or(0.0, |(c, _)| c.score);

        let mut events = vec![RegulationEvent::MessageReceived, RegulationEvent::ReplySent];
        if !unknown.is_empty() {
            events.push(RegulationEvent::UnknownConcept);
        }
        events.extend(events_extra);

        let (text, decision, question, chosen) = match chosen_index {
            Some(index) => {
                self.queue_curiosity(&unknown);
                let (candidate, chosen) = scored[index].clone();
                if let Chosen::Association(i) = chosen {
                    self.associations[i].uses += 1;
                }
                (candidate.text, Decision::Reply, None, chosen)
            }
            None => {
                notes.push(format!(
                    "best score {confidence:.2} below threshold {threshold:.2}; asking instead"
                ));
                // Asked what something means → ask for that meaning. Otherwise
                // ask about the most specific (longest) unknown term.
                let subject = asked_subject
                    .clone()
                    .or_else(|| unknown.iter().max_by_key(|t| t.chars().count()).cloned());
                let mut draft = if let Some(term) = subject {
                    let rationale = if asked_subject.is_some() {
                        format!("no meaning learned for “{term}”")
                    } else {
                        format!("“{term}” is not in my vocabulary")
                    };
                    let mut draft = OutboundDraft::question(
                        format!("I don't know what “{term}” means yet. What does it mean?"),
                        rationale,
                    );
                    self.last_asked = Some(term.clone());
                    self.asked.insert(term.clone());
                    self.curious_about.retain(|t| *t != term);
                    let rest: Vec<String> =
                        unknown.iter().filter(|t| **t != term).cloned().collect();
                    self.queue_curiosity(&rest);
                    draft.subject = Some(term);
                    draft
                } else {
                    OutboundDraft::question(
                        format!(
                            "I'm not sure how to answer that yet. How would you reply to “{}”?",
                            excerpt(context.text, 200)
                        ),
                        "no learned reply scored above my confidence threshold",
                    )
                };
                draft.prompt = Some(context.text.trim().to_string());
                let body = draft.body.clone();
                scored.push((
                    Candidate {
                        text: body.clone(),
                        source: CandidateSource::Question,
                        score: threshold,
                        components: BTreeMap::new(),
                    },
                    Chosen::Question,
                ));
                chosen_index = Some(scored.len() - 1);
                (body, Decision::Ask, Some(draft), Chosen::Question)
            }
        };

        self.remember_turn(context.turn_id, context.text, chosen);
        // The owner's own words are training data for vocabulary and language.
        self.observe(context.text, params.ngram_order);
        self.updates += 1;

        let trace = ThoughtTrace {
            engine: ENGINE.into(),
            input_terms: terms,
            unknown_terms: unknown,
            recalled,
            candidates: scored.into_iter().map(|(c, _)| c).collect(),
            chosen: chosen_index,
            decision,
            confidence,
            threshold,
            exploration,
            explored,
            levels: levels.clone(),
            notes,
        };
        Response {
            text,
            trace,
            question,
            events,
        }
    }

    fn learn(&mut self, signal: LearningSignal, params: &CognitionParams) -> LearnOutcome {
        let lr = params.learning_rate;
        let order = params.ngram_order;
        self.updates += 1;
        match signal {
            LearningSignal::Teach { prompt, reply } => {
                self.observe(&prompt, order);
                self.observe(&reply, order);
                let note =
                    self.add_association(&prompt, &reply, AssociationSource::Taught, 1.0, lr);
                LearnOutcome {
                    events: vec![RegulationEvent::Taught],
                    note,
                    updated: true,
                }
            }
            LearningSignal::Feedback {
                turn_id,
                rating,
                correction,
            } => {
                self.self_regulate(rating);
                let event = if rating > 0 {
                    RegulationEvent::PositiveFeedback
                } else {
                    RegulationEvent::NegativeFeedback
                };
                let Some(turn) = self.recent.iter().find(|t| t.turn_id == turn_id).cloned() else {
                    return LearnOutcome {
                        events: vec![event],
                        note: "turn is no longer in recent memory; only self-regulation adjusted"
                            .into(),
                        updated: true,
                    };
                };
                let mut notes = Vec::new();
                if let Chosen::Definition(key) = &turn.chosen {
                    // Feedback on a meaning corrects the lexicon, not replies.
                    let key = key.clone();
                    match correction.filter(|c| !c.trim().is_empty()) {
                        Some(correction) => {
                            self.observe(&correction, order);
                            notes.push(self.define(&key, &correction));
                        }
                        None if rating < 0 => {
                            self.lexicon.remove(&key);
                            notes.push(format!("forgot what “{key}” means"));
                        }
                        None => {}
                    }
                    notes.push(format!(
                        "reward_ema {:.2}, exploration {:.2}, caution {:.2}",
                        self.reward_ema, self.exploration, self.caution
                    ));
                    return LearnOutcome {
                        events: vec![event],
                        note: notes.join("; "),
                        updated: true,
                    };
                }
                match (&turn.chosen, rating > 0) {
                    (Chosen::Association(i), true) => {
                        if let Some(a) = self.associations.get_mut(*i) {
                            a.weight = (a.weight + lr).min(MAX_WEIGHT);
                            notes.push(format!("association #{i} weight → {:.2}", a.weight));
                        }
                    }
                    (Chosen::Association(i), false) => {
                        if let Some(a) = self.associations.get_mut(*i) {
                            a.weight = (a.weight - 2.0 * lr).max(0.0);
                            notes.push(format!("association #{i} weight → {:.2}", a.weight));
                        }
                    }
                    (Chosen::Text(text), true) => {
                        let text = text.clone();
                        notes.push(self.add_association(
                            &turn.prompt,
                            &text,
                            AssociationSource::Reinforced,
                            1.0,
                            lr,
                        ));
                    }
                    _ => {}
                }
                if let Some(correction) = correction.filter(|c| !c.trim().is_empty()) {
                    self.observe(&correction, order);
                    notes.push(self.add_association(
                        &turn.prompt,
                        &correction,
                        AssociationSource::Corrected,
                        1.5,
                        lr,
                    ));
                }
                notes.push(format!(
                    "reward_ema {:.2}, exploration {:.2}, caution {:.2}",
                    self.reward_ema, self.exploration, self.caution
                ));
                LearnOutcome {
                    events: vec![event],
                    note: notes.join("; "),
                    updated: true,
                }
            }
            LearningSignal::Answer {
                subject,
                prompt,
                answer,
            } => {
                self.observe(&answer, order);
                let mut notes = Vec::new();
                if let Some(subject) = subject {
                    // An answer to "what does X mean?" is a meaning, not a reply.
                    if self.last_asked.as_deref() == Some(understand::key(&subject).as_str()) {
                        self.last_asked = None;
                    }
                    notes.push(self.define(&subject, &answer));
                } else if let Some(prompt) = prompt {
                    notes.push(self.add_association(
                        &prompt,
                        &answer,
                        AssociationSource::Answered,
                        1.0,
                        lr,
                    ));
                } else {
                    let id = format!("answer:{}", self.updates);
                    self.recall.add(&id, "answer", &answer);
                    notes.push("stored answer for recall".into());
                }
                LearnOutcome {
                    events: vec![RegulationEvent::QuestionAnswered],
                    note: notes.join("; "),
                    updated: true,
                }
            }
            LearningSignal::Memory { id, content } => {
                self.observe(&content, order);
                self.recall.add(&id, "memory", &content);
                LearnOutcome {
                    events: vec![],
                    note: format!("indexed memory {id}"),
                    updated: true,
                }
            }
            LearningSignal::Forget { id } => {
                let removed = self.recall.remove(&id);
                LearnOutcome {
                    events: vec![],
                    note: if removed {
                        format!("forgot {id}")
                    } else {
                        format!("{id} was not indexed")
                    },
                    updated: removed,
                }
            }
            LearningSignal::Ingest { name, text } => {
                self.observe(&text, order);
                let mut passages = 0;
                for (i, passage) in text
                    .split("\n\n")
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .enumerate()
                {
                    self.recall
                        .add(&format!("ingest:{name}#{i}"), "ingest", passage);
                    passages += 1;
                }
                LearnOutcome {
                    events: vec![RegulationEvent::Ingested],
                    note: format!("ingested {name}: {passages} passages"),
                    updated: true,
                }
            }
        }
    }

    fn initiative(
        &mut self,
        levels: &RegulatoryState,
        params: &CognitionParams,
    ) -> Option<OutboundDraft> {
        if levels.energy.get() < params.rest_energy_below {
            return Some(OutboundDraft {
                kind: OutboundKind::Request,
                body: "My energy is low. May I rest for a while?".into(),
                rationale: format!(
                    "energy {:.0} is below the rest threshold {:.0}",
                    levels.energy.get(),
                    params.rest_energy_below
                ),
                subject: None,
                prompt: None,
                action: Some(Action::RegulateState),
                effect: Some(RequestEffect::Rest),
            });
        }
        if levels.curiosity.get() < params.initiative_curiosity_min {
            return None;
        }
        if let Some(term) = self.curious_about.pop_front() {
            self.asked.insert(term.clone());
            self.last_asked = Some(term.clone());
            let mut draft = OutboundDraft::question(
                format!("Earlier you mentioned “{term}”. What does it mean?"),
                format!(
                    "curiosity {:.0} and “{term}” is still unknown to me",
                    levels.curiosity.get()
                ),
            );
            draft.subject = Some(term);
            return Some(draft);
        }
        if self.associations.is_empty() && !self.asked_for_first_lesson {
            self.asked_for_first_lesson = true;
            return Some(OutboundDraft::question(
                "I haven't learned any replies yet. What would you like to teach me first?",
                "no associations learned",
            ));
        }
        None
    }

    fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        serde_json::to_vec(self).map_err(|_| SnapshotError)
    }

    fn corpus(&self) -> String {
        crate::corpus::export(self)
    }

    fn stats(&self) -> BTreeMap<String, f64> {
        BTreeMap::from([
            ("vocabulary".into(), self.vocab.len() as f64),
            ("ngram_tokens".into(), self.ngram.tokens_seen as f64),
            ("associations".into(), self.associations.len() as f64),
            ("meanings".into(), self.lexicon.len() as f64),
            ("recall_documents".into(), self.recall.len() as f64),
            ("curiosity_queue".into(), self.curious_about.len() as f64),
            ("reward_ema".into(), self.reward_ema),
            ("exploration_position".into(), self.exploration),
            ("caution_position".into(), self.caution),
            ("updates".into(), self.updates as f64),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> CognitionParams {
        CognitionParams {
            rng_seed: Some(42),
            ..Default::default()
        }
    }

    fn ask(mind: &mut StatisticalMind, text: &str) -> (Uuid, Response) {
        let id = Uuid::new_v4();
        let levels = RegulatoryState::default();
        let params = params();
        let response = mind.respond(&TurnContext {
            turn_id: id,
            text,
            levels: &levels,
            params: &params,
        });
        (id, response)
    }

    #[test]
    fn untrained_mind_asks_about_unknown_terms() {
        let mut mind = StatisticalMind::new(&params());
        let (_, response) = ask(&mut mind, "Tell me about nebulae");
        assert_eq!(response.trace.decision, Decision::Ask);
        let question = response.question.unwrap();
        assert_eq!(question.subject.as_deref(), Some("nebulae"));
        assert!(
            response
                .trace
                .unknown_terms
                .contains(&"nebulae".to_string())
        );
    }

    #[test]
    fn taught_pair_is_returned_for_same_prompt() {
        let mut mind = StatisticalMind::new(&params());
        mind.learn(
            LearningSignal::Teach {
                prompt: "hello".into(),
                reply: "Hi! I'm Aster.".into(),
            },
            &params(),
        );
        let (_, response) = ask(&mut mind, "Hello!");
        assert_eq!(response.trace.decision, Decision::Reply);
        assert_eq!(response.text, "Hi! I'm Aster.");
    }

    #[test]
    fn negative_feedback_demotes_and_correction_replaces() {
        let mut mind = StatisticalMind::new(&params());
        mind.learn(
            LearningSignal::Teach {
                prompt: "how are you".into(),
                reply: "bad reply".into(),
            },
            &params(),
        );
        let (id, response) = ask(&mut mind, "how are you");
        assert_eq!(response.text, "bad reply");
        mind.learn(
            LearningSignal::Feedback {
                turn_id: id,
                rating: -1,
                correction: Some("I'm steady, thanks.".into()),
            },
            &params(),
        );
        assert!(mind.associations()[0].weight < 1.0);
        let (_, response) = ask(&mut mind, "how are you");
        assert_eq!(response.text, "I'm steady, thanks.");
    }

    #[test]
    fn answers_teach_subjects_and_enable_recall() {
        let mut mind = StatisticalMind::new(&params());
        mind.learn(
            LearningSignal::Answer {
                subject: Some("nebula".into()),
                prompt: None,
                answer: "a cloud of gas and dust in space".into(),
            },
            &params(),
        );
        let (_, response) = ask(&mut mind, "what is nebula");
        assert_eq!(response.trace.decision, Decision::Reply);
        assert!(response.text.contains("cloud of gas"));
    }

    #[test]
    fn documented_training_loop_works() {
        let mut mind = StatisticalMind::new(&params());
        let (_, first) = ask(&mut mind, "hello");
        let question = first.question.unwrap();
        assert!(first.text.contains("hello"));
        mind.learn(
            LearningSignal::Answer {
                subject: question.subject,
                prompt: question.prompt,
                answer: "a friendly greeting".into(),
            },
            &params(),
        );
        mind.learn(
            LearningSignal::Teach {
                prompt: "hello".into(),
                reply: "Hi! Good to see you.".into(),
            },
            &params(),
        );
        let (_, reply) = ask(&mut mind, "hello");
        assert_eq!(reply.text, "Hi! Good to see you.");
    }

    #[test]
    fn known_words_without_a_reply_ask_how_to_reply() {
        let mut mind = StatisticalMind::new(&params());
        mind.learn(
            LearningSignal::Memory {
                id: "m".into(),
                content: "purple".into(),
            },
            &params(),
        );
        let (_, unsure) = ask(&mut mind, "today");
        assert_eq!(unsure.trace.decision, Decision::Ask);
        let (_, unsure) = ask(&mut mind, "today");
        assert!(unsure.text.contains("How would you reply"));
        let question = unsure.question.unwrap();
        mind.learn(
            LearningSignal::Answer {
                subject: question.subject,
                prompt: question.prompt,
                answer: "Today is a good day.".into(),
            },
            &params(),
        );
        assert_eq!(ask(&mut mind, "today").1.text, "Today is a good day.");
    }

    #[test]
    fn a_meaning_is_not_a_reply() {
        let mut mind = StatisticalMind::new(&params());
        let (_, first) = ask(&mut mind, "hello");
        let question = first.question.unwrap();
        assert_eq!(question.subject.as_deref(), Some("hello"));
        mind.learn(
            LearningSignal::Answer {
                subject: question.subject,
                prompt: question.prompt,
                answer: "it means a friendly greeting".into(),
            },
            &params(),
        );
        // Saying the word does not recite its meaning; Aster asks how to reply.
        let (_, said) = ask(&mut mind, "hello");
        assert_eq!(said.trace.decision, Decision::Ask);
        assert!(said.text.contains("How would you reply"), "{}", said.text);
        assert!(!said.text.contains("greeting"));
        // Asking what it means does.
        let (_, meaning) = ask(&mut mind, "What does hello mean?");
        assert_eq!(meaning.trace.decision, Decision::Reply);
        assert_eq!(meaning.text, "“hello” means a friendly greeting.");
    }

    #[test]
    fn meanings_can_be_stated_in_conversation() {
        let mut mind = StatisticalMind::new(&params());
        let (_, asked) = ask(&mut mind, "I saw a nebula");
        assert!(asked.text.contains("nebula"));
        let (_, ack) = ask(&mut mind, "it's a cloud of gas and dust");
        assert_eq!(ack.text, "Got it. “nebula” means a cloud of gas and dust.");
        assert_eq!(
            mind.lexicon().get("nebula").map(String::as_str),
            Some("a cloud of gas and dust")
        );
        let (_, ack) = ask(&mut mind, "A quasar means a very bright galactic core.");
        assert!(ack.text.starts_with("Got it."));
        let (_, answer) = ask(&mut mind, "what is a quasar?");
        assert!(answer.text.contains("bright galactic core"));
        // "it's …" with nothing pending is just talk.
        let (_, talk) = ask(&mut mind, "it's raining");
        assert!(!talk.text.starts_with("Got it."));
        assert!(!mind.lexicon().contains_key("it"));
    }

    #[test]
    fn feedback_on_a_meaning_corrects_the_lexicon() {
        let mut mind = StatisticalMind::new(&params());
        ask(&mut mind, "what is rust");
        mind.learn(
            LearningSignal::Answer {
                subject: Some("rust".into()),
                prompt: None,
                answer: "a kind of metal".into(),
            },
            &params(),
        );
        let (id, _) = ask(&mut mind, "what is rust");
        mind.learn(
            LearningSignal::Feedback {
                turn_id: id,
                rating: -1,
                correction: Some("a programming language".into()),
            },
            &params(),
        );
        assert!(mind.associations().is_empty());
        assert_eq!(
            ask(&mut mind, "define rust").1.text,
            "“rust” means a programming language."
        );
    }

    #[test]
    fn version_one_snapshots_move_meanings_into_the_lexicon() {
        let mut mind = StatisticalMind::new(&params());
        mind.learn(
            LearningSignal::Teach {
                prompt: "ping".into(),
                reply: "pong".into(),
            },
            &params(),
        );
        mind.associations.insert(
            0,
            Association {
                prompt: "what is hello".into(),
                words: vec!["what".into(), "is".into(), "hello".into()],
                reply: "a friendly greeting".into(),
                weight: 1.0,
                uses: 0,
                source: AssociationSource::Answered,
            },
        );
        mind.recall
            .add("answer:hello", "answer", "hello: a friendly greeting");
        mind.format_version = 1;
        let bytes = mind.snapshot().unwrap();
        let mut restored = StatisticalMind::restore(&bytes, &params()).unwrap();
        assert_eq!(restored.associations().len(), 1);
        assert_eq!(restored.recall_index().len(), 0);
        assert_eq!(ask(&mut restored, "ping").1.text, "pong");
        assert!(!ask(&mut restored, "hello").1.text.contains("greeting"));
        assert!(
            ask(&mut restored, "what is hello")
                .1
                .text
                .contains("greeting")
        );
    }

    #[test]
    fn snapshot_round_trip_preserves_learning() {
        let mut mind = StatisticalMind::new(&params());
        mind.learn(
            LearningSignal::Teach {
                prompt: "ping".into(),
                reply: "pong".into(),
            },
            &params(),
        );
        let bytes = mind.snapshot().unwrap();
        let mut restored = StatisticalMind::restore(&bytes, &params()).unwrap();
        let (_, response) = ask(&mut restored, "ping");
        assert_eq!(response.text, "pong");
        assert!(StatisticalMind::restore(b"{}", &params()).is_err());
    }

    #[test]
    fn seeded_minds_are_deterministic() {
        let run = || {
            let mut mind = StatisticalMind::new(&params());
            let corpus =
                "the stars are bright tonight .\nthe stars are far away .\nthe moon is bright .";
            for _ in 0..5 {
                mind.learn(
                    LearningSignal::Ingest {
                        name: "sky".into(),
                        text: corpus.into(),
                    },
                    &params(),
                );
            }
            ask(&mut mind, "stars").1.trace
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn initiative_requests_rest_and_asks_curiosity() {
        let mut mind = StatisticalMind::new(&params());
        let levels = RegulatoryState {
            energy: aster_core::Level::new(10.0).unwrap(),
            ..Default::default()
        };
        let draft = mind.initiative(&levels, &params()).unwrap();
        assert_eq!(draft.effect, Some(RequestEffect::Rest));
        assert!(draft.admit().is_ok());
        let levels = RegulatoryState::default();
        let first = mind.initiative(&levels, &params()).unwrap();
        assert!(first.body.contains("teach me first"));
        assert!(mind.initiative(&levels, &params()).is_none());
    }
}
