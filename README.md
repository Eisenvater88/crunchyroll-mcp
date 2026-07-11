# crunchyroll-mcp

Ein MCP-Server (Model Context Protocol) für Crunchyroll, geschrieben in Rust auf Basis
der aktiv gepflegten Bibliothek [`crunchyroll-rs`](https://github.com/crunchy-labs/crunchyroll-rs).

Der Server stellt Suche, Katalog-Browsing, Staffel-/Episoden-Auflistung und die
persönliche Watchlist als MCP-Tools bereit. Die Anmeldung erfolgt über den
`etp-rt`-Cookie aus dem Browser (empfohlen) oder einen Refresh-Token; die Session wird
gespeichert, sodass nach dem ersten Login jeder Neustart automatisch angemeldet ist.

## Warum Rust / crunchyroll-rs?

Crunchyroll hat eine undokumentierte API und schützt den Auth-Endpunkt mit Cloudflare.
`crunchyroll-rs` wird genau dafür gepflegt (u. a. holt es vor dem Login die nötigen
Cloudflare-Cookies von der Startseite und bringt einen passenden User-Agent mit). Damit
erben wir die Auth-Pflege, statt sie selbst dauerhaft nachzuziehen.

## Bauen

Voraussetzung: eine Rust-Toolchain (`rustup`, hier gebaut mit Rust 1.95).

```sh
cargo build --release
```

Das Binary liegt danach unter `target/release/crunchyroll-mcp.exe` (Windows) bzw.
`target/release/crunchyroll-mcp`.

## In Claude Code / Claude Desktop registrieren

**Claude Code (CLI):**

```sh
claude mcp add crunchyroll -- "D:\\crunchroll_mcp\\target\\release\\crunchyroll-mcp.exe"
```

**Claude Desktop / andere Clients** — Eintrag in der MCP-Server-Konfiguration:

```json
{
  "mcpServers": {
    "crunchyroll": {
      "command": "D:\\crunchroll_mcp\\target\\release\\crunchyroll-mcp.exe"
    }
  }
}
```

## Anmelden

1. Auf [crunchyroll.com](https://www.crunchyroll.com) im Browser einloggen.
2. Entwicklertools öffnen (F12) → **Application/Storage** → **Cookies** →
   `https://www.crunchyroll.com` → den Wert des Cookies **`etp_rt`** kopieren.
3. Das Tool **`crunchyroll_login`** mit diesem Wert aufrufen.

Die Session wird unter `~/.crunchyroll-mcp/session.json` gespeichert. Zum Abmelden das
Tool `crunchyroll_logout` verwenden.

> **Hinweis zu Cloudflare/VPN:** Crunchyrolls Auth-Endpunkt ist Cloudflare-geschützt und
> blockiert häufig VPN-IPs. Falls der Login mit einem Cloudflare-/403-Fehler scheitert,
> das VPN für die erste Anmeldung deaktivieren oder einen anderen Ausgangsserver wählen.

## Tools

| Tool | Beschreibung |
|------|--------------|
| `crunchyroll_login` | Anmelden mit dem `etp-rt`-Cookie |
| `crunchyroll_login_with_refresh_token` | Anmelden mit einem Refresh-Token |
| `crunchyroll_status` | Anmeldestatus / Premium prüfen |
| `crunchyroll_logout` | Abmelden, Session löschen |
| `crunchyroll_search` | Katalog nach Suchbegriff durchsuchen (`query`, `limit?`) |
| `crunchyroll_browse` | Katalog durchstöbern (`limit?`) |
| `crunchyroll_seasons` | Staffeln einer Serie auflisten (`id` = Serien-ID) |
| `crunchyroll_episodes` | Episoden einer Staffel auflisten (`id` = Staffel-ID) |
| `crunchyroll_watchlist` | Persönliche Watchlist abrufen (`limit?`) |
| `crunchyroll_add_to_watchlist` | Serie/Movie-Listing zur Watchlist hinzufügen (`id`) |
| `crunchyroll_remove_from_watchlist` | Serie/Movie-Listing von der Watchlist entfernen (`id`) |
| `crunchyroll_watch_history` | Wiedergabe-History abrufen, inkl. Playhead (`limit?`) |
| `crunchyroll_update_playhead` | „Weiterschauen"-Position setzen (`id`, `position_seconds`) |

Typischer Ablauf: `crunchyroll_search` → aus dem Ergebnis die Serien-ID nehmen →
`crunchyroll_seasons` → Staffel-ID → `crunchyroll_episodes`.

## Lizenz / Hinweis

Inoffiziell, nicht mit Crunchyroll affiliiert. Nutzung auf eigene Verantwortung im Rahmen
der Crunchyroll-Nutzungsbedingungen.
