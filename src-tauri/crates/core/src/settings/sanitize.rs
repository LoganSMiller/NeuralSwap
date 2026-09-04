use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::schema::{Addon, ArtRecord, Recent, ScanRecord, Settings, Theme, SCHEMA_VERSION};

/// Persisted settings are untrusted input: an older build wrote them, a newer
/// one may have, or somebody opened the file in an editor. Every field is
/// coerced against a default rather than believed.
///
/// Field-by-field rather than one `serde` deserialise, because a single
/// wrong-typed field would fail the whole document - and dropping one
/// malformed field must never cost the user the other forty. That is the
/// failure this module exists to prevent.
pub fn sanitize(input: &Value) -> Settings {
    let mut out = Settings::default();
    let Some(row) = input.as_object() else {
        return out;
    };

    out.schema = SCHEMA_VERSION;

    if let Some(theme) = row.get("theme").and_then(Value::as_str) {
        out.theme = match theme {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            "system" => Theme::System,
            _ => out.theme,
        };
    }
    // A language tag, not free text: it indexes a catalogue and reaches the DOM.
    if let Some(lang) = row.get("lang").and_then(Value::as_str) {
        if is_language_tag(lang) {
            out.lang = lang.to_owned();
        }
    }
    out.group_games_by_store = boolean(row, "groupGamesByStore", out.group_games_by_store);
    out.auto_scan_drives = boolean(row, "autoScanDrives", out.auto_scan_drives);

    out.folders = path_list(row, "folders");
    out.excluded_roots = path_list(row, "excludedRoots");
    out.manual = path_list(row, "manual");
    out.hidden = path_list(row, "hidden");

    out.posters = string_map(row, "posters");
    out.scans = record_map(row, "scans", scan_record);
    out.art = record_map(row, "art", art_record);
    out.recents = recents(row);
    out.addons = addons(row);
    out
}

fn is_language_tag(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    parts.all(|part| {
        (2..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

fn boolean(row: &Map<String, Value>, key: &str, fallback: bool) -> bool {
    row.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

/// Absolute-ish path strings, de-duplicated case-insensitively, order kept.
fn path_list(row: &Map<String, Value>, key: &str) -> Vec<String> {
    let Some(list) = row.get(key).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for item in list {
        let Some(text) = item.as_str() else { continue };
        if text.is_empty() || text.contains('\0') {
            continue;
        }
        let folded = text.to_lowercase();
        if seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        out.push(text.to_owned());
    }
    out
}

fn string_map(row: &Map<String, Value>, key: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(map) = row.get(key).and_then(Value::as_object) {
        for (name, value) in map {
            if let Some(text) = value.as_str() {
                out.insert(name.clone(), text.to_owned());
            }
        }
    }
    out
}

fn record_map<T>(
    row: &Map<String, Value>,
    key: &str,
    each: fn(&Value) -> Option<T>,
) -> BTreeMap<String, T> {
    let mut out = BTreeMap::new();
    if let Some(map) = row.get(key).and_then(Value::as_object) {
        for (name, value) in map {
            if let Some(parsed) = each(value) {
                out.insert(name.clone(), parsed);
            }
        }
    }
    out
}

fn text(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(row: &Map<String, Value>, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn scan_record(value: &Value) -> Option<ScanRecord> {
    let row = value.as_object()?;
    // A record with no generation stamp predates the field and cannot be
    // trusted to match current detection rules; treat it as absent so the
    // folder is rescanned rather than answered from a stale verdict.
    let rules = row.get("rules").and_then(Value::as_i64)?;
    Some(ScanRecord {
        ok: row.get("ok").and_then(Value::as_bool).unwrap_or(false),
        api: text(row, "api"),
        api_label: text(row, "apiLabel"),
        bitness: match row.get("bitness").and_then(Value::as_i64) {
            Some(32) => Some(32),
            Some(64) => Some(64),
            _ => None,
        },
        exe: text(row, "exe"),
        dlss_version: text(row, "dlssVersion"),
        routes: row
            .get("routes")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        reason: text(row, "reason"),
        scanned_at: number(row, "scannedAt"),
        rules,
    })
}

fn art_record(value: &Value) -> Option<ArtRecord> {
    let row = value.as_object()?;
    let rules = row.get("rules").and_then(Value::as_i64)?;
    Some(ArtRecord {
        appid: row.get("appid").and_then(Value::as_i64),
        cover: text(row, "cover"),
        hero: text(row, "hero"),
        rules,
        fetched_at: number(row, "fetchedAt"),
        miss: row.get("miss").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn recents(row: &Map<String, Value>) -> Vec<Recent> {
    let Some(list) = row.get("recents").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<Recent> = list
        .iter()
        .filter_map(|item| {
            let entry = item.as_object()?;
            Some(Recent {
                dir: text(entry, "dir")?,
                at: number(entry, "at"),
            })
        })
        .collect();
    out.sort_by_key(|row| std::cmp::Reverse(row.at));
    out.truncate(24);
    out
}

fn addons(row: &Map<String, Value>) -> Vec<Addon> {
    let Some(list) = row.get("addons").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let entry = item.as_object()?;
            Some(Addon {
                path: text(entry, "path")?,
                name: text(entry, "name")
                    .map(|n| n.trim().to_owned())
                    .filter(|n| !n.is_empty()),
                notes: entry
                    .get("notes")
                    .and_then(Value::as_array)
                    .map(|l| {
                        l.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                enabled: entry
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}
