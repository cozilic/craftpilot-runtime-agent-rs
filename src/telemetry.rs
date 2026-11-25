use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::docker::Docker;
use crate::docker_api::ContainerSample;
use crate::error::Result;

/// Docker Engine API:s CPU-formel ur två kumulativa sampel.
fn percent_between(
    (previous_total, previous_system): (u64, u64),
    (total, system): (u64, u64),
    cpus: u64,
) -> Option<f64> {
    if system <= previous_system || total < previous_total {
        return None;
    }

    let percent = (total - previous_total) as f64 / (system - previous_system) as f64
        * cpus.max(1) as f64
        * 100.0;
    (0.0..=1000.0).contains(&percent).then_some(percent)
}

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
    /// Senaste kumulativa CPU-räknarna (total, system) per server. `one-shot`
    /// lämnar `precpu_stats` med nollor, så deltat räknas mot förra rapporten.
    cpu: HashMap<String, (u64, u64)>,
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
            cpu: HashMap::new(),
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
        // CPU, minne och nätverksräknare i EN Engine API-läsning (2026-11-25).
        // Bara en körande container mäts: en stoppad svarar med nollor som ser
        // ut som en mätning. Ett fält som saknas blir `None` och hoppas över i
        // rapporten.
        let stats = if online {
            docker.sample(server_id).await
        } else {
            None
        };
        let (network_rx_bytes, network_tx_bytes) = match &stats {
            Some(stats) => (stats.network_rx_bytes, stats.network_tx_bytes),
            None => (None, None),
        };
        let cpu_percent = match &stats {
            Some(stats) => self.cpu_percent(server_id, stats),
            None => None,
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

        let lines = docker.logs_since(server_id, since).await?;
        let (logs, cursor) = self.collect_new_lines(server_id, since, lines);

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
            cpu_percent,
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

    /// CPU i procent sedan FÖREGÅENDE rapport (Docker Engine API:s formel:
    /// delta CPU-tid / delta systemtid × kärnor × 100). Räknarna är
    /// kumulativa och `one-shot` bär ingen `precpu_stats`, så deltat hålls här.
    /// Första rapporten för en server — och en omstartad container, där
    /// räknaren går bakåt — lämnar `None` och blir en mätning nästa varv.
    fn cpu_percent(&mut self, server_id: &str, stats: &ContainerSample) -> Option<f64> {
        let (Some(total), Some(system)) = (stats.cpu_total_usage, stats.system_cpu_usage) else {
            return None;
        };
        let previous = self.cpu.insert(server_id.to_string(), (total, system))?;

        percent_between(previous, (total, system), stats.online_cpus.unwrap_or(1))
    }

    /// Nya konsolrader sedan cursorn UTAN stats, ping eller disk (2026-11-25).
    /// Den snabba loggposten kör varje sekund och får inte kosta en
    /// stats-läsning per server och varv; bara `docker logs`.
    ///
    /// Loggkursorn returneras bara tillsammans med raderna: anroparen flyttar
    /// den när mastern kvitterat, så en tappad post skickas om i stället för
    /// att tappas (rapportens 10 s-väg bär samma rader som reserv).
    pub async fn new_logs(
        &mut self,
        docker: &Docker,
        server_id: &str,
        since: Option<&str>,
    ) -> Result<(Vec<(String, String)>, Option<String>)> {
        let lines = docker.logs_since(server_id, since).await?;

        Ok(self.collect_new_lines(server_id, since, lines))
    }

    /// Rader efter cursorn, rensade och kapade till `LOG_WINDOW`. En join-rad
    /// räknas även när den faller utanför fönstret — spelarräkningen är en
    /// händelsesumma, inte ett urval av de rader som skickas.
    fn collect_new_lines(
        &mut self,
        server_id: &str,
        since: Option<&str>,
        lines: Vec<(String, String)>,
    ) -> (Vec<(String, String)>, Option<String>) {
        let mut cursor = since.map(str::to_string);
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

        (logs, cursor)
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
    fn cpu_is_the_delta_since_the_previous_report() {
        let mut reporter = Reporter::new();
        let first = ContainerSample {
            cpu_total_usage: Some(1_000_000_000),
            system_cpu_usage: Some(10_000_000_000),
            online_cpus: Some(2),
            ..Default::default()
        };
        // Första rapporten har inget delta att jämföra med.
        assert_eq!(reporter.cpu_percent("srv", &first), None);

        let second = ContainerSample {
            cpu_total_usage: Some(2_000_000_000),
            system_cpu_usage: Some(20_000_000_000),
            online_cpus: Some(2),
            ..Default::default()
        };
        let percent = reporter.cpu_percent("srv", &second).expect("a percentage");
        assert!((percent - 20.0).abs() < 0.001);
    }

    #[test]
    fn a_restarted_container_has_no_cpu_until_the_next_report() {
        let mut reporter = Reporter::new();
        let before = ContainerSample {
            cpu_total_usage: Some(5_000),
            system_cpu_usage: Some(10_000),
            ..Default::default()
        };
        assert_eq!(reporter.cpu_percent("srv", &before), None);

        // Räknaren går bakåt vid omstart → ingen procent, men det nya värdet
        // sparas så nästa rapport mäter igen.
        let restarted = ContainerSample {
            cpu_total_usage: Some(1),
            system_cpu_usage: Some(20_000),
            ..Default::default()
        };
        assert_eq!(reporter.cpu_percent("srv", &restarted), None);

        let next = ContainerSample {
            cpu_total_usage: Some(1_001),
            system_cpu_usage: Some(30_000),
            ..Default::default()
        };
        assert!(reporter.cpu_percent("srv", &next).is_some());
    }

    #[test]
    fn ignores_unrelated_lines() {
        let mut reporter = Reporter::new();
        reporter.apply_player_event("srv", "Starting minecraft server version 1.21");
        assert_eq!(reporter.players.get("srv").copied(), None);
    }

    #[test]
    fn collects_new_lines_after_the_cursor_and_counts_the_joins() {
        let mut reporter = Reporter::new();
        let lines = vec![
            ("2026-11-25T10:00:00Z".to_string(), "old line".to_string()),
            (
                "2026-11-25T10:00:01Z".to_string(),
                "Steve joined the game".to_string(),
            ),
            (
                "2026-11-25T10:00:02Z".to_string(),
                "  padded\u{0}".to_string(),
            ),
        ];

        let (logs, cursor) = reporter.collect_new_lines("srv", Some("2026-11-25T10:00:00Z"), lines);

        assert_eq!(cursor.as_deref(), Some("2026-11-25T10:00:02Z"));
        assert_eq!(
            logs,
            vec![
                (
                    "2026-11-25T10:00:01Z".to_string(),
                    "Steve joined the game".to_string()
                ),
                ("2026-11-25T10:00:02Z".to_string(), "  padded".to_string()),
            ]
        );
        assert_eq!(reporter.players.get("srv").copied(), Some(1));
    }

    #[test]
    fn a_busy_server_is_capped_to_the_log_window_but_every_join_is_counted() {
        let mut reporter = Reporter::new();
        let mut lines = Vec::new();

        for index in 0..(LOG_WINDOW + 5) {
            let text = if index == 0 {
                "Alex joined the game".to_string()
            } else {
                format!("rad {index}")
            };
            // Strikt stigande tider: en upprepad stämpel är inte en ny rad.
            lines.push((
                format!("2026-11-25T10:{:02}:{:02}Z", index / 60, index % 60),
                text,
            ));
        }

        let (logs, _) = reporter.collect_new_lines("srv", None, lines);

        assert_eq!(logs.len(), LOG_WINDOW);
        // Första raden föll utanför fönstret men joinen räknades ändå.
        assert_eq!(logs.first().map(|(_, text)| text.as_str()), Some("rad 5"));
        assert_eq!(reporter.players.get("srv").copied(), Some(1));
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
