//! Feature `neural`: a small byte-level MLP language model trained in-process
//! with candle, only on what the owner taught (no pretrained weights, no
//! external models). Bytes, not characters, so the vocabulary never freezes.

use std::collections::HashMap;

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{
    AdamW, Embedding, Linear, Optimizer, ParamsAdamW, VarBuilder, VarMap, embedding, linear,
};

use crate::rng::Rng;

/// 256 byte values plus padding.
pub const VOCAB: usize = 257;
pub const PAD: u32 = 256;
const MAGIC: &[u8; 4] = b"ASTN";
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shape {
    pub context: usize,
    pub embed_dim: usize,
    pub hidden: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Header {
    format: u32,
    shape: Shape,
    steps: u64,
    last_loss: f32,
    /// Variable name, element count, in blob order.
    tensors: Vec<(String, Vec<usize>)>,
}

pub struct ByteModel {
    shape: Shape,
    embedding: Embedding,
    hidden: Linear,
    output: Linear,
    varmap: VarMap,
    optimizer: Option<(f64, AdamW)>,
    device: Device,
    steps: u64,
    last_loss: f32,
}

/// One sampled continuation and how sure the model was of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub bytes: Vec<u8>,
    /// Mean natural-log probability of the sampled bytes (≤ 0).
    pub mean_log_prob: f64,
}

impl ByteModel {
    /// A fresh network. Weights are drawn from `rng` (candle's own CPU
    /// generator cannot be seeded), so a seeded mind starts from the same
    /// weights every time.
    pub fn new(shape: Shape, rng: &mut Rng) -> candle_core::Result<Self> {
        let device = Device::Cpu;
        let mut varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        let embedding = embedding(VOCAB, shape.embed_dim, vb.pp("embedding"))?;
        let flat = shape.embed_dim * shape.context;
        let hidden = linear(flat, shape.hidden, vb.pp("hidden"))?;
        let output = linear(shape.hidden, VOCAB, vb.pp("output"))?;
        // Same distributions candle uses: N(0, 1) embeddings, Kaiming-normal
        // weights, U(±1/√fan_in) biases. Fixed order, so draws are reproducible.
        let normal = |rng: &mut Rng, stdev: f64| {
            let u1 = 1.0 - rng.next_f64();
            let u2 = rng.next_f64();
            ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos() * stdev) as f32
        };
        let uniform = |rng: &mut Rng, bound: f64| ((rng.next_f64() * 2.0 - 1.0) * bound) as f32;
        for (name, rows, fan_in, bias) in [
            ("embedding.weight", VOCAB, shape.embed_dim, false),
            ("hidden.weight", shape.hidden, flat, false),
            ("hidden.bias", shape.hidden, flat, true),
            ("output.weight", VOCAB, shape.hidden, false),
            ("output.bias", VOCAB, shape.hidden, true),
        ] {
            let tensor = if bias {
                let bound = 1.0 / (fan_in as f64).sqrt();
                let values = (0..rows).map(|_| uniform(rng, bound)).collect();
                Tensor::from_vec(values, rows, &device)?
            } else {
                let stdev = if name == "embedding.weight" {
                    1.0
                } else {
                    std::f64::consts::SQRT_2 / (fan_in as f64).sqrt()
                };
                let values = (0..rows * fan_in).map(|_| normal(rng, stdev)).collect();
                Tensor::from_vec(values, (rows, fan_in), &device)?
            };
            varmap.set_one(name, tensor)?;
        }
        Ok(Self {
            shape,
            embedding,
            hidden,
            output,
            varmap,
            optimizer: None,
            device,
            steps: 0,
            last_loss: f32::NAN,
        })
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    pub fn last_loss(&self) -> f32 {
        self.last_loss
    }

    pub fn parameters(&self) -> usize {
        let s = self.shape;
        VOCAB * s.embed_dim + (s.embed_dim * s.context + 1) * s.hidden + (s.hidden + 1) * VOCAB
    }

    fn logits(&self, inputs: &Tensor) -> candle_core::Result<Tensor> {
        let (batch, _) = inputs.dims2()?;
        let embedded = self.embedding.forward(inputs)?.reshape((batch, ()))?;
        let hidden = self.hidden.forward(&embedded)?.tanh()?;
        self.output.forward(&hidden)
    }

    /// The `context` tokens before position `end` (padded at the start).
    fn window(&self, encoded: &[u32], end: usize) -> Vec<u32> {
        let context = self.shape.context;
        (0..context)
            .map(move |i| {
                let at = end as isize - context as isize + i as isize;
                if at < 0 { PAD } else { encoded[at as usize] }
            })
            .collect()
    }

