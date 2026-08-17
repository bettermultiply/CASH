use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    seed_dir: Option<PathBuf>,
}

pub fn default_seed_dir() -> PathBuf {
    if let Some(path) = env_path("CASH_SEED_DIR") {
        return path;
    }

    if let Some(cfg) = read_config("CASH_CONFIG", ".config/cash/config.json")
        && let Some(seed_dir) = cfg.seed_dir
    {
        return expand_home(seed_dir);
    }

    home_dir().join(".local/share/cash/seeds")
}

pub fn default_seed_output(agent: &str, session_id: &str) -> PathBuf {
    default_seed_dir()
        .join(agent)
        .join(sanitize_path_segment(session_id))
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn read_config(env_name: &str, default: &str) -> Option<ConfigFile> {
    let path = std::env::var(env_name)
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(default));
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .map(expand_home)
}

fn expand_home(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        home_dir()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home_dir().join(rest)
    } else {
        path
    }
}

fn sanitize_path_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}
