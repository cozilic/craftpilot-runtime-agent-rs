//! Skriver in byggversionen i binären så agenten kan rapportera vilken release
//! noden faktiskt kör.
//!
//! Ordningen: `CRAFTPILOT_AGENT_VERSION` (som CI/Docker sätter från taggen),
//! annars `git describe --tags --always --dirty`, annars `dev`. Värdet
//! normaliseras till masterns tillåtna tecken (`^[A-Za-z0-9][A-Za-z0-9._+-]*$`,
//! max 40) så en tagg eller gren med `/` inte gör heartbeatet ogiltigt.

use std::process::Command;

const ENV_OVERRIDE: &str = "CRAFTPILOT_AGENT_VERSION";

fn main() {
    println!("cargo:rerun-if-env-changed={ENV_OVERRIDE}");
    println!("cargo:rerun-if-changed=.git/HEAD");

    let version = std::env::var(ENV_OVERRIDE)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "dev".to_string());

    println!(
        "cargo:rustc-env=CRAFTPILOT_BUILD_VERSION={}",
        normalize(&version)
    );
}

fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn normalize(raw: &str) -> String {
    let trimmed = raw.trim();
    // "v0.3.0" -> "0.3.0"; ett "v" som inte följs av en siffra är en del av namnet.
    let trimmed = match trimmed.strip_prefix('v') {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => trimmed,
    };

    let mut cleaned = String::with_capacity(trimmed.len());
    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() || "._+-".contains(character) {
            cleaned.push(character);
        } else {
            cleaned.push('.');
        }
    }

    let cleaned = cleaned.trim_start_matches(['.', '-', '+', '_']);
    if cleaned.is_empty() {
        "dev".to_string()
    } else {
        cleaned.to_string()
    }
}
