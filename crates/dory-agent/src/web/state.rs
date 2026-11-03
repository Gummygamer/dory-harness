//! What survives between runs about web search: when the quota was last found used up. The file
//! is `.dory/web-state.json` in the workspace and holds no key.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

/// Where the state file lives for a workspace.
pub fn state_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".dory").join("web-state.json")
}

/// Records that the quota was found used up now.
pub fn record_quota_used_up(path: &Path, now: SystemTime) -> io::Result<()> {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, json!({"quota_used_up_unix": secs}).to_string())
}

/// Removes the record, after a search has succeeded again. A missing file is not an error.
pub fn clear(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The time the quota was last found used up, as shown to the user, if it was.
pub fn quota_used_up_at(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let secs = v.get("quota_used_up_unix")?.as_i64()?;
    Some(format_utc(secs))
}

/// `YYYY-MM-DD HH:MM UTC` for a Unix time in seconds.
pub fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60)
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe as i64 + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_formatted_in_utc() {
        assert_eq!(format_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_utc(1_704_067_200), "2024-01-01 00:00 UTC");
        assert_eq!(format_utc(1_709_251_199), "2024-02-29 23:59 UTC", "2024 is a leap year");
        assert_eq!(format_utc(1_793_714_232), "2026-11-03 13:57 UTC");
    }

    #[test]
    fn a_recorded_quota_time_is_read_back_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(dir.path());
        assert_eq!(quota_used_up_at(&path), None);
        record_quota_used_up(&path, UNIX_EPOCH + std::time::Duration::from_secs(1_704_067_200)).unwrap();
        assert_eq!(quota_used_up_at(&path).as_deref(), Some("2024-01-01 00:00 UTC"));
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("key"), "the state file must hold no key: {text}");
        clear(&path).unwrap();
        assert_eq!(quota_used_up_at(&path), None);
        clear(&path).unwrap();
    }
}
