use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRecord {
    pub ok: bool,
    pub api: Option<String>,
    pub api_label: Option<String>,
    pub bitness: Option<u8>,
    pub exe: Option<String>,
    pub dlss_version: Option<String>,
    pub routes: Vec<String>,
    pub reason: Option<String>,
    pub scanned_at: i64,
    /// Detection-logic generation. A bump invalidates every cached verdict.
    pub rules: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtRecord {
    pub appid: Option<i64>,
    pub cover: Option<String>,
    pub hero: Option<String>,
    pub rules: i64,
    pub fetched_at: i64,
    /// Remembering a miss is what stops us asking Steam again every launch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub miss: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub dir: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Addon {
    pub path: String,
    pub name: Option<String>,
    pub notes: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub schema: u32,
    pub theme: Theme,
    pub lang: String,
    pub group_games_by_store: bool,
    pub auto_scan_drives: bool,
    pub folders: Vec<String>,
    pub excluded_roots: Vec<String>,
    pub manual: Vec<String>,
    pub hidden: Vec<String>,
    pub posters: BTreeMap<String, String>,
    pub scans: BTreeMap<String, ScanRecord>,
    pub art: BTreeMap<String, ArtRecord>,
    pub recents: Vec<Recent>,
    pub addons: Vec<Addon>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            theme: Theme::System,
            lang: "en".to_owned(),
            group_games_by_store: true,
            // Sweeping every drive without being asked is a surprise, not a
            // feature.
            auto_scan_drives: false,
            folders: Vec::new(),
            excluded_roots: Vec::new(),
            manual: Vec::new(),
            hidden: Vec::new(),
            posters: BTreeMap::new(),
            scans: BTreeMap::new(),
            art: BTreeMap::new(),
            recents: Vec::new(),
            addons: Vec::new(),
        }
    }
}

/// Migrate a document one schema version forward, or `None` if there is no
/// step from that version.
///
/// Each step is total: it must cope with fields that are missing or the wrong
/// type, because the document it is handed was written by an older build or
/// edited by hand.
pub fn migrate(version: u32, input: &Map<String, Value>) -> Option<Map<String, Value>> {
    match version {
        1 => Some(migrate_v1(input)),
        _ => None,
    }
}

/// v1 is the upstream `library.json` layout, which spread add-ons over three
/// fields that had drifted apart over releases:
///
///   `addon`      - a single enabled path, from the oldest builds
///   `addons`     - the enabled paths, as bare strings
///   `addonFiles` - the catalogue of hand-added builds, with names and notes
///
/// They collapse into one list carrying an `enabled` flag. Enablement is the
/// union of the first two; metadata comes from the third. A build that was
/// enabled but never catalogued still has to appear, or migrating would
/// quietly switch off something the user had turned on.
fn migrate_v1(input: &Map<String, Value>) -> Map<String, Value> {
    let mut out = input.clone();

    let path_of = |entry: &Value| -> Option<String> {
        match entry {
            Value::String(text) => Some(text.clone()),
            Value::Object(row) => row.get("path")?.as_str().map(str::to_owned),
            _ => None,
        }
    };
    let array = |key: &str| -> Vec<Value> {
        input
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };

    let mut enabled: Vec<String> = Vec::new();
    for entry in array("addons") {
        if let Some(file) = path_of(&entry) {
            enabled.push(file.to_lowercase());
        }
    }
    if let Some(single) = input.get("addon").and_then(Value::as_str) {
        enabled.push(single.to_lowercase());
    }

    // Insertion-ordered so the migrated list is stable across runs.
    let mut order: Vec<String> = Vec::new();
    let mut catalogue: BTreeMap<String, Addon> = BTreeMap::new();
    let mut remember = |file: &str, name: Option<&str>, notes: Option<&Value>| {
        let key = file.to_lowercase();
        if !catalogue.contains_key(&key) {
            order.push(key.clone());
        }
        let row = catalogue.entry(key).or_insert_with(|| Addon {
            path: file.to_owned(),
            name: None,
            notes: Vec::new(),
            enabled: false,
        });
        if let Some(text) = name.map(str::trim).filter(|t| !t.is_empty()) {
            row.name = Some(text.to_owned());
        }
        if let Some(Value::Array(list)) = notes {
            row.notes = list
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
    };

    for entry in array("addonFiles") {
        let Some(file) = path_of(&entry) else {
            continue;
        };
        let row = entry.as_object();
        remember(
            &file,
            row.and_then(|r| r.get("name")).and_then(Value::as_str),
            row.and_then(|r| r.get("notes")),
        );
    }
    // Anything enabled but absent from the catalogue is added bare, so the
    // switched-on state survives even when its description does not.
    for entry in array("addons") {
        if let Some(file) = path_of(&entry) {
            remember(&file, None, None);
        }
    }
    if let Some(single) = input.get("addon").and_then(Value::as_str) {
        remember(single, None, None);
    }

    let list: Vec<Value> = order
        .iter()
        .filter_map(|key| {
            let row = catalogue.get(key)?;
            let mut row = row.clone();
            row.enabled = enabled.iter().any(|e| e == key);
            serde_json::to_value(row).ok()
        })
        .collect();

    out.insert("addons".to_owned(), Value::Array(list));
    out.remove("addon");
    out.remove("addonFiles");
    out.insert("schema".to_owned(), Value::from(2));
    out
}