    /// Train `steps` minibatches on `corpus`. The optimizer persists across
    /// calls (until restore or a learning-rate change). Returns the last loss.
    pub fn train(
        &mut self,
        corpus: &[u8],
        steps: u32,
        batch: usize,
        learning_rate: f64,
        rng: &mut Rng,
    ) -> candle_core::Result<f32> {
        if corpus.len() < 2 || steps == 0 {
            return Ok(self.last_loss);
        }
        if self.optimizer.as_ref().map(|(lr, _)| *lr) != Some(learning_rate) {
            let params = ParamsAdamW {
                lr: learning_rate,
                ..ParamsAdamW::default()
            };
            self.optimizer = Some((learning_rate, AdamW::new(self.varmap.all_vars(), params)?));
        }
        let encoded: Vec<u32> = corpus.iter().map(|b| u32::from(*b)).collect();
        let context = self.shape.context;
        for _ in 0..steps {
            let mut inputs = Vec::with_capacity(batch * context);
            let mut targets = Vec::with_capacity(batch);
            for _ in 0..batch {
                let target = 1 + (rng.next_u64() as usize) % (encoded.len() - 1);
                inputs.extend(self.window(&encoded, target));
                targets.push(encoded[target]);
            }
            let inputs = Tensor::from_vec(inputs, (batch, context), &self.device)?;
            let targets = Tensor::from_vec(targets, batch, &self.device)?;
            let loss = candle_nn::loss::cross_entropy(&self.logits(&inputs)?, &targets)?;
            let (_, optimizer) = self.optimizer.as_mut().expect("set above");
            optimizer.backward_step(&loss)?;
            self.last_loss = loss.to_scalar::<f32>()?;
            self.steps += 1;
        }
        Ok(self.last_loss)
    }

    /// Sample up to `max` bytes after `prefix`, stopping at `stop`.
    pub fn sample(
        &self,
        prefix: &[u8],
        max: usize,
        stop: u8,
        temperature: f64,
        top_k: usize,
        rng: &mut Rng,
    ) -> candle_core::Result<Sample> {
        let mut encoded: Vec<u32> = prefix.iter().map(|b| u32::from(*b)).collect();
        let start = encoded.len();
        let mut log_prob_sum = 0.0;
        for _ in 0..max {
            let window = self.window(&encoded, encoded.len());
            let input = Tensor::from_vec(window, (1, self.shape.context), &self.device)?;
            let logits = (self.logits(&input)? / temperature)?;
            let probs = candle_nn::ops::softmax(&logits, 1)?
                .squeeze(0)?
                .to_vec1::<f32>()?;
            let mut ranked: Vec<(usize, f32)> = probs
                .iter()
                .copied()
                .enumerate()
                .filter(|(i, _)| *i != PAD as usize)
                .collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
            ranked.truncate(top_k.max(1));
            let total: f32 = ranked.iter().map(|(_, p)| p).sum();
            let mut pick = rng.next_f64() as f32 * total;
            let mut next = ranked[ranked.len() - 1];
            for candidate in &ranked {
                if pick < candidate.1 {
                    next = *candidate;
                    break;
                }
                pick -= candidate.1;
            }
            let byte = next.0 as u8;
            if byte == stop {
                break;
            }
            log_prob_sum += f64::from((next.1 / total).max(1e-9)).ln();
            encoded.push(next.0 as u32);
        }
        let bytes: Vec<u8> = encoded[start..].iter().map(|t| *t as u8).collect();
        let mean_log_prob = if bytes.is_empty() {
            f64::NEG_INFINITY
        } else {
            log_prob_sum / bytes.len() as f64
        };
        Ok(Sample {
            bytes,
            mean_log_prob,
        })
    }

    /// Weights and counters as bytes: magic, header length, JSON header, then
    /// little-endian f32 tensors. Optimizer moments are not kept.
    pub fn to_bytes(&self) -> candle_core::Result<Vec<u8>> {
        let data = self.varmap.data().lock().expect("varmap lock");
        let mut names: Vec<&String> = data.keys().collect();
        names.sort();
        let mut tensors = Vec::with_capacity(names.len());
        let mut blob = Vec::new();
        for name in names {
            let tensor = data[name].as_tensor();
            tensors.push((name.clone(), tensor.dims().to_vec()));
            for value in tensor.flatten_all()?.to_vec1::<f32>()? {
                blob.extend_from_slice(&value.to_le_bytes());
            }
        }
        let header = serde_json::to_vec(&Header {
            format: FORMAT,
            shape: self.shape,
            steps: self.steps,
            last_loss: self.last_loss,
            tensors,
        })
        .map_err(candle_core::Error::wrap)?;
        let mut out = Vec::with_capacity(8 + header.len() + blob.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&blob);
        Ok(out)
    }

