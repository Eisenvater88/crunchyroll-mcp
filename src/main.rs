//! MCP-Server für Crunchyroll, aufgebaut auf `crunchyroll-rs`.
//!
//! Anmeldung über den `etp-rt`-Cookie aus dem Browser (oder einen Refresh-Token).
//! Die Session wird persistiert, sodass nach dem ersten Login jeder Neustart
//! automatisch angemeldet ist.

mod session;

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use crunchyroll_rs::common::Pagination;
use crunchyroll_rs::crunchyroll::DeviceIdentifier;
use crunchyroll_rs::release_calendar::ReleaseCalendarItem;
use crunchyroll_rs::search::{BrowseOptions, SearchMediaCollection};
use crunchyroll_rs::{Crunchyroll, MediaCollection};
use futures_util::StreamExt;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use session::{StoredSession, TokenKind};

// ---------------------------------------------------------------------------
// Server-Zustand
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CrunchyrollServer {
    inner: Arc<Mutex<Inner>>,
    // Wird vom `#[tool_handler]`-Makro zur Laufzeit genutzt; der Compiler sieht das
    // nicht und würde sonst fälschlich "never read" warnen.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

struct Inner {
    /// Zwischengespeicherte, angemeldete Instanz.
    client: Option<Crunchyroll>,
    /// Pfad zur Session-Datei.
    session_path: std::path::PathBuf,
}

fn cr_err(e: crunchyroll_rs::Error) -> McpError {
    McpError::internal_error(format!("Crunchyroll-API-Fehler: {e}"), None)
}

impl CrunchyrollServer {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                client: None,
                session_path: session::default_path(),
            })),
            tool_router: Self::tool_router(),
        }
    }

    /// Meldet sich mit einem gespeicherten oder frisch übergebenen Token an.
    async fn login_with(stored: &StoredSession) -> Result<Crunchyroll, McpError> {
        let device = DeviceIdentifier::default();
        let builder = Crunchyroll::builder();
        let result = match stored.kind {
            TokenKind::EtpRt => builder.login_with_etp_rt(&stored.token, device).await,
            TokenKind::RefreshToken => {
                builder
                    .login_with_refresh_token(&stored.token, device)
                    .await
            }
        };
        result.map_err(cr_err)
    }

    /// Liefert eine angemeldete Instanz oder einen sprechenden Fehler, wenn nicht
    /// angemeldet. Lädt bei Bedarf lazily aus der gespeicherten Session.
    async fn ensure_client(&self) -> Result<Crunchyroll, McpError> {
        let mut guard = self.inner.lock().await;
        if let Some(client) = &guard.client {
            return Ok(client.clone());
        }
        let Some(stored) = session::load(&guard.session_path) else {
            return Err(McpError::invalid_request(
                "Nicht angemeldet. Bitte zuerst das Tool 'crunchyroll_login' mit dem \
                 etp-rt-Cookie aus dem Browser aufrufen."
                    .to_string(),
                None,
            ));
        };
        let client = Self::login_with(&stored).await?;
        guard.client = Some(client.clone());
        Ok(client)
    }
}

// ---------------------------------------------------------------------------
// Hilfsfunktionen zur kompakten Ausgabe
// ---------------------------------------------------------------------------

