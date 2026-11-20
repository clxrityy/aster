# neural

A second engine. It does everything the statistical engine does (meanings, recall, taught replies, asking) and adds a small byte-level network trained in-process with [candle](https://github.com/huggingface/candle). It learns only from what you taught Aster. There are no pretrained weights and no external models.

```bash
aster-set cognition.engine '"neural"'      # or the Neural page, or /set in the console
```

Aster switches on its next tick. It snapshots what it knows, rebuilds as the neural engine, and keeps everything learned. A fresh network starts training. Switching back to `"statistical"` keeps everything except the network.

It needs a build with the feature. The container image has it by default:

```bash
cargo build --release -p aster-api --features neural
```

Without the feature, `engine = "neural"` is rejected when the config is checked.

## how it learns

On each runtime tick (unless Aster is paused or resting), it trains for up to `train_steps_per_tick` minibatches or `max_train_ms_per_tick`, whichever comes first. The training text is built from:

```
<prompt>⇥<reply>      every learned reply (stronger ones repeated)
what is <x>⇥<meaning> every meaning
<line>                recalled memories and ingested text
```

Training waits until there are `min_corpus_bytes` of it. Watch the loss fall on the dashboard's Neural page, or with `aster-curl /v1/mind/training`.

## how it answers

After 50 training steps, each turn the network continues `<your message>⇥` up to a newline. That text becomes one more candidate in the thought trace, source `neural`. Its score is how sure the network was, times `weight`. It wins only when it beats the other candidates and the threshold. If it says the same thing as a learned reply, the trace notes that instead of listing it twice.

Prefer a neural candidate ("Use this") and it becomes a learned reply, which the network then trains on too.

## settings (`[cognition.neural]`)

| key | default | meaning |
|---|---|---|
| `context` | 32 | bytes the network sees per prediction |
| `embed_dim` · `hidden` | 24 · 256 | network size |
| `learning_rate` · `batch` | 0.003 · 32 | training |
| `train_steps_per_tick` | 8 | 0 pauses training |
| `max_train_ms_per_tick` | 200 | hard time cap per tick |
| `min_corpus_bytes` | 512 | wait for this much training text |
| `weight` | 0.6 | scales its candidate's score; 0 silences it |
| `temperature` · `top_k` | 0.8 · 8 | sampling |
| `max_reply_bytes` | 160 | longest reply |

Changing `context`, `embed_dim` or `hidden` starts a new network the next time the mind is restored: a restart, rollback or engine switch (the trace notes it). Weights persist in mind snapshots. The optimizer's momentum restarts with the process.

← [docs](README.md)
