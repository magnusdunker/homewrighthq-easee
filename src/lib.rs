//! Easee charging robots through Easee's cloud, as an external integration
//! for HomewrightHQ.
//!
//! The user logs in with the user name and password from the Easee app.
//! Each charger becomes a device with status, power, session and lifetime
//! energy, an online sensor, a switch that starts and pauses charging and a
//! number for the dynamic charger current. The services
//! `easee.set_charging_current` and `easee.send_command` let an automation
//! charge in the cheap hours. Chargers are read every minute by default, and
//! a 429 answer pauses polling.

pub mod api;
pub mod model;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use homewright_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ApiError, Client};
use model::ChargerInfo;

pub const DOMAIN: &str = "easee";
/// Default and allowed range for reading the chargers, in seconds.
pub const DEFAULT_SCAN_SECONDS: u64 = 60;
const MIN_SCAN_SECONDS: u64 = 30;
const MAX_SCAN_SECONDS: u64 = 600;
/// How often the configuration (maximum current) is read again.
const CONFIG_POLL: Duration = Duration::from_secs(3600);
/// How long after a command the charger is read again.
const AFTER_COMMAND: Duration = Duration::from_secs(10);
/// The pause after a 429 without Retry-After.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(5 * 60);

/// Logging to HomewrightHQ's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    homewright_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

fn login_schema(username: &str) -> Value {
    json!({
        "type": "object",
        "required": ["username", "password"],
        "properties": {
            "username": { "type": "string", "title": "Brugernavn", "default": username, "description": "E-mail eller telefonnummer fra Easee-appen" },
            "password": { "type": "string", "format": "password", "title": "Adgangskode" }
        }
    })
}

fn options_schema(seconds: u64) -> Value {
    json!({
        "type": "object",
        "properties": {
            "scan_interval": {
                "type": "integer", "title": "Sekunder mellem opdateringer", "default": seconds,
                "minimum": MIN_SCAN_SECONDS, "maximum": MAX_SCAN_SECONDS,
                "description": "Easee begrænser, hvor tit man må spørge. Hver ladeboks hentes for sig."
            }
        }
    })
}

fn base_url(get: impl Fn(&str) -> Option<String>) -> String {
    get("base_url").unwrap_or_else(|| api::BASE_URL.to_string())
}

fn error_code(e: &ApiError) -> &'static str {
    match e {
        ApiError::Auth => "invalid_auth",
        ApiError::RateLimited(_) => "rate_limited",
        ApiError::Other(_) => "cannot_connect",
    }
}