fn truncate(s: &str) -> String {
    const MAX: usize = 240;
    if s.chars().count() > MAX {
        let mut out: String = s.chars().take(MAX).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// Verdichtet ein Medium auf die für einen Agenten relevanten Felder.
fn summarize(m: &MediaCollection) -> Value {
    match m {
        MediaCollection::Series(s) => json!({
            "type": "series",
            "id": s.id,
            "title": s.title,
            "episode_count": s.episode_count,
            "subbed": s.is_subbed,
            "dubbed": s.is_dubbed,
            "description": truncate(&s.description),
        }),
        MediaCollection::Season(s) => json!({
            "type": "season",
            "id": s.id,
            "series_id": s.series_id,
            "title": s.title,
            "season_number": s.season_number,
            "episodes": s.number_of_episodes,
            "subbed": s.is_subbed,
            "dubbed": s.is_dubbed,
        }),
        MediaCollection::Episode(e) => json!({
            "type": "episode",
            "id": e.id,
            "series_id": e.series_id,
            "series_title": e.series_title,
            "season_number": e.season_number,
            "episode_number": e.episode_number,
            "title": e.title,
            "premium_only": e.is_premium_only,
            "description": truncate(&e.description),
        }),
        MediaCollection::MovieListing(m) => json!({
            "type": "movie_listing",
            "id": m.id,
            "title": m.title,
            "subbed": m.is_subbed,
            "dubbed": m.is_dubbed,
            "description": truncate(&m.description),
        }),
        MediaCollection::Movie(m) => json!({
            "type": "movie",
            "id": m.id,
            "title": m.title,
            "description": truncate(&m.description),
        }),
        MediaCollection::MusicVideo(m) => json!({
            "type": "music_video",
            "id": m.id,
            "title": m.title,
        }),
        MediaCollection::Concert(c) => json!({
            "type": "concert",
            "id": c.id,
            "title": c.title,
        }),
        MediaCollection::Artist(a) => json!({
            "type": "artist",
            "id": a.id,
            "name": a.name,
        }),
    }
}

/// Sammelt bis zu `limit` Einträge aus einer Medien-Pagination.
async fn collect_media(
    mut pag: Pagination<SearchMediaCollection>,
    limit: usize,
) -> Result<Vec<Value>, McpError> {
    let mut out = Vec::new();
    while let Some(item) = pag.next().await {
        let item = item.map_err(cr_err)?;
        let mc: MediaCollection = item.into();
        out.push(summarize(&mc));
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

fn ok_json(value: Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|e| format!("{{\"error\":\"serialize: {e}\"}}"));
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

// ---------------------------------------------------------------------------
// Tool-Parameter
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LoginParams {
    /// Der `etp-rt`-Cookie-Wert aus einer im Browser angemeldeten Crunchyroll-Session.
    etp_rt: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RefreshLoginParams {
    /// Ein Crunchyroll-Refresh-Token.
    refresh_token: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchParams {
    /// Suchbegriff (Titel, Stichwort …).
    query: String,
    /// Maximale Anzahl Ergebnisse (Standard 10).
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BrowseParams {
    /// Maximale Anzahl Ergebnisse (Standard 20).
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct IdParams {
    /// Die Crunchyroll-ID (Serie bzw. Staffel).
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WatchlistParams {
    /// Maximale Anzahl Einträge (Standard 20).
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WatchHistoryParams {
    /// Maximale Anzahl Einträge (Standard 20).
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PlayheadParams {
    /// Die ID der Episode oder des Films.
    id: String,
    /// Neue Wiedergabeposition in Sekunden.
    position_seconds: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReleaseCalendarParams {
    /// Ein Datum (YYYY-MM-DD) innerhalb der gewünschten Woche. Standard: aktuelle Woche.
    #[serde(default)]
    date: Option<String>,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl CrunchyrollServer {
    #[tool(
        description = "Bei Crunchyroll anmelden mit dem etp-rt-Cookie aus dem Browser. \
        So findest du ihn: auf crunchyroll.com einloggen, Entwicklertools öffnen (F12) → \
        Application/Storage → Cookies → www.crunchyroll.com → Wert von 'etp_rt' kopieren. \
        Die Session wird gespeichert; künftige Starts melden sich automatisch an."
    )]
    async fn crunchyroll_login(
        &self,
        Parameters(LoginParams { etp_rt }): Parameters<LoginParams>,
    ) -> Result<CallToolResult, McpError> {
        let stored = StoredSession {
            kind: TokenKind::EtpRt,
            token: etp_rt.trim().to_string(),
        };
        let client = Self::login_with(&stored).await?;
        let premium = client.premium().await;

        let mut guard = self.inner.lock().await;
        session::save(&guard.session_path, &stored).map_err(|e| {
            McpError::internal_error(format!("Session speichern fehlgeschlagen: {e}"), None)
        })?;
        guard.client = Some(client);

        ok_json(json!({
            "logged_in": true,
            "premium": premium,
            "message": "Erfolgreich angemeldet. Session gespeichert."
        }))
    }

    #[tool(
        description = "Bei Crunchyroll mit einem Refresh-Token anmelden (Alternative zu etp-rt). \
        Die Session wird gespeichert."
    )]
    async fn crunchyroll_login_with_refresh_token(
        &self,
        Parameters(RefreshLoginParams { refresh_token }): Parameters<RefreshLoginParams>,
    ) -> Result<CallToolResult, McpError> {
        let stored = StoredSession {
            kind: TokenKind::RefreshToken,
            token: refresh_token.trim().to_string(),
        };
        let client = Self::login_with(&stored).await?;
        let premium = client.premium().await;

        let mut guard = self.inner.lock().await;
        session::save(&guard.session_path, &stored).map_err(|e| {
            McpError::internal_error(format!("Session speichern fehlgeschlagen: {e}"), None)
        })?;
        guard.client = Some(client);

        ok_json(json!({ "logged_in": true, "premium": premium }))
    }

    #[tool(description = "Anmeldestatus prüfen (angemeldet? Premium?).")]
    async fn crunchyroll_status(&self) -> Result<CallToolResult, McpError> {
        let has_session = {
            let guard = self.inner.lock().await;
            guard.client.is_some() || session::load(&guard.session_path).is_some()
        };
        if !has_session {
            return ok_json(json!({ "logged_in": false }));
        }
        match self.ensure_client().await {
            Ok(client) => ok_json(json!({
                "logged_in": true,
                "premium": client.premium().await,
            })),
            Err(_) => ok_json(json!({
                "logged_in": false,
                "note": "Gespeicherte Session konnte nicht erneuert werden (evtl. abgelaufen oder \
                         Cloudflare-Block). Bitte neu anmelden."
            })),
        }
    }

    #[tool(description = "Abmelden und die gespeicherte Session löschen.")]
    async fn crunchyroll_logout(&self) -> Result<CallToolResult, McpError> {
        let mut guard = self.inner.lock().await;
        session::clear(&guard.session_path);
        guard.client = None;
        ok_json(json!({ "logged_in": false, "message": "Abgemeldet." }))
    }

    #[tool(description = "Den Crunchyroll-Katalog nach einem Suchbegriff durchsuchen.")]
    async fn crunchyroll_search(
        &self,
        Parameters(SearchParams { query, limit }): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let limit = limit.unwrap_or(10).clamp(1, 50) as usize;
        let results = collect_media(client.query(&query).top_results, limit).await?;
        ok_json(json!({ "query": query, "count": results.len(), "results": results }))
    }

    #[tool(
        description = "Den Crunchyroll-Katalog durchstöbern (neu hinzugefügte Serien und Filme)."
    )]
    async fn crunchyroll_browse(
        &self,
        Parameters(BrowseParams { limit }): Parameters<BrowseParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let limit = limit.unwrap_or(20).clamp(1, 50) as usize;
        let results = collect_media(client.browse(BrowseOptions::default()), limit).await?;
        ok_json(json!({ "count": results.len(), "results": results }))
    }

    #[tool(description = "Die Staffeln einer Serie anhand ihrer Serien-ID auflisten.")]
    async fn crunchyroll_seasons(
        &self,
        Parameters(IdParams { id }): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let media = client.media_collection_from_id(&id).await.map_err(cr_err)?;
        let MediaCollection::Series(series) = media else {
            return Err(McpError::invalid_params(
                format!("ID '{id}' ist keine Serie."),
                None,
            ));
        };
        let seasons = series.seasons().await.map_err(cr_err)?;
        let list: Vec<Value> = seasons
            .iter()
            .map(|s| summarize(&MediaCollection::Season(s.clone())))
            .collect();
        ok_json(
            json!({ "series_id": id, "series_title": series.title, "count": list.len(), "seasons": list }),
        )
    }

    #[tool(description = "Die Episoden einer Staffel anhand ihrer Staffel-ID auflisten.")]
    async fn crunchyroll_episodes(
        &self,
        Parameters(IdParams { id }): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let media = client.media_collection_from_id(&id).await.map_err(cr_err)?;
        let MediaCollection::Season(season) = media else {
            return Err(McpError::invalid_params(
                format!("ID '{id}' ist keine Staffel."),
                None,
            ));
        };
        let episodes = season.episodes().await.map_err(cr_err)?;
        let list: Vec<Value> = episodes
            .iter()
            .map(|e| summarize(&MediaCollection::Episode(e.clone())))
            .collect();
        ok_json(json!({ "season_id": id, "count": list.len(), "episodes": list }))
    }

    #[tool(description = "Die persönliche Watchlist des angemeldeten Kontos abrufen.")]
    async fn crunchyroll_watchlist(
        &self,
        Parameters(WatchlistParams { limit }): Parameters<WatchlistParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let limit = limit.unwrap_or(20).clamp(1, 100) as usize;
        let entries = client
            .watchlist(crunchyroll_rs::list::WatchlistOptions::default())
            .await
            .map_err(cr_err)?;
        let list: Vec<Value> = entries
            .iter()
            .take(limit)
            .map(|entry| {
                let mut v = summarize(&entry.panel);
                if let Value::Object(map) = &mut v {
                    map.insert("fully_watched".into(), json!(entry.fully_watched));
                    map.insert("never_watched".into(), json!(entry.never_watched));
                    map.insert("is_favorite".into(), json!(entry.is_favorite));
                }
                v
            })
            .collect();
        ok_json(json!({ "count": list.len(), "watchlist": list }))
    }

    #[tool(
        description = "Die Wiedergabe-History des angemeldeten Kontos abrufen (zuletzt gesehene \
        Episoden/Filme, inkl. Playhead-Position und ob fertig geschaut)."
    )]
    async fn crunchyroll_watch_history(
        &self,
        Parameters(WatchHistoryParams { limit }): Parameters<WatchHistoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let limit = limit.unwrap_or(20).clamp(1, 100) as usize;
        let mut pag = client.watch_history();
        let mut list = Vec::new();
        while let Some(item) = pag.next().await {
            let entry = item.map_err(cr_err)?;
            let mut v = match &entry.panel {
                Some(panel) => summarize(panel),
                None => json!({ "type": "unknown", "id": entry.id }),
            };
            if let Value::Object(map) = &mut v {
                map.insert("playhead_seconds".into(), json!(entry.playhead));
                map.insert("fully_watched".into(), json!(entry.fully_watched));
                map.insert("date_played".into(), json!(entry.date_played.to_rfc3339()));
            }
            list.push(v);
            if list.len() >= limit {
                break;
            }
        }
        ok_json(json!({ "count": list.len(), "history": list }))
    }

    #[tool(
        description = "Eine Serie oder ein Movie-Listing zur Watchlist hinzufügen. \
        'id' ist die Serien- bzw. Movie-Listing-ID (z. B. aus crunchyroll_search)."
    )]
    async fn crunchyroll_add_to_watchlist(
        &self,
        Parameters(IdParams { id }): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let media = client.media_collection_from_id(&id).await.map_err(cr_err)?;
        let title = match media {
            MediaCollection::Series(series) => {
                series.add_to_watchlist().await.map_err(cr_err)?;
                series.title.clone()
            }
            MediaCollection::MovieListing(movie_listing) => {
                movie_listing.add_to_watchlist().await.map_err(cr_err)?;
                movie_listing.title.clone()
            }
            _ => {
                return Err(McpError::invalid_params(
                    format!("ID '{id}' ist keine Serie und kein Movie-Listing."),
                    None,
                ));
            }
        };
        ok_json(json!({
            "added": true,
            "id": id,
            "title": title,
            "message": format!("'{title}' zur Watchlist hinzugefügt.")
        }))
    }

    #[tool(
        description = "Eine Serie oder ein Movie-Listing von der Watchlist entfernen. \
        'id' ist die Serien- bzw. Movie-Listing-ID."
    )]
    async fn crunchyroll_remove_from_watchlist(
        &self,
        Parameters(IdParams { id }): Parameters<IdParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let media = client.media_collection_from_id(&id).await.map_err(cr_err)?;
        let (title, was_present) = match media {
            MediaCollection::Series(series) => {
                let title = series.title.clone();
                match series.into_watchlist_entry().await.map_err(cr_err)? {
                    Some(entry) => {
                        entry.remove().await.map_err(cr_err)?;
                        (title, true)
                    }
                    None => (title, false),
                }
            }
            MediaCollection::MovieListing(movie_listing) => {
                let title = movie_listing.title.clone();
                match movie_listing.into_watchlist_entry().await.map_err(cr_err)? {
                    Some(entry) => {
                        entry.remove().await.map_err(cr_err)?;
                        (title, true)
                    }
                    None => (title, false),
                }
            }
            _ => {
                return Err(McpError::invalid_params(
                    format!("ID '{id}' ist keine Serie und kein Movie-Listing."),
                    None,
                ));
            }
        };
        ok_json(json!({
            "removed": was_present,
            "id": id,
            "title": title,
            "message": if was_present {
                format!("'{title}' von der Watchlist entfernt.")
            } else {
                format!("'{title}' war nicht auf der Watchlist.")
            }
        }))
    }

    #[tool(
        description = "Die Wiedergabeposition (Playhead) einer Episode oder eines Films setzen, \
        z. B. um den 'Weiterschauen'-Fortschritt zu aktualisieren. 'position_seconds' ist die \
        Position in Sekunden ab Videobeginn."
    )]
    async fn crunchyroll_update_playhead(
        &self,
        Parameters(PlayheadParams {
            id,
            position_seconds,
        }): Parameters<PlayheadParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let media = client.media_collection_from_id(&id).await.map_err(cr_err)?;
        let title = match media {
            MediaCollection::Episode(episode) => {
                episode
                    .set_playhead(position_seconds)
                    .await
                    .map_err(cr_err)?;
                episode.title.clone()
            }
            MediaCollection::Movie(movie) => {
                movie.set_playhead(position_seconds).await.map_err(cr_err)?;
                movie.title.clone()
            }
            _ => {
                return Err(McpError::invalid_params(
                    format!("ID '{id}' ist keine Episode und kein Film."),
                    None,
                ));
            }
        };
        ok_json(json!({
            "updated": true,
            "id": id,
            "title": title,
            "playhead_seconds": position_seconds,
        }))
    }

    #[tool(
        description = "Den Crunchyroll-Release-Kalender (Simulcast) für die Woche eines Datums \
        abrufen, gruppiert nach Wochentag. 'date' optional als YYYY-MM-DD; Standard ist die \
        aktuelle Woche. Hinweis: Crunchyroll liefert meist nur Releases bis zum heutigen Tag."
    )]
    async fn crunchyroll_release_calendar(
        &self,
        Parameters(ReleaseCalendarParams { date }): Parameters<ReleaseCalendarParams>,
    ) -> Result<CallToolResult, McpError> {
        let client = self.ensure_client().await?;
        let when = match date {
            Some(s) => {
                let nd = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| {
                    McpError::invalid_params(
                        format!("Ungültiges Datum '{s}', erwartet Format YYYY-MM-DD."),
                        None,
                    )
                })?;
                nd.and_hms_opt(12, 0, 0).unwrap().and_utc()
            }
            None => Utc::now(),
        };

        let week = client.release_calendar(when).await.map_err(cr_err)?;

        let day = |items: &[ReleaseCalendarItem]| -> Vec<Value> {
            items
                .iter()
                .map(|it| {
                    json!({
                        "series_id": it.series_id,
                        "episode_id": it.episode_id,
                        "series_title": it.season_title,
                        "episode_title": it.episode_title,
                        "episode_number": it.episode_number,
                        "release_time": it.release_time.to_rfc3339(),
                        "premium": it.premium,
                    })
                })
                .collect()
        };

        ok_json(json!({
            "week_of": when.format("%Y-%m-%d").to_string(),
            "monday": day(&week.monday),
            "tuesday": day(&week.tuesday),
            "wednesday": day(&week.wednesday),
            "thursday": day(&week.thursday),
            "friday": day(&week.friday),
            "saturday": day(&week.saturday),
            "sunday": day(&week.sunday),
        }))
    }
}

#[tool_handler]
impl ServerHandler for CrunchyrollServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Crunchyroll-MCP: Suche, Katalog, Staffeln/Episoden und Watchlist. \
             Zuerst mit 'crunchyroll_login' (etp-rt-Cookie) anmelden."
                .to_string(),
        );
        info
    }
}

// ---------------------------------------------------------------------------
// Einstiegspunkt
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs auf stderr, damit der stdio-Transport (stdout) sauber bleibt.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let service = CrunchyrollServer::new()
        .serve(rmcp::transport::io::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
