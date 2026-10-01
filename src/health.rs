//! Google Health API v4 client (Fitbit / Pixel Watch sleep stages and breathing rate).
//! Port of google_health.py; uses the same files in ~/.config/csi-sleep/ (credentials.json =
//! OAuth desktop client, token.json = access + refresh token), so a login from either the Python
//! tools or this app works for both.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use chrono::{DateTime, Local, TimeZone, Utc};
use serde_json::{Value, json};
use sha2::Digest;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const API: &str = "https://health.googleapis.com/v4/users/me/dataTypes";
pub const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/googlehealth.sleep.readonly",
    "https://www.googleapis.com/auth/googlehealth.health_metrics_and_measurements.readonly",
];

pub fn conf_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config").join("csi-sleep")
}
pub fn cred_path() -> PathBuf {
    conf_dir().join("credentials.json")
}
pub fn token_path() -> PathBuf {
    conf_dir().join("token.json")
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn save_private(path: &PathBuf, v: &Value) -> Result<()> {
    std::fs::create_dir_all(conf_dir())?;
    std::fs::write(path, serde_json::to_vec_pretty(v)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn client() -> Result<Value> {
    let p = cred_path();
    let c: Value = serde_json::from_slice(&std::fs::read(&p).with_context(|| format!("no OAuth client at {}", p.display()))?)?;
    Ok(c.get("installed").or_else(|| c.get("web")).cloned().ok_or_else(|| anyhow!("credentials.json has no client"))?)
}

/// Copy an OAuth client JSON into the config folder.
pub fn install_credentials(src: &std::path::Path) -> Result<()> {
    let v: Value = serde_json::from_slice(&std::fs::read(src)?)?;
    if v.get("installed").is_none() && v.get("web").is_none() {
        bail!("not an OAuth client file (expected an \"installed\" desktop client)");
    }
    save_private(&cred_path(), &v)
}

pub struct Status {
    pub has_client: bool,
    pub has_token: bool,
    pub token_valid: bool,
}
pub fn status() -> Status {
    let tok: Option<Value> = std::fs::read(token_path()).ok().and_then(|b| serde_json::from_slice(&b).ok());
    Status {
        has_client: cred_path().exists(),
        has_token: tok.is_some(),
        token_valid: tok.and_then(|t| t["expires_at"].as_f64()).map(|e| now() < e).unwrap_or(false),
    }
}

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn urlenc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn urldec(s: &str) -> String {
    let b = s.as_bytes();
    let mut o = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                o.push(v);
                i += 3;
                continue;
            }
        }
        o.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&o).into_owned()
}

/// Desktop OAuth with a loopback redirect and PKCE: opens the browser, waits up to 5 min.
pub fn login() -> Result<String> {
    use rand::Rng;
    let c = client()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://localhost:{port}");
    let mut rng = rand::rng();
    let verifier = b64url(&(0..48).map(|_| rng.random::<u8>()).collect::<Vec<_>>());
    let state = b64url(&(0..12).map(|_| rng.random::<u8>()).collect::<Vec<_>>());
    let challenge = b64url(&sha2::Sha256::digest(verifier.as_bytes()));
    let url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}&code_challenge_method=S256&code_challenge={}",
        c["auth_uri"].as_str().unwrap_or("https://accounts.google.com/o/oauth2/auth"),
        urlenc(c["client_id"].as_str().unwrap_or_default()),
        urlenc(&redirect),
        urlenc(&SCOPES.join(" ")),
        state,
        challenge
    );
    let _ = open::that(&url);
    listener.set_nonblocking(true)?;
    let deadline = now() + 300.0;
    let (code, got_state) = loop {
        if now() > deadline {
            bail!("no answer from the browser within 5 min");
        }
        match listener.accept() {
            Ok((mut s, _)) => {
                s.set_nonblocking(false)?;
                s.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut buf = [0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("");
                let q = path.split_once('?').map(|x| x.1).unwrap_or("");
                let params: std::collections::HashMap<String, String> =
                    q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), urldec(v))).collect();
                let ok = params.contains_key("code");
                let body = if ok {
                    "Done: you can close this tab and go back to CSI Breathing.".to_string()
                } else {
                    format!("Login failed: {}", params.get("error").cloned().unwrap_or_else(|| "?".into()))
                };
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                if ok {
                    break (params["code"].clone(), params.get("state").cloned().unwrap_or_default());
                }
                if let Some(e) = params.get("error") {
                    bail!("login failed: {e}");
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.into()),
        }
    };
    if got_state != state {
        bail!("login failed: state mismatch");
    }
    let resp: Value = ureq::post(c["token_uri"].as_str().unwrap_or("https://oauth2.googleapis.com/token"))
        .send_form([
            ("code", code.as_str()),
            ("client_id", c["client_id"].as_str().unwrap_or_default()),
            ("client_secret", c["client_secret"].as_str().unwrap_or_default()),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .map_err(|e| anyhow!("token exchange failed: {e}"))?
        .body_mut()
        .read_json()?;
    let mut tok = resp.clone();
    tok["expires_at"] = json!(now() + resp["expires_in"].as_f64().unwrap_or(3600.0) - 60.0);
    save_private(&token_path(), &tok)?;
    Ok(resp["scope"].as_str().unwrap_or("").to_string())
}

fn access_token() -> Result<String> {
    let mut tok: Value = serde_json::from_slice(&std::fs::read(token_path()).map_err(|_| anyhow!("not logged in to Google Health"))?)?;
    if now() < tok["expires_at"].as_f64().unwrap_or(0.0) {
        return Ok(tok["access_token"].as_str().unwrap_or_default().to_string());
    }
    let c = client()?;
    let r = ureq::post(c["token_uri"].as_str().unwrap_or("https://oauth2.googleapis.com/token")).send_form([
        ("client_id", c["client_id"].as_str().unwrap_or_default()),
        ("client_secret", c["client_secret"].as_str().unwrap_or_default()),
        ("refresh_token", tok["refresh_token"].as_str().unwrap_or_default()),
        ("grant_type", "refresh_token"),
    ]);
    let mut r = r.map_err(|e| anyhow!("Google login expired ({e}; logins made while the Cloud app is in Testing last 7 days): log in again"))?;
    let new: Value = r.body_mut().read_json()?;
    for (k, v) in new.as_object().unwrap() {
        tok[k] = v.clone();
    }
    tok["expires_at"] = json!(now() + new["expires_in"].as_f64().unwrap_or(3600.0) - 60.0);
    save_private(&token_path(), &tok)?;
    Ok(tok["access_token"].as_str().unwrap_or_default().to_string())
}

/// Check the login works (refreshes the token if needed).
pub fn check() -> Result<()> {
    access_token().map(|_| ())
}

pub fn list_points(dtype: &str, filter: &str) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut req = ureq::get(&format!("{API}/{dtype}/dataPoints"))
            .header("Authorization", &format!("Bearer {}", access_token()?))
            .query("filter", filter);
        if let Some(t) = &token {
            req = req.query("pageToken", t);
        }
        let mut resp = req.call().map_err(|e| anyhow!("{dtype}: {e}"))?;
        let j: Value = resp.body_mut().read_json()?;
        if let Some(a) = j["dataPoints"].as_array() {
            out.extend(a.iter().cloned());
        }
        match j["nextPageToken"].as_str() {
            Some(t) if !t.is_empty() => token = Some(t.to_string()),
            _ => return Ok(out),
        }
    }
}

