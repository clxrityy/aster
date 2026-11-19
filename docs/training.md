# training

Aster starts empty. It learns from:

- **teaching:** `/teach hello => Hi, I'm Aster.`
- **rating:** in `/train` mode, `+` reinforces and `-` weakens. Use `/correct Better reply.` to replace a bad reply.
- **answering:** when Aster doesn't know a word or a reply, it asks. Use `/answer 1 <text>`.
- **memories:** `/remember Rust is my main language.` Aster can then recall it.
- **files:** put text in `data/ingest/`, then run `/ingest notes.txt`. This adds to its vocabulary, language model and recall.

A typical loop:

```
/train
hello                         → I don't know "hello" yet. What does it mean?
/answer 1 a friendly greeting
/teach hello => Hi! Good to see you.
hello                         → Hi! Good to see you.
+
```

When Aster knows the words but has no good reply, it asks "How would you reply to …?" instead. Your `/answer` then becomes that reply.

How a reply is chosen (shown in the thought trace):

1. Candidates come from taught associations, recalled memories and the n-gram model.
2. Each gets a score. The best must beat a threshold that moves with the levels.
3. If none does, Aster asks you instead.

Snapshots (saved every `cognition.snapshot_every_updates` updates and on shutdown):

```
/snapshot before-experiment
/snapshots
/rollback 12
```

Reproducible sessions: `/set cognition.rng_seed 42`.

← [docs](README.md)
