use std::collections::HashMap;
use std::path::Path;

use crate::docker::Docker;
use crate::error::Result;

/// Hur många loggrader en rapport får bära (masterns `LOG_WINDOW`).
const LOG_WINDOW: usize = 400;
const MAX_LOG_CHARS: usize = 8192;

/// Telemetri per server: status, containerdata, spelare och nya
/// konsolrader. Loggkursorn hålls i minnet, så en omstart skickar en
/// fönsterbacklog en gång och därefter bara nya rader.
pub struct Reporter {
    cursors: HashMap<String, String>,
    players: HashMap<String, i64>,
}

pub struct Sample {
    pub status: &'static str,
    pub cpu_percent: Option<f64>,
    pub memory_used_mb: Option<u64>,
    pub memory_limit_mb: Option<u64>,
    pub players_online: Option<i64>,
    pub players_max: Option<i64>,
    pub logs: Vec<(String, String)>,
}

impl Reporter {
    pub fn new() -> Self {
        Self {
            cursors: HashMap::new(),
            players: HashMap::new(),
        }
    }

    pub async fn sample(
        &mut self,
        docker: &Docker,
        data_root: &Path,
        server_id: &str,
    ) -> Result<Sample> {
        let state = docker.state(server_id).await?;
        let status = match &state {
            None => "OFFLINE",
            Some(state) if state.running => "ONLINE",
            Some(state) if state.exit_code == 0 => "OFFLINE",
            Some(_) => "CRASHED",
        };

        let stats = if matches!(status, "ONLINE") {
            docker.stats(server_id).await?
        } else {
            None
        };

        let since = self.cursors.get(server_id).cloned();
        let lines = docker.logs_since(server_id, since.as_deref()).await?;
        let mut logs = Vec::new();
        for (ts, text) in lines {
            self.cursors.insert(server_id.to_string(), ts.clone());
            let text = sanitize(&text);
            if text.is_empty() {
                continue;
            }
            self.apply_player_event(server_id, &text);
            logs.push((ts, text));
        }
        if logs.len() > LOG_WINDOW {
            logs.drain(..logs.len() - LOG_WINDOW);
        }

        Ok(Sample {
            status,
            cpu_percent: stats.as_ref().and_then(|stats| stats.cpu_percent),
            memory_used_mb: stats.as_ref().and_then(|stats| stats.memory_used_mb),
            memory_limit_mb: stats.as_ref().and_then(|stats| stats.memory_limit_mb),
            players_online: self.players.get(server_id).copied(),
            players_max: max_players(data_root, server_id),
            logs,
        })
    }

    /// Spelarantalet räknas ur anslutningsraderna i den nya loggsträngen.
    /// Första gången en server ses är fönstret hela referensen.
    fn apply_player_event(&mut self, server_id: &str, text: &str) {
        let joined = text.contains("joined the game");
        let left = text.contains("left the game");
        if !joined && !left {
            return;
        }
        let count = self.players.entry(server_id.to_string()).or_insert(0);
        if joined {
            *count += 1;
        } else {
            *count = (*count - 1).max(0);
        }
    }
}

impl Default for Reporter {
    fn default() -> Self {
        Self::new()
    }
}

/// `max-players` ur serverns `server.properties` (samma standard som
/// Minecraft: 20 när raden saknas).
fn max_players(data_root: &Path, server_id: &str) -> Option<i64> {
    let path = data_root.join(server_id).join("server.properties");
    let Ok(contents) = std::fs::read_to_string(path) else {
        return None;
    };
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "max-players" {
            return value.trim().parse::<i64>().ok().filter(|value| *value >= 0);
        }
    }
    Some(20)
}

fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(MAX_LOG_CHARS)
        .collect();
    cleaned.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_players_from_join_and_leave_lines() {
        let mut reporter = Reporter::new();
        reporter.apply_player_event("srv", "Steve joined the game");
        reporter.apply_player_event("srv", "Alex joined the game");
        reporter.apply_player_event("srv", "Steve left the game");
        reporter.apply_player_event("srv", "Alex left the game");
        reporter.apply_player_event("srv", "Alex left the game");
        assert_eq!(reporter.players.get("srv").copied(), Some(0));
    }

    #[test]
    fn ignores_unrelated_lines() {
        let mut reporter = Reporter::new();
        reporter.apply_player_event("srv", "Starting minecraft server version 1.21");
        assert_eq!(reporter.players.get("srv").copied(), None);
    }

    #[test]
    fn sanitizes_control_characters_and_caps_length() {
        assert_eq!(sanitize("hello\u{0}world\t"), "hello world");
        assert_eq!(
            sanitize(&"a".repeat(MAX_LOG_CHARS + 10)).chars().count(),
            MAX_LOG_CHARS
        );
    }

    #[test]
    fn reads_max_players_from_the_server_properties() {
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-telemetry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(dir.join("srv")).unwrap();
        std::fs::write(
            dir.join("srv/server.properties"),
            "motd=hej\nmax-players=50\n",
        )
        .unwrap();
        assert_eq!(max_players(&dir, "srv"), Some(50));
        std::fs::write(dir.join("srv/server.properties"), "motd=hej\n").unwrap();
        assert_eq!(max_players(&dir, "srv"), Some(20));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_server_properties_yields_no_player_maximum() {
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-telemetry-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(max_players(&dir, "srv"), None);
    }
}
