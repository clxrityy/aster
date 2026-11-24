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

Once it is ready (at least `min_steps_to_speak` steps, and the average loss of its last few training rounds at or below `max_loss_to_speak`), each turn the network continues `<your message>⇥` up to a newline. It tries `samples` times and keeps the continuation it was surest of. That text becomes one more candidate in the thought trace, source `neural`.

Its score is how sure the network was (below 0.35 it is dropped), times `weight`, times the share of its words Aster already knows. If fewer than `min_known_words` of them are known, it is dropped. It wins only when it beats the other candidates and the threshold. If it says the same thing as a learned reply, the trace notes that instead of listing it twice. Whenever it stays quiet or is dropped, the trace says why (`neural quiet: loss 2.41 > 1.30`, `neural candidate dropped: 2/6 words known`).

Prefer a neural candidate ("Use this") and it becomes a learned reply, which the network then trains on too.

## settings (`[cognition.neural]`)

| key | default | meaning |
|---|---|---|
| `context` | 32 | bytes the network sees per prediction |
| `embed_dim` · `hidden` | 24 · 256 | network size |
| `learning_rate` · `batch` | 0.003 · 32 | training |
| `train_steps_per_tick` | 64 | 0 pauses training |
| `max_train_ms_per_tick` | 200 | hard time cap per tick |
| `min_corpus_bytes` | 512 | wait for this much training text |
| `weight` | 0.6 | scales its candidate's score; 0 silences it |
| `temperature` · `top_k` | 0.6 · 5 | sampling |
| `max_reply_bytes` | 160 | longest reply |
| `min_steps_to_speak` | 2000 | stay quiet until trained this many steps |
| `max_loss_to_speak` | 1.3 | …and recent loss (nats per byte) is this low |
| `min_known_words` | 0.8 | share of a reply's words Aster must know; 0 turns the check off |
| `samples` | 3 | continuations tried per turn |

## words, not jumbles

The network reads bytes, not words. It has to learn spelling before it can say anything, and that takes text and time:

- **Give it more text.** A few taught replies are a few KB. It needs tens of KB of real sentences: [read](reading.md) pages, upload notes, or ingest files. `neural_corpus_bytes` on the Neural page shows how much it has. Aim for 50 KB or more.
- **Let it train.** Watch the loss on the Neural page. Around 2.5 it babbles, under about 1.5 it spells common words, and under 1.0 it starts to sound like what it read. It stays quiet until `max_loss_to_speak`.
- **Train faster** on a machine with spare CPU: `aster-set cognition.neural.max_train_ms_per_tick 800` and `aster-set cognition.neural.train_steps_per_tick 256`. A step takes about 2 ms on a laptop, often more on a small VPS. Expect half-words around 20k steps and mostly real words with some fragments around 50k (on about 100 KB of text). Until then it stays quiet, and the trace says why.
- **Rate it.** Use `-` on bad neural candidates and "Use this" on good ones. A preferred one becomes a learned reply, and the network trains on it too.
- **Make it stricter** if it still slips through: lower `temperature` (0.4) or `top_k` (3), or raise `min_known_words` (0.9).

Changing `context`, `embed_dim` or `hidden` starts a new network the next time the mind is restored: a restart, rollback or engine switch (the trace notes it). Weights persist in mind snapshots. The optimizer's momentum restarts with the process.

← [docs](README.md)
