# neural (experimental)

A tiny character-level model trained in-process with [candle](https://github.com/huggingface/candle) on Aster's own corpus. It isn't used for replies yet; `cognition.engine` only accepts `"statistical"`.

```bash
cargo test -p aster-mind --features neural
```

Export what Aster has learned from:

```bash
curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/v1/mind/corpus > corpus.txt
```

```rust
use aster_mind::neural::CharModel;
let mut model = CharModel::new(&corpus, 8, 16)?;
let loss = model.train(&corpus, 500, 64, 42)?;
println!("{}", model.sample("hello", 80, 7)?);
```

← [docs](README.md)
