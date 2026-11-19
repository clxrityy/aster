//! Append-only JSONL mirrors: `<logs.dir>/<category>/YYYY-MM-DD.jsonl`.
//! PostgreSQL stays the source of truth; a failed log write is reported but
//! never blocks or undoes a database operation.

use std::path::{Path, PathBuf};

use chrono::{NaiveDate, Utc};
use tokio::{
    io::AsyncWriteExt,
    sync::{broadcast, watch},
};

use crate::{
    config::{ConfigHandle, LOG_CATEGORIES},
    events::Envelope,
};

pub fn log_file(dir: &Path, category: &str, date: NaiveDate) -> PathBuf {
    dir.join(category)
        .join(format!("{}.jsonl", date.format("%Y-%m-%d")))
}

pub async fn append(dir: &Path, envelope: &Envelope) -> std::io::Result<()> {
    let path = log_file(dir, envelope.event.category(), envelope.at.date_naive());
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut line = serde_json::to_string(envelope).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    file.flush().await
}

/// Delete daily files older than `retention_days` (0 keeps everything).
pub async fn prune(dir: &Path, retention_days: u32) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let cutoff = Utc::now().date_naive() - chrono::Days::new(u64::from(retention_days));
    let mut removed = 0;
    for category in LOG_CATEGORIES {
        let Ok(mut entries) = tokio::fs::read_dir(dir.join(category)).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".jsonl")) else {
                continue;
            };
            if let Ok(date) = NaiveDate::parse_from_str(stem, "%Y-%m-%d")
                && date < cutoff
                && tokio::fs::remove_file(entry.path()).await.is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

pub fn spawn_writer(
    config: ConfigHandle,
    mut events: broadcast::Receiver<Envelope>,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut prune_timer = tokio::time::interval(std::time::Duration::from_secs(3600));
        let mut warned = false;
        loop {
            let envelope = tokio::select! {
                received = events.recv() => match received {
                    Ok(envelope) => envelope,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "log mirror lagged; some events were not written");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = prune_timer.tick() => {
                    let logs = config.get().logs.clone();
                    let removed = prune(Path::new(&logs.dir), logs.retention_days).await;
                    if removed > 0 {
                        tracing::info!(removed, "pruned old log files");
                    }
                    continue;
                }
                _ = stop.changed() => {
                    // Drain anything already queued, then stop.
                    while let Ok(envelope) = events.try_recv() {
                        write(&config, &envelope, &mut warned).await;
                    }
                    return;
                }
            };
            write(&config, &envelope, &mut warned).await;
        }
    })
}

async fn write(config: &ConfigHandle, envelope: &Envelope, warned: &mut bool) {
    let logs = config.get().logs.clone();
    if !logs.enabled.iter().any(|c| c == envelope.event.category()) {
        return;
    }
    match append(Path::new(&logs.dir), envelope).await {
        Ok(()) => *warned = false,
        Err(error) if !*warned => {
            *warned = true;
            tracing::warn!(error = %error.kind(), "unable to write log mirror");
        }
        Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;

    #[tokio::test]
    async fn appends_jsonl_per_category_and_prunes() {
        let dir = std::env::temp_dir().join(format!("aster-logs-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let envelope = Envelope::now(Event::Runtime {
            message: "started".into(),
            paused: false,
        });
        append(&dir, &envelope).await.unwrap();
        append(&dir, &envelope).await.unwrap();
        let path = log_file(&dir, "runtime", envelope.at.date_naive());
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(text.lines().count(), 2);
        let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["type"], "runtime");
        let old = log_file(
            &dir,
            "runtime",
            NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
        );
        tokio::fs::write(&old, "{}\n").await.unwrap();
        assert_eq!(prune(&dir, 30).await, 1);
        assert!(path.exists());
    }
}