pub fn scan_interval(entry: &Entry) -> Duration {
    let s = entry
        .options
        .get("scan_interval")
        .or_else(|| entry.get("scan_interval"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SCAN_SECONDS)
        .clamp(MIN_SCAN_SECONDS, MAX_SCAN_SECONDS);
    Duration::from_secs(s)
}

fn service_descriptions() -> Vec<(&'static str, Value)> {
    let target = json!({ "entity": { "integration": DOMAIN } });
    let charger = json!({
        "name": "Ladeboks-id", "description": "Ladeboksens id (f.eks. EH123456). Kan udelades, når en entity er valgt, eller kontoen kun har én ladeboks.",
        "required": false, "example": "EH123456", "selector": { "text": {} }, "advanced": true
    });
    vec![
        (
            "set_charging_current",
            json!({
                "name": "Sæt ladestrøm",
                "description": "Sætter ladeboksens dynamiske strømgrænse. 0 A stopper ladningen uden at afslutte den.",
                "target": target,
                "fields": {
                    "current": { "name": "Strøm", "description": "Ampere pr. fase", "required": true, "example": 16, "selector": { "number": { "min": 0, "max": 32, "step": 1, "unit": "A" } } },
                    "charger_id": charger
                }
            }),
        ),
        (
            "send_command",
            json!({
                "name": "Send kommando",
                "description": "Starter, stopper, sætter ladningen på pause eller fortsætter den.",
                "target": target,
                "fields": {
                    "command": { "name": "Kommando", "required": true, "example": "pause", "selector": { "select": { "options": ["start", "stop", "pause", "resume"] } } },
                    "charger_id": charger
                }
            }),
        ),
    ]
}

#[derive(Default)]
pub struct Easee {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
}

impl Easee {
    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }

    /// The chargers a domain service call is for: the ones behind the given
    /// entities, the given charger id, or the only charger there is.
    fn targets(&self, data: &Map<String, Value>, entity_ids: &[String]) -> Result<Vec<(Arc<Account>, String)>, Error> {
        let accounts: Vec<Arc<Account>> = self.accounts.lock().unwrap().values().cloned().collect();
        let mut out: Vec<(Arc<Account>, String)> = vec![];
        let mut push = |a: &Arc<Account>, id: String| {
            if !out.iter().any(|(b, i)| Arc::ptr_eq(a, b) && *i == id) {
                out.push((a.clone(), id));
            }
        };
        let mut ids: Vec<String> = entity_ids.to_vec();
        match data.get("entity_id") {
            Some(Value::String(s)) => ids.push(s.clone()),
            Some(Value::Array(a)) => ids.extend(a.iter().filter_map(Value::as_str).map(String::from)),
            _ => {}
        }
        for e in &ids {
            let found = accounts.iter().find_map(|a| a.charger_of(e).map(|c| (a, c)));
            match found {
                Some((a, c)) => push(a, c),
                None => return Err(Error::new("not_found", format!("{e} er ikke en Easee-entity"))),
            }
        }
        if let Some(id) = data.get("charger_id").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            let id = id.trim().to_uppercase();
            let a = accounts.iter().find(|a| a.chargers.lock().unwrap().contains_key(&id)).ok_or_else(|| Error::new("not_found", format!("ladeboks {id}")))?;
            push(a, id);
        }
        if out.is_empty() {
            let all: Vec<(Arc<Account>, String)> =
                accounts.iter().flat_map(|a| a.chargers.lock().unwrap().keys().map(|k| (a.clone(), k.clone())).collect::<Vec<_>>()).collect();
            if all.len() == 1 {
                out = all;
            } else {
                return Err(Error::invalid("vælg en ladeboks"));
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl Integration for Easee {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        match (step, input) {
            ("user", None) => Ok(FlowReply::form("user", login_schema(""))),
            ("reauth", None) => {
                let user = flow.entry.as_ref().and_then(|e| e.get_str("username")).unwrap_or_default().to_string();
                Ok(FlowReply::form("user", login_schema(&user)))
            }
            ("user", Some(v)) => {
                let username = v["username"].as_str().unwrap_or_default().trim().to_string();
                let password = v["password"].as_str().unwrap_or_default().to_string();
                let base = base_url(|k| flow.data.get(k).and_then(Value::as_str).map(String::from));
                let client = Client::new(&base, &username, &password);
                let chargers = match client.chargers().await {
                    Ok(list) => model::parse_chargers(&list),
                    Err(e) => {
                        info!("{e}");
                        return Ok(FlowReply::form_with_errors("user", login_schema(&username), json!({ "base": error_code(&e) })));
                    }
                };
                if flow.source == "reauth" {
                    let mut data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
                    data.insert("username".into(), json!(username));
                    data.insert("password".into(), json!(password));
                    return Ok(FlowReply::create("", Value::Object(data)));
                }
                if chargers.is_empty() {
                    return Ok(FlowReply::abort("no_devices_found"));
                }
                let mut data = flow.data.clone();
                data.insert("username".into(), json!(username));
                data.insert("password".into(), json!(password));
                Ok(FlowReply::create(&format!("Easee ({username})"), Value::Object(data)).with_unique_id(username.to_lowercase()))
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, _step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let current = flow.entry.as_ref().map(|e| scan_interval(e).as_secs()).unwrap_or(DEFAULT_SCAN_SECONDS);
        match input {
            None => Ok(FlowReply::form("init", options_schema(current))),
            Some(v) => {
                let s = v["scan_interval"].as_u64().unwrap_or(current).clamp(MIN_SCAN_SECONDS, MAX_SCAN_SECONDS);
                Ok(FlowReply::create("", json!({ "scan_interval": s })))
            }
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let username = entry.get_str("username").ok_or_else(|| Error::auth_failed("brugernavn mangler"))?.to_string();
        let password = entry.get_str("password").unwrap_or_default().to_string();
        let client = Arc::new(Client::new(&base_url(|k| entry.get_str(k).map(String::from)), &username, &password));

        let chargers = match client.chargers().await {
            Ok(list) => model::parse_chargers(&list),
            Err(ApiError::Auth) => return Err(Error::auth_failed("Easee afviste brugernavn eller adgangskode")),
            Err(e) => return Err(Error::not_ready(e)),
        };
        let _ = host.delete_issue(&entry_id, "auth").await;

        let account = Arc::new(Account {
            entry_id: entry_id.clone(),
            host: host.clone(),
            client: client.clone(),
            chargers: Mutex::new(HashMap::new()),
            entities: Mutex::new(vec![]),
            task: Mutex::new(None),
            paused_until: Mutex::new(None),
        });
        for info in chargers {
            let state = client.state(&info.id).await.map_err(Error::not_ready)?.as_object().cloned().unwrap_or_default();
            let config = match client.config(&info.id).await {
                Ok(c) => c.as_object().cloned().unwrap_or_default(),
                Err(e) => {
                    warn!("kunne ikke læse konfigurationen for {}: {e}", info.id);
                    Map::new()
                }
            };
            account.register(&info, &config).await?;
            let id = info.id.clone();
            account.chargers.lock().unwrap().insert(id.clone(), Charger { info, state, config, available: true });
            account.publish(&id).await;
        }
        for (name, desc) in service_descriptions() {
            if let Err(e) = host.register_service(&entry_id, name, Some(desc)).await {
                // a second account finds the services already registered
                info!("tjenesten {DOMAIN}.{name}: {e}");
            }
        }

        let interval = scan_interval(&entry);
        let acc = account.clone();
        let task = tokio::spawn(async move {
            let mut last_config = Instant::now();
            loop {
                tokio::time::sleep(interval).await;
                let config_due = last_config.elapsed() >= CONFIG_POLL;
                acc.poll(config_due).await;
                if config_due {
                    last_config = Instant::now();
                }
            }
        });
        *account.task.lock().unwrap() = Some(task);
        if let Some(old) = self.accounts.lock().unwrap().insert(entry_id, account) {
            old.stop();
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, data: Map<String, Value>) -> Result<(), Error> {
        self.account(entry_id)?.call_service(entity_id, service, &data).await
    }

    async fn domain_service(&self, _host: &Host, _entry_id: &str, service: &str, data: Map<String, Value>, entity_ids: Vec<String>) -> Result<Value, Error> {
        match service {
            "set_charging_current" => {
                for (account, id) in self.targets(&data, &entity_ids)? {
                    account.set_current(&id, data.get("current")).await?;
                }
            }
            "send_command" => {
                let name = data.get("command").and_then(Value::as_str).unwrap_or_default();
                let command = model::named_command(name).ok_or_else(|| Error::invalid(format!("ukendt kommando: {name}")))?;
                for (account, id) in self.targets(&data, &entity_ids)? {
                    account.command(&id, command).await?;
                }
            }
            _ => return Err(Error::not_supported(service)),
        }
        Ok(Value::Null)
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = entry.data.clone();
        if let Value::Object(m) = &mut d {
            m.insert("password".into(), json!("**REDACTED**"));
        }
        let chargers: Vec<Value> = self
            .accounts
            .lock()
            .unwrap()
            .get(&entry.entry_id)
            .map(|a| {
                a.chargers
                    .lock()
                    .unwrap()
                    .values()
                    .map(|c| json!({ "id": c.info.id, "available": c.available, "state": c.state, "config": c.config }))
                    .collect()
            })
            .unwrap_or_default();
        json!({ "entry": d, "chargers": chargers })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Status,
    Power,
    SessionEnergy,
    LifetimeEnergy,
    Online,
    Charging,
    Current,
}

struct Charger {
    info: ChargerInfo,
    /// The last answer from `/state`.
    state: Map<String, Value>,
    /// The last answer from `/config`.
    config: Map<String, Value>,
    available: bool,
}

struct Account {
    entry_id: String,
    host: Host,
    client: Arc<Client>,
    chargers: Mutex<HashMap<String, Charger>>,
    /// (entity_id, charger id, kind)
    entities: Mutex<Vec<(String, String, Kind)>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    paused_until: Mutex<Option<Instant>>,
}

type StateUpdate = (String, Map<String, Value>);

fn plain(v: Option<impl ToString>) -> Option<StateUpdate> {
    v.map(|v| (v.to_string(), Map::new()))
}

impl Account {
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }

    fn charger_of(&self, entity_id: &str) -> Option<String> {
        self.entities.lock().unwrap().iter().find(|(e, _, _)| e == entity_id).map(|(_, c, _)| c.clone())
    }

    async fn register(&self, info: &ChargerInfo, config: &Map<String, Value>) -> Result<(), Error> {
        let host = &self.host;
        let dev = host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), info.id.clone())],
                    name: info.name.clone(),
                    manufacturer: Some("Easee".into()),
                    serial_number: Some(info.id.clone()),
                    configuration_url: Some("https://easee.cloud".into()),
                    ..Default::default()
                },
            )
            .await?;
        let id = &info.id;
        let entity = |key: &str, name: &str, domain: &str| Entity::new(domain, format!("{id}-{key}")).name(name).device(&dev);
        let max = model::max_current(config);
        let list: Vec<(Kind, Entity, bool)> = vec![
            (
                Kind::Status,
                entity("status", "Status", "sensor").device_class("enum").icon("mdi:ev-station").capabilities(json!({ "options": model::STATUS })),
                false,
            ),
            (Kind::Power, entity("power", "Effekt", "sensor").device_class("power").unit("W").state_class("measurement"), false),
            (Kind::SessionEnergy, entity("session_energy", "Energi denne ladning", "sensor").device_class("energy").unit("kWh").state_class("total"), false),
            (Kind::LifetimeEnergy, entity("lifetime_energy", "Energi i alt", "sensor").device_class("energy").unit("kWh").state_class("total_increasing"), false),
            (Kind::Online, entity("online", "Forbundet", "binary_sensor").device_class("connectivity").category("diagnostic"), false),
            (Kind::Charging, entity("charging", "Opladning", "switch").icon("mdi:ev-plug-type2"), true),
            (
                Kind::Current,
                entity("dynamic_current", "Dynamisk strømgrænse", "number")
                    .device_class("current")
                    .unit("A")
                    .icon("mdi:current-ac")
                    .capabilities(json!({ "min": 0.0, "max": max, "step": 1.0, "mode": "slider" })),
                true,
            ),
        ];
        for (kind, e, handler) in list {
            if let Some(entity_id) = host.add_entity(&self.entry_id, e, handler).await? {
                self.entities.lock().unwrap().push((entity_id, id.clone(), kind));
            }
        }
        Ok(())
    }

    /// Push states for every entity of a charger.
    async fn publish(&self, id: &str) {
        let (available, updates): (bool, Vec<(String, Kind, Option<StateUpdate>)>) = {
            let chargers = self.chargers.lock().unwrap();
            let Some(c) = chargers.get(id) else { return };
            let s = &c.state;
            let list = self
                .entities
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, d, _)| d == id)
                .map(|(entity_id, _, kind)| {
                    let value = match kind {
                        Kind::Status => plain(model::status(s)),
                        Kind::Power => plain(model::power_w(s)),
                        Kind::SessionEnergy => plain(model::session_energy(s)),
                        Kind::LifetimeEnergy => plain(model::lifetime_energy(s)),
                        Kind::Online => Some((if model::online(s) { "on" } else { "off" }.to_string(), Map::new())),
                        Kind::Charging => Some((if model::charging(s) { "on" } else { "off" }.to_string(), Map::new())),
                        Kind::Current => model::dynamic_current(s).map(|a| (a.to_string(), model::number_attributes(model::max_current(&c.config)))),
                    };
                    (entity_id.clone(), *kind, value)
                })
                .collect();
            (c.available, list)
        };
        for (entity_id, kind, value) in updates {
            let res = match value {
                // the online sensor tells that the cloud lost the charger
                _ if !available && kind != Kind::Online => self.host.set_unavailable(&entity_id).await,
                _ if !available => self.host.set_state(&entity_id, "off", Map::new()).await,
                Some((state, attrs)) => self.host.set_state(&entity_id, state, attrs).await,
                None => self.host.set_state(&entity_id, "unknown", Map::new()).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {entity_id}: {e}");
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused_until.lock().unwrap().is_some_and(|t| Instant::now() < t)
    }

    /// Read one charger and publish it.
    async fn refresh(&self, id: &str) -> Result<(), ApiError> {
        let res = self.client.state(id).await;
        match &res {
            Ok(v) => {
                let _ = self.host.delete_issue(&self.entry_id, "auth").await;
                if let (Some(c), Some(s)) = (self.chargers.lock().unwrap().get_mut(id), v.as_object()) {
                    c.state = s.clone();
                    c.available = true;
                }
            }
            Err(e) => {
                self.report(e).await;
                if !matches!(e, ApiError::RateLimited(_)) {
                    if let Some(c) = self.chargers.lock().unwrap().get_mut(id) {
                        c.available = false;
                    }
                }
            }
        }
        self.publish(id).await;
        res.map(|_| ())
    }

    async fn poll(&self, config_due: bool) {
        if self.paused() {
            return;
        }
        let ids: Vec<String> = self.chargers.lock().unwrap().keys().cloned().collect();
        for id in ids {
            if config_due {
                if let Ok(c) = self.client.config(&id).await {
                    if let (Some(ch), Some(c)) = (self.chargers.lock().unwrap().get_mut(&id), c.as_object()) {
                        ch.config = c.clone();
                    }
                }
            }
            if let Err(ApiError::RateLimited(_)) = self.refresh(&id).await {
                return;
            }
        }
    }

    async fn report(&self, e: &ApiError) {
        match e {
            ApiError::Auth => {
                let r = self
                    .host
                    .create_issue(
                        &self.entry_id,
                        "auth",
                        "error",
                        "Easee: login afvist",
                        "Easee afviste brugernavn eller adgangskode. Godkend integrationen igen med de rigtige oplysninger.",
                    )
                    .await;
                if let Err(e) = r {
                    warn!("kunne ikke oprette problem: {e}");
                }
            }
            ApiError::RateLimited(after) => {
                let pause = after.unwrap_or(RATE_LIMIT_PAUSE);
                warn!("Easee svarede 429; venter {} sekunder", pause.as_secs());
                *self.paused_until.lock().unwrap() = Some(Instant::now() + pause);
            }
            ApiError::Other(err) => warn!("{err}"),
        }
    }

    /// Read the charger again a little after a command.
    fn refresh_later(self: &Arc<Self>, id: &str) {
        let (acc, id) = (self.clone(), id.to_string());
        tokio::spawn(async move {
            tokio::time::sleep(AFTER_COMMAND).await;
            if !acc.paused() {
                let _ = acc.refresh(&id).await;
            }
        });
    }

    fn check_paused(&self) -> Result<(), Error> {
        if self.paused() {
            return Err(Error::unavailable("Easee har bedt om en pause efter for mange forespørgsler"));
        }
        Ok(())
    }

    async fn command(self: &Arc<Self>, id: &str, command: &str) -> Result<(), Error> {
        self.check_paused()?;
        if let Err(e) = self.client.command(id, command).await {
            self.report(&e).await;
            return Err(Error::failed(e));
        }
        if let Some(c) = self.chargers.lock().unwrap().get_mut(id) {
            model::after_command(&mut c.state, command);
        }
        self.publish(id).await;
        self.refresh_later(id);
        Ok(())
    }

    async fn set_current(self: &Arc<Self>, id: &str, value: Option<&Value>) -> Result<(), Error> {
        let max = self.chargers.lock().unwrap().get(id).map(|c| model::max_current(&c.config)).ok_or_else(|| Error::new("not_found", id))?;
        let ampere = model::current(value, max).map_err(Error::invalid)?;
        self.check_paused()?;
        if let Err(e) = self.client.set_dynamic_current(id, ampere).await {
            self.report(&e).await;
            return Err(Error::failed(e));
        }
        if let Some(c) = self.chargers.lock().unwrap().get_mut(id) {
            c.state.insert("dynamicChargerCurrent".into(), json!(ampere));
        }
        self.publish(id).await;
        self.refresh_later(id);
        Ok(())
    }

    async fn call_service(self: &Arc<Self>, entity_id: &str, service: &str, data: &Map<String, Value>) -> Result<(), Error> {
        let (id, kind) = self
            .entities
            .lock()
            .unwrap()
            .iter()
            .find(|(e, _, _)| e == entity_id)
            .map(|(_, d, k)| (d.clone(), *k))
            .ok_or_else(|| Error::new("not_found", entity_id))?;
        match (kind, service) {
            (Kind::Charging, _) => {
                let state = self.chargers.lock().unwrap().get(&id).map(|c| c.state.clone()).ok_or_else(|| Error::unavailable(entity_id))?;
                let command = model::switch_command(service, &state).ok_or_else(|| Error::not_supported(service))?;
                self.command(&id, command).await
            }
            (Kind::Current, "set_value") => self.set_current(&id, data.get("value")).await,
            _ => Err(Error::not_supported(service)),
        }
    }
}
