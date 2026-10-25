//! Easee's data and how it becomes states and commands.

use serde_json::{json, Map, Value};

/// A charger from `GET /api/chargers`.
#[derive(Clone, Debug, PartialEq)]
pub struct ChargerInfo {
    pub id: String,
    pub name: String,
}

pub fn parse_chargers(v: &Value) -> Vec<ChargerInfo> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let id = c["id"].as_str().filter(|s| !s.is_empty())?.to_string();
            let name = c["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(&id).to_string();
            Some(ChargerInfo { id, name })
        })
        .collect()
}

/// `chargerOpMode` to the status sensor's options, in code order.
pub const STATUS: [&str; 9] = [
    "offline",
    "disconnected",
    "awaiting_start",
    "charging",
    "completed",
    "error",
    "ready_to_charge",
    "awaiting_authorization",
    "de_authorizing",
];

pub fn status(state: &Map<String, Value>) -> Option<&'static str> {
    let code = state.get("chargerOpMode")?.as_u64()?;
    STATUS.get(code as usize).copied()
}

fn num(state: &Map<String, Value>, key: &str) -> Option<f64> {
    state.get(key).and_then(Value::as_f64)
}

/// Charging power in W; Easee reports `totalPower` in kW.
pub fn power_w(state: &Map<String, Value>) -> Option<f64> {
    num(state, "totalPower").map(|kw| (kw * 1000.0).round())
}

pub fn session_energy(state: &Map<String, Value>) -> Option<f64> {
    num(state, "sessionEnergy")
}

pub fn lifetime_energy(state: &Map<String, Value>) -> Option<f64> {
    num(state, "lifetimeEnergy")
}

pub fn dynamic_current(state: &Map<String, Value>) -> Option<f64> {
    num(state, "dynamicChargerCurrent")
}

pub fn online(state: &Map<String, Value>) -> bool {
    state.get("isOnline").and_then(Value::as_bool).unwrap_or(true) && status(state) != Some("offline")
}

/// The switch is on while the charger charges or is ready and waiting for the car.
pub fn charging(state: &Map<String, Value>) -> bool {
    matches!(status(state), Some("charging") | Some("ready_to_charge"))
}

/// Default upper limit for the current number when the configuration has none.
pub const DEFAULT_MAX_CURRENT: f64 = 32.0;

pub fn max_current(config: &Map<String, Value>) -> f64 {
    config.get("maxChargerCurrent").and_then(Value::as_f64).filter(|v| *v > 0.0).unwrap_or(DEFAULT_MAX_CURRENT)
}

/// The command for the switch. Turning on resumes a paused session
/// (`awaiting_start`) and starts one otherwise. Turning off pauses.
pub fn switch_command(service: &str, state: &Map<String, Value>) -> Option<&'static str> {
    match service {
        "turn_on" => Some(if status(state) == Some("awaiting_start") { "resume_charging" } else { "start_charging" }),
        "turn_off" => Some("pause_charging"),
        "toggle" => switch_command(if charging(state) { "turn_off" } else { "turn_on" }, state),
        _ => None,
    }
}

/// The command for `easee.send_command`.
pub fn named_command(name: &str) -> Option<&'static str> {
    match name {
        "start" => Some("start_charging"),
        "stop" => Some("stop_charging"),
        "pause" => Some("pause_charging"),
        "resume" => Some("resume_charging"),
        _ => None,
    }
}

/// What the state looks like right after a command, until the next poll.
pub fn after_command(state: &mut Map<String, Value>, command: &str) {
    let mode = match command {
        "start_charging" | "resume_charging" => 3,
        "pause_charging" => 2,
        "stop_charging" => 4,
        _ => return,
    };
    // only when a car is connected; otherwise Easee ignores the command
    if !matches!(status(state), Some("offline") | Some("disconnected") | None) {
        state.insert("chargerOpMode".into(), json!(mode));
    }
}

/// A current in ampere, as a number or a numeric string, rounded and kept in range.
pub fn current(v: Option<&Value>, max: f64) -> Result<f64, String> {
    let a = v.and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).ok_or("strømstyrke mangler")?;
    if !a.is_finite() || a < 0.0 {
        return Err(format!("ugyldig strømstyrke: {a}"));
    }
    Ok(a.round().min(max))
}

/// Number attributes the UI reads.
pub fn number_attributes(max: f64) -> Map<String, Value> {
    let mut a = Map::new();
    a.insert("min".into(), json!(0.0));
    a.insert("max".into(), json!(max));
    a.insert("step".into(), json!(1.0));
    a.insert("mode".into(), json!("slider"));
    a
}
