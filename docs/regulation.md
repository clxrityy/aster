# regulation

Seven levels, each 0–100: `energy`, `focus`, `cognitive_load`, `curiosity`, `stimulation`, `stress`, `confidence`. They model internal regulation. They are not feelings.

**You set:** baselines, decay rates, min/max bounds, and how much each event moves each level.

```toml
[regulation.levels]
stress = { baseline = 10.0, decay_per_min = 0.08, min = 0.0, max = 60.0 }

[regulation.events]
negative_feedback = { confidence = -3.0, stress = 3.0 }
```

**Aster regulates:** the level values themselves, within your bounds. It also adjusts:

- `exploration`: how often it tries its second-best reply. Poor ratings raise it. Bounded by `cognition.exploration_min/max`.
- `caution`: the confidence a reply needs. Poor ratings raise it. Bounded by `cognition.answer_confidence_min/max`.

How levels act on behavior:

| level | effect |
|---|---|
| curiosity ↑ | slightly higher threshold (asks more); asks on its own when idle |
| stress ↑ / confidence ↓ | higher threshold: asks rather than guesses |
| cognitive_load ↑ | shorter generated replies |
| energy < `rest_energy_below` | requests rest (you `/approve` or `/deny`) |

Events: `message_received`, `reply_sent`, `positive_feedback`, `negative_feedback`, `unknown_concept`, `taught`, `question_answered`, `ingested`, `rest`.

```
/history     # recent level changes and causes
```

← [docs](README.md)
