//! The "neural" engine: the statistical mind (intent, meanings, recall,
//! associations, asking, initiative, self-regulation) plus a byte-level
//! network trained in the background on what the owner taught. The network
//! adds one candidate per turn, scored and thresholded like every other.

use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

use aster_core::{
    Candidate, CandidateSource, Cognition, CognitionParams, LearnOutcome, LearningSignal,
    NeuralParams, OutboundDraft, RegulatoryState, Response, SnapshotError, TrainPoint, TrainReport,
    TurnContext,
};
use serde::{Deserialize, Serialize};

use crate::{
    StatisticalMind,
    corpus::training_text,
    neural::{ByteModel, Shape},
    rng::Rng,
};

pub const ENGINE: &str = "neural";
const MAGIC: &[u8; 5] = b"ASTE1";
const HISTORY: usize = 240;
/// Below this many training steps the network's guesses are noise; it stays quiet.
const MIN_STEPS_TO_SPEAK: u64 = 50;

#[derive(Serialize, Deserialize)]
struct Envelope {
    engine: String,
    history: VecDeque<TrainPoint>,
}

pub struct NeuralMind {
    statistical: StatisticalMind,
    model: ByteModel,
    rng: Rng,
    corpus: Vec<u8>,
    corpus_at: Option<u64>,
    history: VecDeque<TrainPoint>,
    notes: Vec<String>,
}

fn shape(params: &NeuralParams) -> Shape {
    Shape {
        context: params.context,
        embed_dim: params.embed_dim,
        hidden: params.hidden,
    }
}

impl NeuralMind {
    pub fn wrap(
        statistical: StatisticalMind,
        params: &CognitionParams,
    ) -> Result<Self, SnapshotError> {
        let mut rng = Rng::new(params.rng_seed);
        Ok(Self {
            statistical,
            model: ByteModel::new(shape(&params.neural), &mut rng).map_err(|_| SnapshotError)?,
            rng,
            corpus: Vec::new(),
            corpus_at: None,
            history: VecDeque::new(),
            notes: Vec::new(),
        })
    }

    pub fn statistical(&self) -> &StatisticalMind {
        &self.statistical
    }

    pub fn into_statistical(self) -> StatisticalMind {
        self.statistical
    }

    pub(crate) fn statistical_mut(&mut self) -> &mut StatisticalMind {
        &mut self.statistical
    }

    /// Is this a neural envelope (vs. a plain statistical snapshot)?
    pub fn is_envelope(bytes: &[u8]) -> bool {
        bytes.starts_with(MAGIC)
    }

    /// Split an envelope into its statistical snapshot bytes and the rest.
    fn open(bytes: &[u8]) -> Result<(Envelope, &[u8], &[u8]), SnapshotError> {
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or(SnapshotError)?;
        let mut parts = [0usize; 3];
        let mut at = 0;
        for part in &mut parts {
            let raw = rest.get(at..at + 8).ok_or(SnapshotError)?;
            *part = u64::from_le_bytes(raw.try_into().map_err(|_| SnapshotError)?) as usize;
            at += 8;
        }
        let mut take = |len: usize| {
            let slice = rest.get(at..at + len).ok_or(SnapshotError);
            at += len;
            slice
        };
        let meta: Envelope = serde_json::from_slice(take(parts[0])?).map_err(|_| SnapshotError)?;
        let statistical = take(parts[1])?;
        let network = take(parts[2])?;
        if at != rest.len() || meta.engine != ENGINE {
            return Err(SnapshotError);
        }
        Ok((meta, statistical, network))
    }

    /// Only the statistical part of an envelope (for switching back).
    pub fn statistical_bytes(bytes: &[u8]) -> Result<&[u8], SnapshotError> {
        Self::open(bytes).map(|(_, statistical, _)| statistical)
    }

