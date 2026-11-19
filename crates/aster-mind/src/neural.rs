//! Experimental (feature `neural`): a tiny character-level MLP language model
//! trained in-process with candle on the exported corpus. Not wired into the
//! runtime yet; it is the slot future self-developed generation grows into.

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{AdamW, Embedding, Linear, Optimizer, VarBuilder, VarMap, embedding, linear};

use crate::rng::Rng;

pub struct CharModel {
    chars: Vec<char>,
    context: usize,
    embedding: Embedding,
    hidden: Linear,
    output: Linear,
    varmap: VarMap,
    device: Device,
}

impl CharModel {
    pub fn new(corpus: &str, context: usize, dim: usize) -> candle_core::Result<Self> {
        let mut chars: Vec<char> = corpus.chars().collect();
        chars.push('\0');
        chars.sort_unstable();
        chars.dedup();
        let device = Device::Cpu;
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        let embedding = embedding(chars.len(), dim, vb.pp("embedding"))?;
        let hidden = linear(dim * context, 128, vb.pp("hidden"))?;
        let output = linear(128, chars.len(), vb.pp("output"))?;
        Ok(Self {
            chars,
            context,
            embedding,
            hidden,
            output,
            varmap,
            device,
        })
    }

    fn index(&self, c: char) -> u32 {
        self.chars.binary_search(&c).unwrap_or(0) as u32
    }

    fn logits(&self, inputs: &Tensor) -> candle_core::Result<Tensor> {
        let (batch, _) = inputs.dims2()?;
        let embedded = self.embedding.forward(inputs)?.reshape((batch, ()))?;
        let hidden = self.hidden.forward(&embedded)?.tanh()?;
        self.output.forward(&hidden)
    }

    /// Train for `steps` minibatches; returns the final loss.
    pub fn train(
        &mut self,
        corpus: &str,
        steps: usize,
        batch: usize,
        seed: u64,
    ) -> candle_core::Result<f32> {
        let encoded: Vec<u32> = corpus.chars().map(|c| self.index(c)).collect();
        if encoded.len() <= self.context {
            return Ok(f32::NAN);
        }
        let mut optimizer = AdamW::new_lr(self.varmap.all_vars(), 1e-2)?;
        let mut rng = Rng::new(Some(seed));
        let mut loss_value = f32::NAN;
        for _ in 0..steps {
            let mut inputs = Vec::with_capacity(batch * self.context);
            let mut targets = Vec::with_capacity(batch);
            for _ in 0..batch {
                let start = (rng.next_u64() as usize) % (encoded.len() - self.context);
                inputs.extend_from_slice(&encoded[start..start + self.context]);
                targets.push(encoded[start + self.context]);
            }
            let inputs = Tensor::from_vec(inputs, (batch, self.context), &self.device)?;
            let targets = Tensor::from_vec(targets, batch, &self.device)?;
            let loss = candle_nn::loss::cross_entropy(&self.logits(&inputs)?, &targets)?;
            optimizer.backward_step(&loss)?;
            loss_value = loss.to_scalar::<f32>()?;
        }
        Ok(loss_value)
    }

    /// Sample `length` characters after `prefix`.
    pub fn sample(&self, prefix: &str, length: usize, seed: u64) -> candle_core::Result<String> {
        let mut window: Vec<u32> = vec![self.index('\0'); self.context];
        for c in prefix.chars() {
            window.remove(0);
            window.push(self.index(c));
        }
        let mut rng = Rng::new(Some(seed));
        let mut out = String::from(prefix);
        for _ in 0..length {
            let input = Tensor::from_vec(window.clone(), (1, self.context), &self.device)?;
            let probs = candle_nn::ops::softmax(&self.logits(&input)?, 1)?
                .squeeze(0)?
                .to_vec1::<f32>()?;
            let mut pick = rng.next_f64() as f32;
            let mut next = probs.len() - 1;
            for (i, p) in probs.iter().enumerate() {
                if pick < *p {
                    next = i;
                    break;
                }
                pick -= p;
            }
            let c = self.chars[next];
            if c == '\0' {
                break;
            }
            out.push(c);
            window.remove(0);
            window.push(next as u32);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_decreases_on_repetitive_corpus() {
        let corpus = "hello aster. ".repeat(40);
        let mut model = CharModel::new(&corpus, 4, 8).unwrap();
        let early = model.train(&corpus, 5, 32, 1).unwrap();
        let late = model.train(&corpus, 200, 32, 2).unwrap();
        assert!(late < early, "{late} !< {early}");
        assert!(model.sample("hel", 5, 3).unwrap().starts_with("hel"));
    }
}
