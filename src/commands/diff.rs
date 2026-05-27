use std::collections::BTreeSet;

use indexmap::IndexMap;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct FieldDiff {
    pub field: String,
    pub body: DiffBody,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DiffBody {
    Scalar { file: String, live: String },
    Set { entries: Vec<SetEntry> },
    Map { entries: Vec<MapEntry> },
}

#[derive(Debug, Clone, Serialize)]
pub struct SetEntry {
    /// `'+'` (only in file) or `'-'` (only live).
    pub op: char,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MapEntry {
    /// `'+'` (only in file), `'-'` (only live), `'~'` (changed).
    pub op: char,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<String>,
}

pub fn diff_set(field: &str, file: &[String], live: &[String]) -> Option<FieldDiff> {
    let file_set: BTreeSet<&str> = file.iter().map(String::as_str).collect();
    let live_set: BTreeSet<&str> = live.iter().map(String::as_str).collect();
    if file_set == live_set {
        return None;
    }
    let mut entries: Vec<SetEntry> = Vec::new();
    for v in file_set.difference(&live_set) {
        entries.push(SetEntry {
            op: '+',
            value: (*v).to_string(),
        });
    }
    for v in live_set.difference(&file_set) {
        entries.push(SetEntry {
            op: '-',
            value: (*v).to_string(),
        });
    }
    Some(FieldDiff {
        field: field.into(),
        body: DiffBody::Set { entries },
    })
}

pub fn diff_map(
    field: &str,
    file: &IndexMap<String, String>,
    live: &IndexMap<String, String>,
) -> Option<FieldDiff> {
    let mut entries: Vec<MapEntry> = Vec::new();
    let mut keys: BTreeSet<&str> = file.keys().map(String::as_str).collect();
    keys.extend(live.keys().map(String::as_str));
    for k in keys {
        match (file.get(k), live.get(k)) {
            (Some(fv), Some(lv)) if fv == lv => {}
            (Some(fv), Some(lv)) => entries.push(MapEntry {
                op: '~',
                key: k.into(),
                file: Some(fv.clone()),
                live: Some(lv.clone()),
            }),
            (Some(fv), None) => entries.push(MapEntry {
                op: '+',
                key: k.into(),
                file: Some(fv.clone()),
                live: None,
            }),
            (None, Some(lv)) => entries.push(MapEntry {
                op: '-',
                key: k.into(),
                file: None,
                live: Some(lv.clone()),
            }),
            (None, None) => {}
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(FieldDiff {
        field: field.into(),
        body: DiffBody::Map { entries },
    })
}

/// Like `diff_set` but only flags entries present in `file` and missing
/// from `live`. Entries unique to `live` are ignored. Use for fields the
/// project file *adds to* rather than *replaces* (managed addon
/// entrypoint domains: Clever sets internal ones we don't want to
/// surface as drift).
pub fn diff_set_additive(field: &str, file: &[String], live: &[String]) -> Option<FieldDiff> {
    let live_set: BTreeSet<&str> = live.iter().map(String::as_str).collect();
    let mut entries: Vec<SetEntry> = Vec::new();
    for v in file {
        if !live_set.contains(v.as_str()) {
            entries.push(SetEntry {
                op: '+',
                value: v.clone(),
            });
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(FieldDiff {
        field: field.into(),
        body: DiffBody::Set { entries },
    })
}

/// Like `diff_map` but only flags keys present in `file` and either
/// missing or with a different value on `live`. Keys unique to `live`
/// are ignored. Use for managed addon entrypoint env: Clever populates
/// dozens of internal vars we never want to flag as drift.
pub fn diff_map_additive(
    field: &str,
    file: &IndexMap<String, String>,
    live: &IndexMap<String, String>,
) -> Option<FieldDiff> {
    let mut entries: Vec<MapEntry> = Vec::new();
    for (k, fv) in file {
        match live.get(k) {
            Some(lv) if lv == fv => {}
            Some(lv) => entries.push(MapEntry {
                op: '~',
                key: k.clone(),
                file: Some(fv.clone()),
                live: Some(lv.clone()),
            }),
            None => entries.push(MapEntry {
                op: '+',
                key: k.clone(),
                file: Some(fv.clone()),
                live: None,
            }),
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(FieldDiff {
        field: field.into(),
        body: DiffBody::Map { entries },
    })
}

/// Loose equivalence between an addon `kind` written in the project file and
/// the live provider id (stripped of the `-addon` suffix on the live side).
/// Mirrors the aliases recognised at apply time so the short and long forms
/// never read as drift.
pub fn kinds_equivalent(file: &str, live: &str) -> bool {
    if file == live {
        return true;
    }
    let canon = |s: &str| -> String {
        match s.to_lowercase().as_str() {
            "postgres" | "pg" | "postgresql-addon" => "postgresql".into(),
            "mongo" | "mongodb-addon" => "mongodb".into(),
            "es" | "es-addon" => "elasticsearch".into(),
            "s3" | "cellar-addon" => "cellar".into(),
            "mysql-addon" => "mysql".into(),
            "redis-addon" => "redis".into(),
            "addon-matomo" => "matomo".into(),
            "addon-pulsar" => "pulsar".into(),
            other => other.to_string(),
        }
    };
    canon(file) == canon(live)
}

/// Treat `S_BIG` and `s_big` as the same plan; matches the case-normalisation
/// done in `apply::validate_addons`.
pub fn sizes_equivalent(file: &str, live: &str) -> bool {
    file.to_lowercase() == live.to_lowercase()
}

pub fn quote_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_set_identical_returns_none() {
        assert!(diff_set("x", &["a".into()], &["a".into()]).is_none());
    }

    #[test]
    fn diff_set_adds_and_removes() {
        let d = diff_set(
            "domains",
            &["a".into(), "b".into()],
            &["b".into(), "c".into()],
        )
        .unwrap();
        let DiffBody::Set { entries } = &d.body else {
            panic!()
        };
        let added: Vec<&str> = entries
            .iter()
            .filter(|e| e.op == '+')
            .map(|e| e.value.as_str())
            .collect();
        let removed: Vec<&str> = entries
            .iter()
            .filter(|e| e.op == '-')
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(added, ["a"]);
        assert_eq!(removed, ["c"]);
    }

    #[test]
    fn diff_map_classifies_entries() {
        let mut f = IndexMap::new();
        f.insert("KEPT".into(), "v".into());
        f.insert("ADDED".into(), "new".into());
        f.insert("CHANGED".into(), "after".into());
        let mut l = IndexMap::new();
        l.insert("KEPT".into(), "v".into());
        l.insert("REMOVED".into(), "old".into());
        l.insert("CHANGED".into(), "before".into());
        let d = diff_map("env", &f, &l).unwrap();
        let DiffBody::Map { entries } = &d.body else {
            panic!()
        };
        assert_eq!(entries.len(), 3);
        let ops: BTreeSet<char> = entries.iter().map(|e| e.op).collect();
        assert!(ops.contains(&'+') && ops.contains(&'-') && ops.contains(&'~'));
    }

    #[test]
    fn diff_set_additive_only_flags_missing_from_live() {
        // file has "a" and "b"; live has "b" and an extra "c". Only "a"
        // should surface — "c" is ignored.
        let d = diff_set_additive(
            "domains",
            &["a".into(), "b".into()],
            &["b".into(), "c".into()],
        )
        .unwrap();
        let DiffBody::Set { entries } = &d.body else {
            panic!()
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].op, '+');
        assert_eq!(entries[0].value, "a");
    }

    #[test]
    fn diff_set_additive_returns_none_when_file_subset_of_live() {
        let d = diff_set_additive(
            "domains",
            &["b".into()],
            &["a".into(), "b".into(), "c".into()],
        );
        assert!(d.is_none());
    }

    #[test]
    fn diff_map_additive_ignores_live_only_keys() {
        let mut f = IndexMap::new();
        f.insert("FOO".into(), "new".into());
        f.insert("BAR".into(), "same".into());
        let mut l = IndexMap::new();
        l.insert("FOO".into(), "old".into()); // changed
        l.insert("BAR".into(), "same".into()); // unchanged
        l.insert("CC_INTERNAL".into(), "leave-me-alone".into()); // live-only — ignored
        let d = diff_map_additive("env", &f, &l).unwrap();
        let DiffBody::Map { entries } = &d.body else {
            panic!()
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "FOO");
        assert_eq!(entries[0].op, '~');
    }

    #[test]
    fn diff_map_additive_flags_missing_keys_as_add() {
        let mut f = IndexMap::new();
        f.insert("NEW".into(), "yes".into());
        let l = IndexMap::new();
        let d = diff_map_additive("env", &f, &l).unwrap();
        let DiffBody::Map { entries } = &d.body else {
            panic!()
        };
        assert_eq!(entries[0].op, '+');
        assert_eq!(entries[0].key, "NEW");
    }

    #[test]
    fn kinds_equivalent_basics() {
        assert!(kinds_equivalent("postgresql", "postgresql-addon"));
        assert!(kinds_equivalent("pg", "postgresql"));
        assert!(kinds_equivalent("cellar", "s3"));
        assert!(!kinds_equivalent("redis", "postgresql"));
    }

    #[test]
    fn sizes_equivalent_case_insensitive() {
        assert!(sizes_equivalent("S_BIG", "s_big"));
        assert!(!sizes_equivalent("s_big", "m_big"));
    }
}
