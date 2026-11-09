//! settings.json (preferences), usr/secrets.json (passwords/tokens) and tmp/state.json (what's
//! running). Plain JSON so they can be edited by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub fn defaults() -> Value {
    json!({
        "hostname_template": "{name}.test",
        "www_dir": "",
        "web_server": "apache",
        "ssl": true,
        "start_all_on_launch": false,
        "run_on_startup": false,
        "minimize_to_tray": true,
        "stop_all_on_exit": true,
        "editor": "",
        "terminal": "cmd",
        "services": {"mysql": true, "mailpit": true, "postgresql": false, "redis": false,
                     "memcached": false, "mongodb": false},
        "versions": {},
        "ports": {"http": 80, "https": 443, "php": 10987, "php_fallback": 20987, "mysql": 3306,
                  "postgresql": 5432, "mongodb": 27017, "redis": 6379, "memcached": 11211,
                  "mailpit_smtp": 1025, "mailpit_ui": 8025},
        "php": {"children": 10, "max_requests": 0, "build": "ts", "timezone": "UTC",
                "memory_limit": "512M", "upload_max_filesize": "256M", "post_max_size": "256M",
                "max_execution_time": 300, "extensions": {}, "xdebug": false,
                "auto_update": true, "auto_switch": false},
        "memcached": {"memory_mb": 64},
        "backup": {"enabled": true, "interval_hours": 8, "keep": 5},
        "quick_app": {"create_database": true, "open_browser": true},
        "updates": {"cacert": true}
    })
}

pub fn secret_defaults() -> Value {
    json!({"mysql_root_password": "", "ngrok_authtoken": "", "phpmyadmin_secret": ""})
}

pub fn deep_merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => deep_merge(existing, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// A JSON file merged over defaults. Interior mutability so it can live in a shared App.
pub struct JsonStore {
    path: PathBuf,
    defaults: Value,
    data: Mutex<Value>,
}

impl JsonStore {
    pub fn open(path: PathBuf, defaults: Value) -> Result<Self> {
        let s = JsonStore { path, defaults, data: Mutex::new(Value::Null) };
        s.load()?;
        Ok(s)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Value> {
        self.data.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn load(&self) -> Result<()> {
        let mut merged = self.defaults.clone();
        if let Ok(text) = fs::read_to_string(&self.path) {
            if !text.trim().is_empty() {
                let user: Value = serde_json::from_str(&text)
                    .with_context(|| format!("{} is not valid JSON", self.path.display()))?;
                deep_merge(&mut merged, &user);
            }
        }
        *self.lock() = merged;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        let text = serde_json::to_string_pretty(&*self.lock())?;
        if let Some(p) = self.path.parent() {
            fs::create_dir_all(p)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn snapshot(&self) -> Value {
        self.lock().clone()
    }

    pub fn get(&self, dotted: &str) -> Value {
        let data = self.lock();
        let mut cur = &*data;
        for part in dotted.split('.') {
            match cur.get(part) {
                Some(v) => cur = v,
                None => return Value::Null,
            }
        }
        cur.clone()
    }

    pub fn str(&self, dotted: &str) -> String {
        match self.get(dotted) {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }

    pub fn bool(&self, dotted: &str) -> bool {
        self.get(dotted).as_bool().unwrap_or(false)
    }

    pub fn u64(&self, dotted: &str, default: u64) -> u64 {
        let v = self.get(dotted);
        v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(default)
    }

    pub fn port(&self, key: &str) -> u16 {
        self.u64(&format!("ports.{key}"), 0) as u16
    }

    /// Set a value without saving (call save() afterwards).
    pub fn put(&self, dotted: &str, value: Value) {
        let mut data = self.lock();
        let mut cur = &mut *data;
        let parts: Vec<&str> = dotted.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            if !cur.get(*part).map(Value::is_object).unwrap_or(false) {
                cur[*part] = json!({});
            }
            cur = cur.get_mut(*part).unwrap();
        }
        cur[parts[parts.len() - 1]] = value;
    }

    pub fn set(&self, dotted: &str, value: Value) -> Result<()> {
        self.put(dotted, value);
        self.save()
    }
}

/// A tracked process.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProcRec {
    pub pid: u32,
    #[serde(default)]
    pub ctime: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Runtime state shared by the GUI and the CLI (both can run at once), so it is re-read
/// before every change.
pub struct State {
    store: JsonStore,
    write: Mutex<()>,
}

impl State {
    pub fn open(path: PathBuf) -> Result<Self> {
        Ok(State { store: JsonStore::open(path, json!({"procs": {}}))?, write: Mutex::new(()) })
    }

    pub fn procs(&self, sid: &str) -> Vec<ProcRec> {
        let _ = self.store.load();
        serde_json::from_value(self.store.get(&format!("procs.{sid}"))).unwrap_or_default()
    }

    pub fn set_procs(&self, sid: &str, recs: &[ProcRec]) -> Result<()> {
        let _g = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.store.load();
        let mut procs = self.store.get("procs");
        if !procs.is_object() {
            procs = json!({});
        }
        if recs.is_empty() {
            procs.as_object_mut().unwrap().remove(sid);
        } else {
            procs[sid] = serde_json::to_value(recs)?;
        }
        self.store.put("procs", procs);
        self.store.save()
    }

    pub fn value(&self, key: &str) -> Value {
        let _ = self.store.load();
        self.store.get(key)
    }

    pub fn put(&self, key: &str, value: Value) -> Result<()> {
        let _g = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.store.load();
        self.store.set(key, value)
    }
}
