//! Share a local site on the internet through ngrok.

use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;

use crate::core::App;
use crate::error::service_err;
use crate::procs::{alive, kill_tree, spawn};
use crate::settings::ProcRec;
use crate::sites::Site;
use crate::util::{env_with, fwd};
use crate::Log;

const SID: &str = "share";

pub fn running(app: &App) -> Option<ProcRec> {
    app.state.procs(SID).into_iter().find(alive)
}

pub fn start(app: &App, site: &Site, log: Log) -> Result<String> {
    let v = app.active("ngrok").ok_or_else(|| service_err("ngrok is not installed: Tools > Quick add > ngrok."))?;
    let token = app.secrets.str("ngrok_authtoken");
    if token.is_empty() {
        return Err(service_err("Add your ngrok authtoken in Preferences first (free at dashboard.ngrok.com)."));
    }
    stop(app, log)?;
    let port = app.settings.port("http");
    let cmd = vec![fwd(&v.exe), "http".into(), port.to_string(), format!("--host-header={}", site.host),
                   "--log".into(), "stdout".into()];
    // The token goes in the environment, not on the command line, where any program on the
    // machine could read it from the process list.
    let env = env_with(&HashMap::from([("NGROK_AUTHTOKEN".to_string(), token)]));
    let mut rec = spawn(&cmd, &v.path, Some(&env), &app.paths.logs.join("ngrok.log"))?;
    rec.site = Some(site.name.clone());
    app.state.set_procs(SID, std::slice::from_ref(&rec))?;
    let start = Instant::now();
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).no_proxy().build()?;
    while start.elapsed() < Duration::from_secs(20) {
        if !alive(&rec) {
            app.state.set_procs(SID, &[])?;
            return Err(service_err("ngrok exited. Check logs/ngrok.log (is the authtoken right?)."));
        }
        if let Ok(resp) = client.get("http://127.0.0.1:4040/api/tunnels").send() {
            if let Ok(json) = resp.json::<Value>() {
                let urls: Vec<String> = json["tunnels"].as_array().into_iter().flatten()
                    .filter_map(|t| t["public_url"].as_str().map(str::to_string)).collect();
                if let Some(url) = urls.iter().find(|u| u.starts_with("https")).or(urls.first()) {
                    rec.url = Some(url.clone());
                    app.state.set_procs(SID, std::slice::from_ref(&rec))?;
                    log(&format!("Sharing {} at {url}", site.host));
                    return Ok(url.clone());
                }
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
    stop(app, log)?;
    Err(service_err("ngrok didn't report a public URL within 20 seconds."))
}

pub fn stop(app: &App, log: Log) -> Result<()> {
    if let Some(rec) = running(app) {
        kill_tree(rec.pid, Duration::from_secs(5));
        log("Stopped sharing");
    }
    app.state.set_procs(SID, &[])
}