    pub fn restore(bytes: &[u8], params: &CognitionParams) -> Result<Self, SnapshotError> {
        let (meta, statistical, network) = Self::open(bytes)?;
        let statistical = StatisticalMind::restore(statistical, params)?;
        let mut mind = Self::wrap(statistical, params)?;
        match ByteModel::from_bytes(network) {
            Ok(model) if model.shape() == shape(&params.neural) => {
                mind.model = model;
                mind.history = meta.history;
            }
            Ok(_) => mind
                .notes
                .push("network shape changed in config; training a new network".into()),
            Err(_) => mind
                .notes
                .push("saved network could not be read; training a new network".into()),
        }
        Ok(mind)
    }

    fn refresh_corpus(&mut self) {
        let at = self.statistical.updates();
        if self.corpus_at != Some(at) {
            self.corpus = training_text(&self.statistical);
            self.corpus_at = Some(at);
        }
    }
}

impl Cognition for NeuralMind {
    fn engine(&self) -> &'static str {
        ENGINE
    }

    fn respond(&mut self, context: &TurnContext<'_>) -> Response {
        let params = &context.params.neural;
        // Split borrows: the statistical pipeline runs and asks the network
        // for one candidate in the middle of it.
        let Self {
            statistical,
            model,
            rng,
            ..
        } = self;
        let mut response = statistical.respond_with(context, &mut |text, exploration| {
            propose(model, rng, text, exploration, params)
        });
        response.trace.engine = ENGINE.into();
        response.trace.notes.append(&mut self.notes);
        response
    }

    fn learn(&mut self, signal: LearningSignal, params: &CognitionParams) -> LearnOutcome {
        self.statistical.learn(signal, params)
    }

    fn initiative(
        &mut self,
        levels: &RegulatoryState,
        params: &CognitionParams,
    ) -> Option<OutboundDraft> {
        self.statistical.initiative(levels, params)
    }

    fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        let meta = serde_json::to_vec(&Envelope {
            engine: ENGINE.into(),
            history: self.history.clone(),
        })
        .map_err(|_| SnapshotError)?;
        let statistical = self.statistical.snapshot()?;
        let network = self.model.to_bytes().map_err(|_| SnapshotError)?;
        let mut out =
            Vec::with_capacity(MAGIC.len() + 24 + meta.len() + statistical.len() + network.len());
        out.extend_from_slice(MAGIC);
        for part in [&meta, &statistical, &network] {
            out.extend_from_slice(&(part.len() as u64).to_le_bytes());
        }
        for part in [meta, statistical, network] {
            out.extend_from_slice(&part);
        }
        Ok(out)
    }

    fn stats(&self) -> BTreeMap<String, f64> {
        let mut stats = self.statistical.stats();
        stats.insert("neural_steps".into(), self.model.steps() as f64);
        stats.insert("neural_loss".into(), f64::from(self.model.last_loss()));
        stats.insert("neural_parameters".into(), self.model.parameters() as f64);
        stats.insert("neural_corpus_bytes".into(), self.corpus.len() as f64);
        stats
    }

    fn corpus(&self) -> String {
        self.statistical.corpus()
    }

    fn train(&mut self, params: &CognitionParams, budget: Duration) -> Option<TrainReport> {
        let neural = &params.neural;
        if neural.train_steps_per_tick == 0 {
            return None;
        }
        self.refresh_corpus();
        if self.corpus.len() < neural.min_corpus_bytes {
            return None;
        }
        let deadline =
            Instant::now() + budget.min(Duration::from_millis(neural.max_train_ms_per_tick));
        let mut steps = 0;
        let mut loss = self.model.last_loss();
        // One minibatch at a time so the time cap holds.
        while steps < neural.train_steps_per_tick && Instant::now() < deadline {
            match self.model.train(
                &self.corpus,
                1,
                neural.batch,
                neural.learning_rate,
                &mut self.rng,
            ) {
                Ok(value) => loss = value,
                Err(error) => {
                    return Some(TrainReport {
                        steps,
                        total_steps: self.model.steps(),
                        loss,
                        corpus_bytes: self.corpus.len(),
                        note: format!("training stopped: {error}"),
                    });
                }
            }
            steps += 1;
        }
        if steps > 0 {
            self.history.push_back(TrainPoint {
                step: self.model.steps(),
                loss,
            });
            while self.history.len() > HISTORY {
                self.history.pop_front();
            }
        }
        Some(TrainReport {
            steps,
            total_steps: self.model.steps(),
            loss,
            corpus_bytes: self.corpus.len(),
            note: format!("{steps} steps, loss {loss:.3}"),
        })
    }

    fn training_history(&self) -> Vec<TrainPoint> {
        self.history.iter().copied().collect()
    }
}

