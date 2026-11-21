use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::docker::Docker;
use crate::error::Result;

/// Hur många loggrader en rapport får bära (masterns `LOG_WINDOW`).
const LOG_WINDOW: usize = 400;
const MAX_LOG_CHARS: usize = 8192;
/// En rekursiv genomgång av en stor värld får inte köras varje rapport.
const DISK_CACHE_SECONDS: u64 = 300;

/// Masterns artefaktkrav för en server (`artifacts` i `/servers`).
pub struct Artifact<'a> {
    pub jar: &'a str,
    pub min_bytes: u64,
}

/// Telemetri per server: status, containerdata, spelare och nya
/// konsolrader. Loggkursorn ägs av anroparen (state-filen), så en omstart
/// fortsätter där agenten slutade i stället för att skicka om backloggen.
pub struct Reporter {
    players: HashMap<String, i64>,
    disk: HashMap<String, (Instant, u64)>,
}

pub struct Sample {
    pub status: &'static str,
    pub cpu_percent: Option<f64>,
    pub memory_used_mb: Option<u64>,
    pub memory_limit_mb: Option<u64>,
    /// Sekunder sedan containern startade (endast ONLINE).
    pub uptime_seconds: Option<u64>,
    pub players_online: Option<i64>,
    pub players_max: Option<i64>,
    pub disk_used_mb: Option<u64>,
    /// Kundens kumulativa nätverksräknare (2026-11-20); `None` när servern
    /// inte kör eller daemonen inte kunde svara.
    pub network_rx_bytes: Option<u64>,
    pub network_tx_bytes: Option<u64>,
    pub artifact_present: Option<bool>,
    pub logs: Vec<(String, String)>,
    /// Senaste sedda loggtidsstämpeln, att spara som nästa `since`.
    pub cursor: Option<String>,
}

impl Reporter {
    pub fn new() -> Self {
        Self {
            players: HashMap::new(),
            disk: HashMap::new(),
        }
    }

    pub async fn sample(
        &mut self,
        docker: &Docker,
        data_root: &Path,
        server_id: &str,
        since: Option<&str>,
        artifact: Option<Artifact<'_>>,
    ) -> Result<Sample> {
        let state = docker.state(server_id).await?;
        let status = match &state {
            None => "OFFLINE",
            Some(state) if state.running => "ONLINE",
            Some(state) if state.exit_code == 0 => "OFFLINE",
            Some(_) => "CRASHED",
        };

        let online = status == "ONLINE";
        let stats = if online {
            docker.stats(server_id).await?
        } else {
            None
        };
        // Räknarna läses bara medan containern kör: en stoppad container
        // svarar med nollor som ser ut som en mätning.
        let (network_rx_bytes, network_tx_bytes) = if online {
            docker.network_counters(server_id).await
        } else {
            (None, None)
        };
        let uptime_seconds = state
            .as_ref()
            .filter(|state| state.running)
            .and_then(|state| state.started_at_epoch)
            .and_then(|started| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs() as i64;
                u64::try_from(now - started).ok()
            });

        let server_root = data_root.join(server_id);
        let disk_used_mb = self.disk_usage(server_id, &server_root).await;
        let artifact_present = artifact.map(|artifact| has_artifact(&server_root, &artifact));

        let mut cursor = since.map(str::to_string);
        let lines = docker.logs_since(server_id, since).await?;
        let mut logs = Vec::new();
        for (ts, text) in lines {
            // Docker `--since` är inkluderande: gränsraden har redan skickats.
            if !is_new(cursor.as_deref(), &ts) {
                continue;
            }
            cursor = Some(ts.clone());
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

        // Server-list-ping är källan när servern svarar: konsolraderna
        // räknar bara vanilla-engelska "joined/left the game" och tappar
        // räkningen efter en omstart. Loggarna finns kvar som reserv medan
        // servern fortfarande startar och porten inte svarar.
        let players_online = if online {
            let host = crate::docker::container_name(server_id);
            match crate::ping::players_online(&host, crate::docker::MOUNT_PORT).await {
                Some(count) => {
                    self.players.insert(server_id.to_string(), count);
                    Some(count)
                }
                None => self.players.get(server_id).copied(),
            }
        } else {
            self.players.get(server_id).copied()
        };

        Ok(Sample {
            status,
            cpu_percent: stats.as_ref().and_then(|stats| stats.cpu_percent),
            memory_used_mb: stats.as_ref().and_then(|stats| stats.memory_used_mb),
            memory_limit_mb: stats.as_ref().and_then(|stats| stats.memory_limit_mb),
            uptime_seconds,
            players_online,
            players_max: max_players(data_root, server_id),
            disk_used_mb,
            network_rx_bytes,
            network_tx_bytes,
            artifact_present,
            logs,
            cursor,
        })
    }

