// Ported from aionrs (Apache-2.0).
//   Source: crates/aion-agent/src/vcr.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 落在 dev-adapter 而不是 testkit——它要读写磁盘，而 testkit 受
//            `check-no-env.sh` 管；`anyhow` 换成 `std::io::Result`/String，
//            `tracing` 去掉（本 crate 不持有日志设施）。

#![allow(missing_docs, reason = "aionrs 逐字移植，文档待二次优化补齐")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// A recorded set of HTTP interactions
#[derive(Debug, Serialize, Deserialize)]
pub struct Cassette {
    pub name: String,
    pub recorded_at: String,
    pub interactions: Vec<Interaction>,
}

/// A single request-response pair
#[derive(Debug, Serialize, Deserialize)]
pub struct Interaction {
    pub request: RecordedRequest,
    pub response: RecordedResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    /// Body stored as string (may be SSE event stream)
    pub body: String,
}

/// VCR operating mode
pub enum VcrMode {
    /// Normal operation, no VCR
    Off,
    /// Record interactions to cassette file
    Record(PathBuf),
    /// Replay from cassette file (no network)
    Replay(PathBuf),
}

/// VCR layer that intercepts HTTP interactions for recording/replay
pub struct VcrLayer {
    mode: VcrMode,
    cassette: Mutex<Cassette>,
    replay_index: Mutex<usize>,
}

impl VcrLayer {
    /// Create a VCR layer from environment variables
    pub fn from_env() -> Option<Self> {
        let mode = std::env::var("VCR_MODE").ok()?;
        let cassette_path = std::env::var("VCR_CASSETTE").ok()?;
        let path = PathBuf::from(&cassette_path);

        match mode.as_str() {
            "record" => Some(Self::record(path, None)),
            "replay" => Self::replay(path).ok(),
            _ => None,
        }
    }

    /// Create a recording VCR layer
    ///
    /// `recorded_at` 由调用方给（原文用 `chrono::Utc::now()`）：回放同一份
    /// 录像要能重现同一串时刻，自己取墙钟就做不到。
    pub fn record(path: PathBuf, recorded_at: Option<String>) -> Self {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();

        Self {
            mode: VcrMode::Record(path),
            cassette: Mutex::new(Cassette {
                name,
                // 原文用 chrono 取墙钟。本 crate 是宿主参考实现，可以碰时钟，
                // 但录制时刻由调用方给更好——回放同一份录像要能重现同一串时刻。
                recorded_at: recorded_at.unwrap_or_default(),
                interactions: Vec::new(),
            }),
            replay_index: Mutex::new(0),
        }
    }

    /// Create a replay VCR layer from a cassette file
    pub fn replay(path: PathBuf) -> std::io::Result<Self> {
        let cassette = load_cassette(&path)?;
        Ok(Self {
            mode: VcrMode::Replay(path),
            cassette: Mutex::new(cassette),
            replay_index: Mutex::new(0),
        })
    }

    /// Check if this VCR layer is in replay mode
    pub fn is_replay(&self) -> bool {
        matches!(self.mode, VcrMode::Replay(_))
    }

    /// Record an interaction (only in record mode)
    #[allow(clippy::too_many_arguments)]
    pub fn record_interaction(
        &self,
        method: &str,
        url: &str,
        request_headers: &HashMap<String, String>,
        request_body: serde_json::Value,
        status: u16,
        response_headers: &HashMap<String, String>,
        response_body: &str,
    ) {
        if let VcrMode::Record(_) = &self.mode {
            let interaction = Interaction {
                request: RecordedRequest {
                    method: method.to_string(),
                    url: url.to_string(),
                    headers: sanitize_headers(request_headers),
                    body: request_body,
                },
                response: RecordedResponse {
                    status,
                    headers: response_headers.clone(),
                    body: response_body.to_string(),
                },
            };
            if let Ok(mut cassette) = self.cassette.lock() {
                cassette.interactions.push(interaction);
            }
        }
    }

    /// Get the next replay response (only in replay mode)
    pub fn next_replay(&self) -> Option<&RecordedResponse> {
        // We need to work around Mutex not allowing returning references.
        // Instead, use get_replay_response which returns owned data.
        None
    }

    /// Get the next replay response as owned data
    pub fn get_replay_response(&self) -> Option<(u16, HashMap<String, String>, String)> {
        if let VcrMode::Replay(_) = &self.mode {
            let mut index = self.replay_index.lock().ok()?;
            let cassette = self.cassette.lock().ok()?;

            if *index < cassette.interactions.len() {
                let interaction = &cassette.interactions[*index];
                *index += 1;
                Some((
                    interaction.response.status,
                    interaction.response.headers.clone(),
                    interaction.response.body.clone(),
                ))
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Save the cassette to disk (only in record mode)
    pub fn save(&self) -> std::io::Result<()> {
        if let VcrMode::Record(path) = &self.mode {
            let cassette = self.cassette.lock().map_err(|e| std::io::Error::other(format!("lock poisoned: {e}")))?;

            if cassette.interactions.is_empty() {
                return Ok(()); // nothing to save
            }

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let json = serde_json::to_string_pretty(&*cassette)?;
            std::fs::write(path, json)?;
        }
        Ok(())
    }
}

impl Drop for VcrLayer {
    fn drop(&mut self) {
        // 原文在这里记一条 warning。本 crate 不持有日志设施，而析构里
        // 也没有可以报错的去处——录像存不下就是存不下，不该因此 panic。
        let _ = self.save();
    }
}

/// Load a cassette from disk
fn load_cassette(path: &Path) -> std::io::Result<Cassette> {
    let content = std::fs::read_to_string(path)?;
    let cassette: Cassette = serde_json::from_str(&content)?;
    Ok(cassette)
}

/// Remove sensitive headers from recorded requests
fn sanitize_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            let sanitized_value = if k.to_lowercase().contains("key")
                || k.to_lowercase().contains("auth")
                || k.to_lowercase().contains("token")
            {
                "[REDACTED]".to_string()
            } else {
                v.clone()
            };
            (k.clone(), sanitized_value)
        })
        .collect()
}