fn iso(t: f64) -> String {
    Utc.timestamp_opt(t as i64, 0).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn ts(s: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp() as f64 + d.timestamp_subsec_nanos() as f64 * 1e-9)
}

pub fn sleep_sessions(t_from: f64, t_to: f64) -> Result<Vec<Value>> {
    list_points("sleep", &format!("sleep.interval.end_time >= \"{}\" AND sleep.interval.end_time < \"{}\"", iso(t_from), iso(t_to)))
}

pub fn session_span(dp: &Value) -> Option<(f64, f64)> {
    let iv = &dp["sleep"]["interval"];
    Some((ts(iv["startTime"].as_str()?)?, ts(iv["endTime"].as_str()?)?))
}

/// (start, end, "wake"|"light"|"deep"|"rem") of one session.
pub fn stages(dp: &Value) -> Vec<(f64, f64, String)> {
    let mut out: Vec<(f64, f64, String)> = dp["sleep"]["stages"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let typ = match s["type"].as_str()? {
                        "AWAKE" => "wake",
                        "LIGHT" => "light",
                        "DEEP" => "deep",
                        "REM" => "rem",
                        _ => return None,
                    };
                    Some((ts(s["startTime"].as_str()?)?, ts(s["endTime"].as_str()?)?, typ.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    out
}

/// Start times of the short awakenings (the app's "restless" marks).
pub fn restless(dp: &Value) -> Vec<f64> {
    let mut v: Vec<f64> = dp["sleep"]["shortAwakenings"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| ts(s["startTime"].as_str()?)).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v
}

/// Breathing rate per stage for the session ending at t_end: [(stage, bpm, sd, snr)].
pub fn respiration(t_end: f64) -> Result<Vec<(String, f64, f64, f64)>> {
    let f = "respiratory_rate_sleep_summary.sample_time.physical_time";
    let pts = list_points("respiratory-rate-sleep-summary", &format!("{f} >= \"{}\" AND {f} < \"{}\"", iso(t_end - 3600.0), iso(t_end + 3600.0)))?;
    let best = pts.iter().min_by(|a, b| {
        let d = |p: &Value| (ts(p["respiratoryRateSleepSummary"]["sampleTime"]["physicalTime"].as_str().unwrap_or("")).unwrap_or(0.0) - t_end).abs();
        d(a).partial_cmp(&d(b)).unwrap()
    });
    let mut out = vec![];
    if let Some(p) = best {
        let r = &p["respiratoryRateSleepSummary"];
        for k in ["deep", "light", "rem", "full"] {
            let s = &r[format!("{k}SleepStats")];
            let bpm = s["breathsPerMinute"].as_f64().unwrap_or(0.0);
            if bpm > 0.0 {
                out.push((k.to_string(), bpm, s["standardDeviation"].as_f64().unwrap_or(0.0), s["signalToNoise"].as_f64().unwrap_or(0.0)));
            }
        }
    }
    Ok(out)
}

/// The session overlapping [t_start, t_end] most.
pub fn night_reference(t_start: f64, t_end: f64) -> Result<Option<Value>> {
    let mut best = None;
    let mut ov = 0.0;
    for dp in sleep_sessions(t_start - 6.0 * 3600.0, t_end + 18.0 * 3600.0)? {
        let sp = session_span(&dp).or_else(|| {
            let st = stages(&dp);
            if st.is_empty() { None } else { Some((st[0].0, st[st.len() - 1].1)) }
        });
        if let Some((a, b)) = sp {
            let o = b.min(t_end) - a.max(t_start);
            if o > ov {
                ov = o;
                best = Some(dp);
            }
        }
    }
    Ok(best)
}

/// Local midnight .. next midnight of a date (for "sessions ending on this day").
pub fn day_range(y: i32, m: u32, d: u32) -> (f64, f64) {
    let a = Local.with_ymd_and_hms(y, m, d, 0, 0, 0).earliest().unwrap().timestamp() as f64;
    (a, a + 86400.0)
}
