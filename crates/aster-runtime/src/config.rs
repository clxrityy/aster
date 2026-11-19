//! Hot-reloaded `aster.toml`. Owner configuration holds limits, rates, and
//! bounds; values Aster regulates itself live in its state and mind instead.
//! An invalid edit is rejected and the previous configuration stays active.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use arc_swap::ArcSwap;
use aster_core::{CognitionParams, RegulationRules};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::events::{Envelope, Event};

pub const LOG_CATEGORIES: [&str; 7] = [
    "conversations",
    "thoughts",
    "levels",
    "memories",
    "requests",
    "config",
    "runtime",
];

/// Keys under these prefixes only take effect after a restart.
pub const RESTART_REQUIRED: [&str; 2] = ["database.", "server.bind"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerSection {
    pub body_limit_bytes: usize,
    pub request_timeout_ms: u64,
    /// Owner API requests per minute for runtime routes; 0 disables the limit.
    pub rate_limit_per_minute: u32,
    pub shutdown_drain_ms: u64,
    /// Directory `/ingest` may read from (paths are resolved inside it).
    pub ingest_dir: String,
    pub ingest_max_bytes: u64,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            body_limit_bytes: 128 * 1024,
            request_timeout_ms: 15_000,
            rate_limit_per_minute: 240,
            shutdown_drain_ms: 5_000,
            ingest_dir: "data/ingest".into(),
            ingest_max_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DatabaseSection {
    pub pool_size: u32,
    pub acquire_timeout_ms: u64,
    pub statement_timeout_ms: u64,
}

impl Default for DatabaseSection {
    fn default() -> Self {
        Self {
            pool_size: 5,
            acquire_timeout_ms: 5_000,
            statement_timeout_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeSection {
    pub tick_interval_ms: u64,
    /// Freeze level dynamics and initiative.
    pub paused: bool,
    pub idle_before_initiative_secs: u64,
    pub max_pending_outbound: u32,
    pub max_outbound_per_hour: u32,
    pub outbound_expiry_hours: u64,
    /// Minimum level change before decay is persisted.
    pub persist_level_epsilon: f64,
    pub rest_minutes: u64,
    pub keep_snapshots: u32,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            tick_interval_ms: 5_000,
            paused: false,
            idle_before_initiative_secs: 120,
            max_pending_outbound: 5,
            max_outbound_per_hour: 6,
            outbound_expiry_hours: 72,
            persist_level_epsilon: 0.5,
            rest_minutes: 10,
            keep_snapshots: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LogsSection {
    pub dir: String,
    pub enabled: Vec<String>,
    /// Delete daily log files older than this; 0 keeps them forever.
    pub retention_days: u32,
}

impl Default for LogsSection {
    fn default() -> Self {
        Self {
            dir: "data/logs".into(),
            enabled: LOG_CATEGORIES.iter().map(|c| c.to_string()).collect(),
            retention_days: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ConsoleSection {
    pub server_url: String,
    pub refresh_ms: u64,
    pub show_trace: bool,
    pub training_prompt_after_reply: bool,
    pub history_turns: u32,
}

impl Default for ConsoleSection {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:3000".into(),
            refresh_ms: 100,
            show_trace: true,
            training_prompt_after_reply: true,
            history_turns: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AsterConfig {
    pub server: ServerSection,
    pub database: DatabaseSection,
    pub runtime: RuntimeSection,
    pub regulation: RegulationRules,
    pub cognition: CognitionParams,
    pub logs: LogsSection,
    pub console: ConsoleSection,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to read or write the config file: {0}")]
    Io(String),
    #[error("config is not valid TOML for Aster: {0}")]
    Parse(String),
    #[error("config value out of range: {0}")]
    Invalid(String),
    #[error("unknown or malformed config key: {0}")]
    Key(String),
}

fn range<T: PartialOrd + std::fmt::Display>(
    name: &str,
    value: T,
    min: T,
    max: T,
) -> Result<(), ConfigError> {
    if value < min || value > max {
        return Err(ConfigError::Invalid(format!(
            "{name} must be within {min}..={max}"
        )));
    }
    Ok(())
}

impl AsterConfig {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: AsterConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.message().to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let s = &self.server;
        range(
            "server.body_limit_bytes",
            s.body_limit_bytes,
            1024,
            16 << 20,
        )?;
        range(
            "server.request_timeout_ms",
            s.request_timeout_ms,
            100,
            600_000,
        )?;
        range("server.shutdown_drain_ms", s.shutdown_drain_ms, 0, 60_000)?;
        range("server.ingest_max_bytes", s.ingest_max_bytes, 1, 64 << 20)?;
        if s.ingest_dir.trim().is_empty() {
            return Err(ConfigError::Invalid("server.ingest_dir must be set".into()));
        }
        let d = &self.database;
        range("database.pool_size", d.pool_size, 1, 64)?;
        range(
            "database.acquire_timeout_ms",
            d.acquire_timeout_ms,
            100,
            600_000,
        )?;
        range(
            "database.statement_timeout_ms",
            d.statement_timeout_ms,
            100,
            600_000,
        )?;
        let r = &self.runtime;
        range(
            "runtime.tick_interval_ms",
            r.tick_interval_ms,
            100,
            3_600_000,
        )?;
        range(
            "runtime.max_pending_outbound",
            r.max_pending_outbound,
            0,
            100,
        )?;
        range(
            "runtime.max_outbound_per_hour",
            r.max_outbound_per_hour,
            0,
            1000,
        )?;
        range(
            "runtime.outbound_expiry_hours",
            r.outbound_expiry_hours,
            1,
            24 * 365,
        )?;
        range(
            "runtime.persist_level_epsilon",
            r.persist_level_epsilon,
            0.0,
            100.0,
        )?;
        range("runtime.rest_minutes", r.rest_minutes, 0, 24 * 60)?;
        range("runtime.keep_snapshots", r.keep_snapshots, 1, 10_000)?;
        self.regulation
            .validate()
            .map_err(|e| ConfigError::Invalid(e.0))?;
        self.cognition
            .validate()
            .map_err(|e| ConfigError::Invalid(e.0))?;
        for category in &self.logs.enabled {
            if !LOG_CATEGORIES.contains(&category.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "logs.enabled: unknown category {category:?}"
                )));
            }
        }
        if self.logs.dir.trim().is_empty() {
            return Err(ConfigError::Invalid("logs.dir must be set".into()));
        }
        range("console.refresh_ms", self.console.refresh_ms, 16, 5_000)?;
        range("console.history_turns", self.console.history_turns, 1, 500)?;
        Ok(())
    }
}

/// Dotted leaf keys whose values differ between two configs.
pub fn changed_keys(old: &AsterConfig, new: &AsterConfig) -> Vec<String> {
    fn walk(prefix: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
        match (a, b) {
            (Value::Object(a), Value::Object(b)) => {
                let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    walk(
                        &path,
                        a.get(key).unwrap_or(&Value::Null),
                        b.get(key).unwrap_or(&Value::Null),
                        out,
                    );
                }
            }
            _ if a != b => out.push(prefix.to_string()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    let a = serde_json::to_value(old).unwrap_or(Value::Null);
    let b = serde_json::to_value(new).unwrap_or(Value::Null);
    walk("", &a, &b, &mut out);
    out
}

pub fn restart_required(keys: &[String]) -> Vec<String> {
    keys.iter()
        .filter(|key| {
            RESTART_REQUIRED
                .iter()
                .any(|prefix| key.starts_with(prefix))
        })
        .cloned()
        .collect()
}

fn json_to_toml(value: &Value) -> Result<toml_edit::Value, ConfigError> {
    Ok(match value {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else {
                n.as_f64()
                    .ok_or_else(|| ConfigError::Key("number out of range".into()))?
                    .into()
            }
        }
        Value::String(s) => s.as_str().into(),
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(json_to_toml(item)?);
            }
            toml_edit::Value::Array(array)
        }
        Value::Null | Value::Object(_) => {
            return Err(ConfigError::Key(
                "values must be booleans, numbers, strings, or arrays".into(),
            ));
        }
    })
}

/// Interpret console input: TOML literal (`3`, `true`, `"x"`, `[..]`) or bare string.
pub fn parse_cli_value(raw: &str) -> Value {
    let raw = raw.trim();
    if let Ok(table) = format!("v = {raw}").parse::<toml::Table>()
        && let Some(value) = table.get("v")
        && let Ok(json) = serde_json::to_value(value)
    {
        return json;
    }
    Value::String(raw.to_string())
}

struct Inner {
    path: PathBuf,
    current: ArcSwap<AsterConfig>,
    last_text: Mutex<Option<String>>,
    write_lock: tokio::sync::Mutex<()>,
}

/// Shared, cheaply clonable handle to the live configuration.
#[derive(Clone)]
pub struct ConfigHandle {
    inner: Arc<Inner>,
}

impl ConfigHandle {
    /// Load from `path`; a missing file means all defaults.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let text = read_optional(&path)?;
        let config = match &text {
            Some(text) => AsterConfig::parse(text)?,
            None => AsterConfig::default(),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                current: ArcSwap::from_pointee(config),
                last_text: Mutex::new(text),
                write_lock: tokio::sync::Mutex::new(()),
            }),
        })
    }

    pub fn from_config(config: AsterConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                path: PathBuf::from("aster.toml"),
                current: ArcSwap::from_pointee(config),
                last_text: Mutex::new(None),
                write_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn get(&self) -> Arc<AsterConfig> {
        self.inner.current.load_full()
    }

    fn install(&self, text: Option<String>, config: AsterConfig) -> Vec<String> {
        let old = self.get();
        let keys = changed_keys(&old, &config);
        self.inner.current.store(Arc::new(config));
        *self
            .inner
            .last_text
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = text;
        keys
    }

    /// Re-read the file if its text changed. `Ok(None)` when unchanged.
    pub fn reload_if_changed(&self) -> Result<Option<Vec<String>>, ConfigError> {
        let text = read_optional(&self.inner.path)?;
        {
            let mut last = self
                .inner
                .last_text
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == text {
                return Ok(None);
            }
            // Remember the text even if invalid, so one bad edit reports once.
            *last = text.clone();
        }
        let config = match &text {
            Some(text) => AsterConfig::parse(text)?,
            None => AsterConfig::default(),
        };
        Ok(Some(self.install(text, config)))
    }

    /// Set dotted keys (e.g. `runtime.tick_interval_ms`), preserving comments.
    /// Validates the result before atomically replacing the file.
    pub async fn set(&self, updates: &[(String, Value)]) -> Result<Vec<String>, ConfigError> {
        let _guard = self.inner.write_lock.lock().await;
        let original = read_optional(&self.inner.path)?.unwrap_or_default();
        let mut document: toml_edit::DocumentMut = original
            .parse()
            .map_err(|e: toml_edit::TomlError| ConfigError::Parse(e.message().to_string()))?;
        for (key, value) in updates {
            let parts: Vec<&str> = key.split('.').collect();
            if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
                return Err(ConfigError::Key(key.clone()));
            }
            let mut table = document.as_table_mut();
            for part in &parts[..parts.len() - 1] {
                table = table
                    .entry(part)
                    .or_insert_with(toml_edit::table)
                    .as_table_mut()
                    .ok_or_else(|| ConfigError::Key(key.clone()))?;
            }
            let leaf = parts[parts.len() - 1];
            let mut replacement = json_to_toml(value)?;
            match table.get_mut(leaf).and_then(toml_edit::Item::as_value_mut) {
                // Keep the existing key (and its comments) and value spacing.
                Some(existing) => {
                    *replacement.decor_mut() = existing.decor().clone();
                    *existing = replacement;
                }
                None => {
                    table.insert(leaf, toml_edit::Item::Value(replacement));
                }
            }
        }
        let text = document.to_string();
        let config = AsterConfig::parse(&text)?;
        write_atomic(&self.inner.path, &text)?;
        Ok(self.install(Some(text), config))
    }

    /// Poll the file and broadcast reloads/rejections until `stop` fires.
    pub fn spawn_watcher(
        &self,
        bus: broadcast::Sender<Envelope>,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) {
        let handle = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = stop.changed() => return,
                }
                match handle.reload_if_changed() {
                    Ok(None) => {}
                    Ok(Some(keys)) if keys.is_empty() => {}
                    Ok(Some(keys)) => {
                        tracing::info!(?keys, "configuration reloaded");
                        let restart_required = restart_required(&keys);
                        let _ = bus.send(Envelope::now(Event::ConfigChanged {
                            keys,
                            restart_required,
                        }));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "configuration edit rejected; keeping previous");
                        let _ = bus.send(Envelope::now(Event::ConfigRejected {
                            error: error.to_string(),
                        }));
                    }
                }
            }
        });
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io(e.kind().to_string())),
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| ConfigError::Io(e.kind().to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| ConfigError::Io(e.kind().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aster-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("aster.toml")
    }

    #[test]
    fn example_file_documents_exactly_the_defaults() {
        let example = AsterConfig::parse(include_str!("../../../aster.example.toml")).unwrap();
        assert_eq!(example, AsterConfig::default());
    }

    #[test]
    fn defaults_validate_and_partial_files_merge() {
        AsterConfig::default().validate().unwrap();
        let config = AsterConfig::parse("[runtime]\ntick_interval_ms = 250\n").unwrap();
        assert_eq!(config.runtime.tick_interval_ms, 250);
        assert_eq!(config.server.request_timeout_ms, 15_000);
    }

    #[test]
    fn unknown_keys_and_out_of_range_values_are_rejected() {
        assert!(AsterConfig::parse("[runtime]\ntick_ms = 3\n").is_err());
        assert!(AsterConfig::parse("[runtime]\ntick_interval_ms = 1\n").is_err());
        assert!(AsterConfig::parse("[logs]\nenabled = [\"secrets\"]\n").is_err());
    }

    #[test]
    fn invalid_reload_keeps_previous_config() {
        let path = temp_path("reload");
        std::fs::write(&path, "[runtime]\ntick_interval_ms = 300\n").unwrap();
        let handle = ConfigHandle::load(&path).unwrap();
        std::fs::write(&path, "[runtime]\ntick_interval_ms = 0\n").unwrap();
        assert!(handle.reload_if_changed().is_err());
        assert_eq!(handle.get().runtime.tick_interval_ms, 300);
        // Same invalid text is not re-reported.
        assert!(handle.reload_if_changed().unwrap().is_none());
        std::fs::write(&path, "[runtime]\ntick_interval_ms = 400\n").unwrap();
        let keys = handle.reload_if_changed().unwrap().unwrap();
        assert_eq!(keys, ["runtime.tick_interval_ms"]);
        assert_eq!(handle.get().runtime.tick_interval_ms, 400);
    }

    #[tokio::test]
    async fn set_preserves_comments_and_validates() {
        let path = temp_path("set");
        std::fs::write(
            &path,
            "# my notes\n[runtime]\n# fast ticks\ntick_interval_ms = 300\n",
        )
        .unwrap();
        let handle = ConfigHandle::load(&path).unwrap();
        let keys = handle
            .set(&[
                ("runtime.tick_interval_ms".into(), serde_json::json!(900)),
                ("cognition.learning_rate".into(), serde_json::json!(0.5)),
            ])
            .await
            .unwrap();
        assert!(keys.contains(&"runtime.tick_interval_ms".to_string()));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my notes") && text.contains("# fast ticks"));
        assert_eq!(handle.get().cognition.learning_rate, 0.5);
        assert!(
            handle
                .set(&[("runtime.tick_interval_ms".into(), serde_json::json!(1))])
                .await
                .is_err()
        );
        assert!(
            handle
                .set(&[("nope".into(), serde_json::json!(1))])
                .await
                .is_err()
        );
        assert!(
            handle
                .set(&[("runtime.bogus".into(), serde_json::json!(1))])
                .await
                .is_err()
        );
        assert_eq!(handle.get().runtime.tick_interval_ms, 900);
    }

    #[test]
    fn cli_values_parse_as_toml_or_string() {
        assert_eq!(parse_cli_value("3"), serde_json::json!(3));
        assert_eq!(parse_cli_value("0.5"), serde_json::json!(0.5));
        assert_eq!(parse_cli_value("true"), serde_json::json!(true));
        assert_eq!(parse_cli_value("data/x"), serde_json::json!("data/x"));
        assert_eq!(
            parse_cli_value("[\"levels\"]"),
            serde_json::json!(["levels"])
        );
    }

    #[test]
    fn restart_required_keys_are_flagged() {
        let keys = vec!["database.pool_size".into(), "runtime.paused".into()];
        assert_eq!(restart_required(&keys), ["database.pool_size"]);
    }
}
