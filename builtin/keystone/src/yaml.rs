//! A small Bukkit style wrapper around YAML files: values are read and written by
//! dotted paths like "economy.symbol", so the Pumpkin version reads the exact same
//! files as the Java version.

use serde_yaml_ng::{Mapping, Value};
use std::fs;

pub struct Yaml {
    pub root: Value,
    path: String,
    defaults: Option<Value>,
}

fn key(k: &str) -> Value {
    Value::String(k.to_string())
}

impl Yaml {
    /// Loads a file. When it does not exist and a default is given, the default is written first.
    pub fn load(path: &str, default: Option<&str>) -> Yaml {
        if fs::metadata(path).is_err() {
            if let Some(d) = default {
                let _ = fs::write(path, d);
            }
        }
        let text = fs::read_to_string(path).unwrap_or_default();
        let root = serde_yaml_ng::from_str::<Value>(&text).unwrap_or(Value::Mapping(Mapping::new()));
        let root = if root.is_mapping() { root } else { Value::Mapping(Mapping::new()) };
        let defaults = default.and_then(|d| serde_yaml_ng::from_str::<Value>(d).ok());
        Yaml { root, path: path.to_string(), defaults }
    }

    pub fn empty(path: &str) -> Yaml {
        Yaml { root: Value::Mapping(Mapping::new()), path: path.to_string(), defaults: None }
    }

    fn find<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
        let mut cur = root;
        for part in path.split('.') {
            cur = cur.as_mapping()?.get(key(part))?;
        }
        Some(cur)
    }

    /// The value at a path, falling back to the bundled default file.
    pub fn get(&self, path: &str) -> Option<&Value> {
        match Self::find(&self.root, path) {
            Some(Value::Null) | None => self.defaults.as_ref().and_then(|d| Self::find(d, path)),
            v => v,
        }
    }

    pub fn is_set(&self, path: &str) -> bool {
        !matches!(Self::find(&self.root, path), None | Some(Value::Null))
    }

    pub fn str(&self, path: &str, def: &str) -> String {
        match self.get(path) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => def.to_string(),
        }
    }

    pub fn opt_str(&self, path: &str) -> Option<String> {
        match self.get(path) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn f64(&self, path: &str, def: f64) -> f64 {
        match self.get(path) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(def),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(def),
            _ => def,
        }
    }

    pub fn i64(&self, path: &str, def: i64) -> i64 {
        match self.get(path) {
            Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(def),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(def),
            _ => def,
        }
    }

    pub fn bool(&self, path: &str, def: bool) -> bool {
        match self.get(path) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => def,
        }
    }

    pub fn list(&self, path: &str) -> Vec<String> {
        match self.get(path) {
            Some(Value::Sequence(seq)) => seq
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => String::new(),
                })
                .collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    /// Keys directly under a path, in file order. Uses the default file when the path is missing.
    pub fn keys(&self, path: &str) -> Vec<String> {
        let section = if path.is_empty() { Some(&self.root) } else { self.get(path) };
        match section {
            Some(Value::Mapping(m)) => m.keys().filter_map(|k| match k {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            }).collect(),
            _ => Vec::new(),
        }
    }

    pub fn is_section(&self, path: &str) -> bool {
        matches!(self.get(path), Some(Value::Mapping(_)))
    }

    /// Sets a value, creating sections on the way. Null removes the key.
    pub fn set(&mut self, path: &str, value: Value) {
        let parts: Vec<&str> = path.split('.').collect();
        let mut cur = &mut self.root;
        for part in &parts[..parts.len() - 1] {
            if !cur.is_mapping() {
                *cur = Value::Mapping(Mapping::new());
            }
            let map = cur.as_mapping_mut().unwrap();
            if !matches!(map.get(key(part)), Some(Value::Mapping(_))) {
                map.insert(key(part), Value::Mapping(Mapping::new()));
            }
            cur = map.get_mut(key(part)).unwrap();
        }
        if !cur.is_mapping() {
            *cur = Value::Mapping(Mapping::new());
        }
        let map = cur.as_mapping_mut().unwrap();
        let last = parts[parts.len() - 1];
        if value.is_null() {
            map.remove(key(last));
        } else {
            map.insert(key(last), value);
        }
    }

    pub fn set_str(&mut self, path: &str, v: &str) {
        self.set(path, Value::String(v.to_string()));
    }

    pub fn set_f64(&mut self, path: &str, v: f64) {
        self.set(path, Value::from(v));
    }

    pub fn set_i64(&mut self, path: &str, v: i64) {
        self.set(path, Value::from(v));
    }

    pub fn set_bool(&mut self, path: &str, v: bool) {
        self.set(path, Value::Bool(v));
    }

    pub fn set_list(&mut self, path: &str, v: &[String]) {
        self.set(path, Value::Sequence(v.iter().map(|s| Value::String(s.clone())).collect()));
    }

    pub fn remove(&mut self, path: &str) {
        self.set(path, Value::Null);
    }

    pub fn to_string(&self) -> String {
        serde_yaml_ng::to_string(&self.root).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Err(e) = fs::write(&self.path, self.to_string()) {
            crate::log(&format!("Could not save {}: {e}", self.path));
        }
    }
}