/// The network's one candidate for this turn, if it has trained enough.
fn propose(
    model: &ByteModel,
    rng: &mut Rng,
    text: &str,
    exploration: f64,
    params: &NeuralParams,
) -> Option<Candidate> {
    if model.steps() < MIN_STEPS_TO_SPEAK || params.weight == 0.0 {
        return None;
    }
    let mut prefix: Vec<u8> = text
        .replace(['\t', '\n', '\r'], " ")
        .trim()
        .as_bytes()
        .to_vec();
    prefix.push(b'\t');
    let temperature = params.temperature * (0.75 + exploration);
    let sample = model
        .sample(
            &prefix,
            params.max_reply_bytes,
            b'\n',
            temperature,
            params.top_k,
            rng,
        )
        .ok()?;
    let reply = String::from_utf8(sample.bytes).ok()?;
    let reply = reply.trim();
    if reply.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        return None;
    }
    // Geometric-mean byte probability: how sure the network was, 0..1.
    let certainty = sample.mean_log_prob.exp().clamp(0.0, 1.0);
    Some(Candidate {
        text: reply.to_string(),
        source: CandidateSource::Neural,
        score: certainty.sqrt() * params.weight,
        components: BTreeMap::from([
            ("certainty".into(), certainty),
            ("temperature".into(), temperature),
            ("training_steps".into(), model.steps() as f64),
        ]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aster_core::{Decision, RegulatoryState};
    use uuid::Uuid;

    fn params() -> CognitionParams {
        let mut params = CognitionParams {
            engine: ENGINE.into(),
            rng_seed: Some(7),
            starter_pack: false,
            ..Default::default()
        };
        // Small enough to train quickly in a debug build on a slow CI runner.
        params.neural.context = 12;
        params.neural.embed_dim = 8;
        params.neural.hidden = 32;
        params.neural.batch = 16;
        params.neural.learning_rate = 0.02;
        params.neural.min_corpus_bytes = 16;
        params.neural.train_steps_per_tick = 300;
        params.neural.max_train_ms_per_tick = 10_000;
        params
    }

    fn ask(mind: &mut dyn Cognition, text: &str, params: &CognitionParams) -> Response {
        mind.respond(&TurnContext {
            turn_id: Uuid::new_v4(),
            text,
            levels: &RegulatoryState::default(),
            params,
            avoid: None,
        })
    }

    fn taught(params: &CognitionParams) -> Box<dyn Cognition> {
        let mut mind = crate::build(params, None).unwrap();
        for (prompt, reply) in [
            ("good morning", "Morning! Ready when you are."),
            ("how are you", "Steady, thanks for asking."),
            ("thank you", "Any time."),
        ] {
            mind.learn(
                LearningSignal::Teach {
                    prompt: prompt.into(),
                    reply: reply.into(),
                },
                params,
            );
        }
        mind
    }

    #[test]
    fn network_trains_in_the_background_and_adds_a_candidate() {
        let params = params();
        let mut mind = taught(&params);
        assert_eq!(mind.engine(), ENGINE);
        // Untrained: no neural candidate.
        let before = ask(mind.as_mut(), "good morning", &params);
        assert!(
            !before
                .trace
                .candidates
                .iter()
                .any(|c| c.source == CandidateSource::Neural)
        );
        // Greedy decoding, so what the network says doesn't hinge on a dice roll.
        let mut params = params;
        params.neural.top_k = 1;
        params.neural.temperature = 0.2;
        params.neural.train_steps_per_tick = 150;
        mind.train(&params, Duration::from_secs(30)).unwrap();
        let report = mind.train(&params, Duration::from_secs(30)).unwrap();
        // The time cap may stop a slow machine early; it must still get past
        // the point where the network is allowed to speak.
        assert!(report.total_steps > MIN_STEPS_TO_SPEAK, "{report:?}");
        assert!(report.loss.is_finite());
        assert_eq!(mind.training_history().len(), 2);
        // A taught prompt: on an unseen one, a network this small may stop at
        // once and stay quiet, which depends on its starting weights.
        let after = ask(mind.as_mut(), "how are you", &params);
        assert_eq!(after.trace.engine, ENGINE);
        assert_eq!(after.trace.decision, Decision::Reply);
        // The trained network spoke: its own candidate, or the same words as a learned reply.
        let neural = after
            .trace
            .candidates
            .iter()
            .any(|c| c.source == CandidateSource::Neural);
        let agreed = after
            .trace
            .notes
            .iter()
            .any(|n| n.contains("neural candidate repeated"));
        assert!(neural || agreed, "{:?}", after.trace);
        let stats = mind.stats();
        assert_eq!(stats["neural_steps"], report.total_steps as f64);
        assert!(stats["neural_corpus_bytes"] > 16.0);
    }

    #[test]
    fn snapshots_move_between_engines() {
        let params = params();
        let mut mind = taught(&params);
        mind.train(&params, Duration::from_secs(30));
        let bytes = mind.snapshot().unwrap();
        assert!(NeuralMind::is_envelope(&bytes));

        let steps = mind.stats()["neural_steps"];
        assert!(steps > 0.0);
        let restored = crate::build(&params, Some(&bytes)).unwrap();
        assert_eq!(restored.stats()["neural_steps"], steps);
        assert_eq!(restored.training_history(), mind.training_history());

        // Back to statistical: the replies survive, the network does not.
        let statistical = CognitionParams {
            engine: "statistical".into(),
            ..params.clone()
        };
        let mut plain = crate::build(&statistical, Some(&bytes)).unwrap();
        assert_eq!(plain.engine(), "statistical");
        assert_eq!(
            ask(plain.as_mut(), "thank you", &statistical).text,
            "Any time."
        );

        // And forward again: a statistical snapshot gets a fresh network.
        let again = crate::build(&params, Some(&plain.snapshot().unwrap())).unwrap();
        assert_eq!(again.engine(), ENGINE);
        assert_eq!(again.stats()["neural_steps"], 0.0);
    }

    #[test]
    fn changed_shape_starts_a_new_network_and_says_so() {
        let params = params();
        let mut mind = taught(&params);
        mind.train(&params, Duration::from_secs(30));
        let bytes = mind.snapshot().unwrap();
        let mut wider = params.clone();
        wider.neural.hidden = 48;
        let mut restored = crate::build(&wider, Some(&bytes)).unwrap();
        assert_eq!(restored.stats()["neural_steps"], 0.0);
        let response = ask(restored.as_mut(), "thank you", &wider);
        assert!(
            response
                .trace
                .notes
                .iter()
                .any(|n| n.contains("shape changed"))
        );
    }

    #[test]
    fn training_waits_for_enough_corpus_and_respects_zero_steps() {
        let mut params = params();
        params.neural.min_corpus_bytes = 1_000_000;
        let mut mind = taught(&params);
        assert!(mind.train(&params, Duration::from_secs(1)).is_none());
        params.neural.min_corpus_bytes = 1;
        params.neural.train_steps_per_tick = 0;
        assert!(mind.train(&params, Duration::from_secs(1)).is_none());
    }
}
