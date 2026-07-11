//! Persistenz der Anmeldedaten. Wir speichern bewusst genau das vom Nutzer
//! bereitgestellte Credential (etp-rt Cookie oder Refresh-Token) unverändert ab,
//! da diese langlebig sind. Der Server meldet sich damit bei jedem Start neu an.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Welche Art von Token gespeichert ist.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    /// `etp-rt` Cookie aus dem Browser (empfohlen).
    EtpRt,
    /// Refresh-Token (z. B. aus einem vorherigen Credentials-Login).
    RefreshToken,
}

/// Auf Platte gespeicherte Session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredSession {
    pub kind: TokenKind,
    pub token: String,
}

/// Standard-Speicherort: `~/.crunchyroll-mcp/session.json`.
pub fn default_path() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".crunchyroll-mcp").join("session.json")
}

pub fn load(path: &Path) -> Option<StoredSession> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save(path: &Path, session: &StoredSession) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(session)?;
    std::fs::write(path, data)
}

pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}
