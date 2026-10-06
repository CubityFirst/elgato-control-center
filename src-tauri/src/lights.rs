//! Elgato Key Light discovery (mDNS `_elg._tcp`) and HTTP control (port 9123).

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const SERVICE_TYPE: &str = "_elg._tcp.local.";
pub const DEFAULT_PORT: u16 = 9123;
/// Elgato temperature range in mireds (7000 K .. 2900 K).
pub const TEMP_MIN: u16 = 143;
pub const TEMP_MAX: u16 = 344;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Light {
    /// Key in the app's light map: the serial number once known, the IP until then.
    #[serde(default)]
    pub id: String,
    /// From `/elgato/accessory-info`; stays the same when DHCP hands the light a new IP.
    #[serde(default)]
    pub serial: String,
    pub ip: String,
    pub port: u16,
    pub name: String,
    pub product: String,
    /// Added by IP rather than discovered.
    pub manual: bool,
    pub reachable: bool,
    pub on: bool,
    /// 3 ..= 100
    pub brightness: u8,
    /// Mireds, 143 ..= 344
    pub temperature: u16,
    /// Write counter value of the last change made through this app (see `AppState::writes`).
    #[serde(skip)]
    pub rev: u64,
}

impl Light {
    pub fn new(ip: String, port: u16, name: String, manual: bool) -> Self {
        Light {
            id: ip.clone(),
            serial: String::new(),
            ip,
            port,
            name,
            product: String::new(),
            manual,
            reachable: false,
            on: false,
            brightness: 50,
            temperature: 213,
            rev: 0,
        }
    }
}

/// Partial update sent from the UI.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    pub on: Option<bool>,
    pub brightness: Option<u8>,
    pub temperature: Option<u16>,
}

#[derive(Deserialize)]
struct ApiLight {
    #[serde(default)]
    on: u8,
    #[serde(default)]
    brightness: u8,
    #[serde(default)]
    temperature: u16,
}

#[derive(Deserialize)]
struct ApiLights {
    #[serde(default)]
    lights: Vec<ApiLight>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ApiInfo {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    product_name: String,
    #[serde(default)]
    serial_number: String,
}

pub struct State {
    pub on: bool,
    pub brightness: u8,
    pub temperature: u16,
}

pub struct Info {
    pub name: String,
    pub product: String,
    pub serial: String,
}

pub struct Discovered {
    pub ip: String,
    pub port: u16,
    pub name: String,
    pub product: String,
}

/// One shared agent so requests reuse kept-alive connections (slider drags send many).
/// Key Lights hold idle connections for 20 s+; ureq does not retry on a pooled
/// connection the light has since closed, so the pool age stays well below that.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_millis(1500)))
                .max_idle_age(Duration::from_secs(5))
                .build(),
        )
    })
}

fn url(ip: &str, port: u16, path: &str) -> String {
    format!("http://{ip}:{port}/elgato/{path}")
}

fn parse_state(mut resp: ureq::http::Response<ureq::Body>) -> Result<State, String> {
    let body: ApiLights = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    let l = body.lights.into_iter().next().ok_or("no lights in response")?;
    Ok(State {
        on: l.on != 0,
        brightness: l.brightness,
        temperature: l.temperature,
    })
}

pub fn fetch_state(ip: &str, port: u16) -> Result<State, String> {
    let resp = agent()
        .get(&url(ip, port, "lights"))
        .call()
        .map_err(|e| e.to_string())?;
    parse_state(resp)
}

pub fn fetch_info(ip: &str, port: u16) -> Result<Info, String> {
    let mut resp = agent()
        .get(&url(ip, port, "accessory-info"))
        .call()
        .map_err(|e| e.to_string())?;
    let info: ApiInfo = resp.body_mut().read_json().map_err(|e| e.to_string())?;
    Ok(Info {
        name: info.display_name,
        product: info.product_name,
        serial: info.serial_number,
    })
}

pub fn put_state(ip: &str, port: u16, patch: &Patch) -> Result<State, String> {
    let mut light = serde_json::Map::new();
    if let Some(on) = patch.on {
        light.insert("on".into(), json!(if on { 1 } else { 0 }));
    }
    if let Some(b) = patch.brightness {
        light.insert("brightness".into(), json!(b.clamp(3, 100)));
    }
    if let Some(t) = patch.temperature {
        light.insert("temperature".into(), json!(t.clamp(TEMP_MIN, TEMP_MAX)));
    }
    let body = json!({ "numberOfLights": 1, "lights": [light] });
    let resp = agent()
        .put(&url(ip, port, "lights"))
        .send_json(&body)
        .map_err(|e| e.to_string())?;
    parse_state(resp)
}

/// Browse mDNS on every interface for `timeout`, returning unique lights by IP.
pub fn discover(timeout: Duration) -> Vec<Discovered> {
    let mut found = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(_) => return found,
    };
    let rx = match daemon.browse(SERVICE_TYPE) {
        Ok(rx) => rx,
        Err(_) => {
            let _ = daemon.shutdown();
            return found;
        }
    };

    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        match rx.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(svc)) => {
                let name = svc
                    .get_fullname()
                    .split(SERVICE_TYPE.trim_end_matches('.'))
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string();
                let product = svc
                    .get_property_val_str("md")
                    .map(|s| s.split(' ').filter(|w| !w.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                let port = if svc.get_port() == 0 { DEFAULT_PORT } else { svc.get_port() };
                let mut ips: Vec<_> = svc.get_addresses_v4().into_iter().collect();
                ips.sort();
                for ip in ips {
                    let ip = ip.to_string();
                    if seen.insert(ip.clone()) {
                        found.push(Discovered {
                            ip,
                            port,
                            name: name.clone(),
                            product: product.clone(),
                        });
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to a real light: `ELGATO_TEST_LIGHT=10.0.0.5 cargo test -- --ignored`.
    /// Only reads, then writes back the state it read.
    #[test]
    #[ignore]
    fn real_light_round_trip_on_pooled_connection() {
        let ip = std::env::var("ELGATO_TEST_LIGHT").expect("set ELGATO_TEST_LIGHT");
        let info = fetch_info(&ip, DEFAULT_PORT).unwrap();
        assert!(!info.serial.is_empty());
        let before = fetch_state(&ip, DEFAULT_PORT).unwrap();
        // Idle past the point a short keep-alive would have dropped the pooled connection.
        std::thread::sleep(Duration::from_secs(4));
        let patch = Patch {
            brightness: Some(before.brightness),
            ..Default::default()
        };
        let after = put_state(&ip, DEFAULT_PORT, &patch).unwrap();
        assert_eq!(after.brightness, before.brightness);
        assert_eq!(after.on, before.on);
    }
}