    async fn disk_usage(&mut self, server_id: &str, root: &Path) -> Option<u64> {
        if let Some((measured_at, value)) = self.disk.get(server_id) {
            if measured_at.elapsed() < Duration::from_secs(DISK_CACHE_SECONDS) {
                return Some(*value);
            }
        }

        let path = root.to_path_buf();
        let bytes = tokio::task::spawn_blocking(move || directory_bytes(&path))
            .await
            .ok()?;
        let megabytes = bytes / (1024 * 1024);
        self.disk
            .insert(server_id.to_string(), (Instant::now(), megabytes));
        Some(megabytes)
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

/// Summerar vanliga filer under serverroten. Symlinks följs aldrig, så en
/// länk ut ur roten varken räknas eller korsas.
fn directory_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata().map(|meta| meta.len()).unwrap_or(0));
            }
        }
    }

    total
}

fn has_artifact(root: &Path, artifact: &Artifact<'_>) -> bool {
    if !is_safe_basename(artifact.jar) {
        return false;
    }

    match std::fs::symlink_metadata(root.join(artifact.jar)) {
        Ok(metadata) => metadata.file_type().is_file() && metadata.len() >= artifact.min_bytes,
        Err(_) => false,
    }
}

fn is_safe_basename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
}

fn is_new(cursor: Option<&str>, timestamp: &str) -> bool {
    cursor.is_none_or(|cursor| timestamp > cursor)
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

    fn telemetry_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "craftpilot-telemetry-{}-{}",
            std::process::id(),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sums_regular_files_under_the_server_root() {
        let dir = telemetry_dir("disk");
        std::fs::create_dir_all(dir.join("world/region")).unwrap();
        std::fs::write(dir.join("server.properties"), "x".repeat(1000)).unwrap();
        std::fs::write(dir.join("world/level.dat"), "x".repeat(2000)).unwrap();
        std::fs::write(dir.join("world/region/r.0.0.mca"), "x".repeat(3000)).unwrap();

        assert_eq!(directory_bytes(&dir), 6000);
        assert_eq!(directory_bytes(&dir.join("missing")), 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn disk_usage_does_not_follow_symlinks() {
        let dir = telemetry_dir("disk-symlink");
        let outside = telemetry_dir("disk-outside");
        std::fs::write(outside.join("big.bin"), "x".repeat(5000)).unwrap();
        std::fs::write(dir.join("inside.bin"), "x".repeat(100)).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("escape")).unwrap();

        assert_eq!(directory_bytes(&dir), 100);

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn artifact_presence_checks_size_and_basename() {
        let dir = telemetry_dir("artifact");
        std::fs::write(dir.join("server.jar"), "x".repeat(2048)).unwrap();

        let present = Artifact {
            jar: "server.jar",
            min_bytes: 1024,
        };
        assert!(has_artifact(&dir, &present));
        assert!(!has_artifact(
            &dir,
            &Artifact {
                jar: "server.jar",
                min_bytes: 4096
            }
        ));
        assert!(!has_artifact(
            &dir,
            &Artifact {
                jar: "../server.jar",
                min_bytes: 0
            }
        ));
        assert!(!has_artifact(
            &dir,
            &Artifact {
                jar: "missing.jar",
                min_bytes: 0
            }
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn skips_the_inclusive_since_boundary() {
        assert!(is_new(None, "2026-11-11T22:00:00Z"));
        assert!(!is_new(
            Some("2026-11-11T22:00:00Z"),
            "2026-11-11T22:00:00Z"
        ));
        assert!(is_new(Some("2026-11-11T22:00:00Z"), "2026-11-11T22:00:01Z"));
        assert!(!is_new(
            Some("2026-11-11T22:00:01Z"),
            "2026-11-11T22:00:00Z"
        ));
    }

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