    pub fn from_bytes(bytes: &[u8]) -> candle_core::Result<Self> {
        let bad = || candle_core::Error::Msg("invalid neural model bytes".into());
        if bytes.len() < 8 || &bytes[..4] != MAGIC {
            return Err(bad());
        }
        let len = u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| bad())?) as usize;
        let header: Header =
            serde_json::from_slice(bytes.get(8..8 + len).ok_or_else(bad)?).map_err(|_| bad())?;
        if header.format != FORMAT {
            return Err(bad());
        }
        // Every weight is overwritten from the blob below.
        let mut model = Self::new(header.shape, &mut Rng::new(Some(0)))?;
        let mut offset = 8 + len;
        let mut loaded = HashMap::new();
        for (name, dims) in &header.tensors {
            let count: usize = dims.iter().product();
            let end = offset + count * 4;
            let raw = bytes.get(offset..end).ok_or_else(bad)?;
            let values: Vec<f32> = raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            loaded.insert(
                name.clone(),
                Tensor::from_vec(values, dims.as_slice(), &model.device)?,
            );
            offset = end;
        }
        if offset != bytes.len() {
            return Err(bad());
        }
        {
            let data = model.varmap.data().lock().expect("varmap lock");
            if data.len() != loaded.len() {
                return Err(bad());
            }
            for (name, var) in data.iter() {
                let value = loaded.get(name).ok_or_else(bad)?;
                if value.dims() != var.dims() {
                    return Err(bad());
                }
                var.set(value)?;
            }
        }
        model.steps = header.steps;
        model.last_loss = header.last_loss;
        Ok(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> Shape {
        Shape {
            context: 8,
            embed_dim: 8,
            hidden: 64,
        }
    }

    #[test]
    fn loss_decreases_and_learns_a_pair() {
        let corpus = "hello\thi there\n".repeat(30);
        let mut rng = Rng::new(Some(1));
        let mut model = ByteModel::new(shape(), &mut rng).unwrap();
        let early = model
            .train(corpus.as_bytes(), 5, 32, 0.01, &mut rng)
            .unwrap();
        let late = model
            .train(corpus.as_bytes(), 300, 32, 0.01, &mut rng)
            .unwrap();
        assert!(late < early, "{late} !< {early}");
        assert_eq!(model.steps(), 305);
        let sample = model
            .sample(b"hello\t", 20, b'\n', 0.5, 3, &mut rng)
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&sample.bytes), "hi there");
        assert!(sample.mean_log_prob > -0.5, "{}", sample.mean_log_prob);
    }

    #[test]
    fn bytes_round_trip_and_reject_garbage() {
        let corpus = "aster learns bytes\n".repeat(10);
        let mut rng = Rng::new(Some(2));
        let mut model = ByteModel::new(shape(), &mut rng).unwrap();
        model
            .train(corpus.as_bytes(), 20, 16, 0.01, &mut rng)
            .unwrap();
        let bytes = model.to_bytes().unwrap();
        let restored = ByteModel::from_bytes(&bytes).unwrap();
        assert_eq!(restored.steps(), 20);
        assert_eq!(restored.to_bytes().unwrap(), bytes);
        let a = model
            .sample(b"aster ", 10, b'\n', 1.0, 4, &mut Rng::new(Some(9)))
            .unwrap();
        let b = restored
            .sample(b"aster ", 10, b'\n', 1.0, 4, &mut Rng::new(Some(9)))
            .unwrap();
        assert_eq!(a, b);
        assert!(ByteModel::from_bytes(b"nope").is_err());
        assert!(ByteModel::from_bytes(&bytes[..bytes.len() - 4]).is_err());
    }

    #[test]
    fn same_seed_same_starting_weights() {
        let a = ByteModel::new(shape(), &mut Rng::new(Some(4))).unwrap();
        let b = ByteModel::new(shape(), &mut Rng::new(Some(4))).unwrap();
        let c = ByteModel::new(shape(), &mut Rng::new(Some(5))).unwrap();
        assert_eq!(a.to_bytes().unwrap(), b.to_bytes().unwrap());
        assert_ne!(a.to_bytes().unwrap(), c.to_bytes().unwrap());
    }

    #[test]
    fn non_ascii_bytes_are_in_vocabulary() {
        let corpus = "café → ☕\n".repeat(20);
        let mut rng = Rng::new(Some(3));
        let mut model = ByteModel::new(shape(), &mut rng).unwrap();
        let loss = model
            .train(corpus.as_bytes(), 50, 16, 0.01, &mut rng)
            .unwrap();
        assert!(loss.is_finite());
    }
}
