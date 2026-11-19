# training

Aster starts with a small **starter pack**: a few hundred everyday meanings, small-talk replies, and synonym groups. It can greet you, answer "what is a cat", and understand that "hey", "hi" and "hello" mean the same thing from the first message. Everything you teach wins over the pack. Turn it off with `cognition.starter_pack = false` before the first start.

It learns from:

- **teaching:** `/teach hello => Hi, I'm Aster.`
- **rating:** in `/train` mode, `+` reinforces and `-` weakens. Use `/correct Better reply.` to replace a bad reply.
- **answering:** when Aster doesn't know a word or a reply, it asks. Use `/answer 1 <text>`.
- **talking:** say what something means in plain words: `a nebula means a cloud of gas`, or `it's a friendly greeting` right after Aster asks.
- **memories:** `/remember Rust is my main language.` Aster can then recall it.
- **files:** put text in `data/ingest/`, then run `/ingest notes.txt`. This adds to its vocabulary, language model and recall.

Aster keeps two kinds of knowledge apart:

| | learned from | used when you |
|---|---|---|
| **meanings** | answering "What does X mean?", or saying "X means …" | ask `what is X`, `what does X mean`, `define X`, `tell me about X` |
| **replies** | `/teach`, `/correct`, answering "How would you reply to …?", `+` | say anything else |

So telling Aster what "hello" means teaches it the meaning, not to recite that meaning whenever you say hello.

A typical loop:

```
/train
hello                         → I don't know what "hello" means yet. What does it mean?
it's a friendly greeting      → Got it. "hello" means a friendly greeting.
hello                         → I'm not sure how to answer that yet. How would you reply to "hello"?
/answer 1 Hi! Good to see you.
hello                         → Hi! Good to see you.
+
what does hello mean          → "hello" means a friendly greeting.
```

`/answer` works for either question. Rating a meaning with `-` forgets it; `/correct` replaces it.

## packs

A pack is a plain-text file of meanings, replies and synonyms. Write your own, save it as `data/ingest/<name>.aster`, and load it with `/ingest <name>.aster`:

```
# my pack
[meanings]
quasar: a very bright galactic core
[replies]
good morning aster => Morning! Ready when you are.
[synonyms]
quasar, qso, quasi-stellar object
```

- **meanings** answer "what is X". A meaning you already taught is kept.
- **replies** start a little weaker than taught ones. A prompt you already taught a reply for is skipped.
- **synonyms** are matched as one word everywhere: replies, meanings, and unknown-word detection. Plural and possessive forms (`stars`, `star's`) already match `star`.

The built-in pack is [`crates/aster-mind/packs/starter.aster`](../crates/aster-mind/packs/starter.aster). It loads into new minds, and once into existing minds when you upgrade.

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
