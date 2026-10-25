//! Mapping tests on responses shaped like Easee's, and the plugin against a
//! fake Easee cloud with HomewrightHQ played over a pipe.

mod host;

use homewright_easee::api::{ApiError, Client};
use homewright_easee::model;
use homewright_easee::Easee;
use serde_json::{json, Map, Value};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

#[test]
fn chargers_and_state() {
    let c = model::parse_chargers(&fixture("chargers.json"));
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].id, "EH123456");
    assert_eq!(c[0].name, "Carport");

    let s = obj(fixture("state_charging.json"));
    assert_eq!(model::status(&s), Some("charging"));
    assert_eq!(model::power_w(&s), Some(7360.0));
    assert_eq!(model::session_energy(&s), Some(12.48));
    assert_eq!(model::lifetime_energy(&s), Some(4521.73));
    assert_eq!(model::dynamic_current(&s), Some(16.0));
    assert!(model::online(&s));
    assert!(model::charging(&s));
    assert_eq!(model::max_current(&obj(fixture("config.json"))), 16.0);
    assert_eq!(model::max_current(&Map::new()), model::DEFAULT_MAX_CURRENT);

    let p = obj(fixture("state_paused.json"));
    assert_eq!(model::status(&p), Some("awaiting_start"));
    assert!(!model::charging(&p));

    let mut off = s.clone();
    off.insert("isOnline".into(), json!(false));
    assert!(!model::online(&off));
    off.insert("chargerOpMode".into(), json!(42));
    assert_eq!(model::status(&off), None);
}

#[test]
fn commands() {
    let charging = obj(fixture("state_charging.json"));
    let paused = obj(fixture("state_paused.json"));
    assert_eq!(model::switch_command("turn_on", &paused), Some("resume_charging"));
    let mut done = paused.clone();
    done.insert("chargerOpMode".into(), json!(4));
    assert_eq!(model::switch_command("turn_on", &done), Some("start_charging"));
    assert_eq!(model::switch_command("turn_off", &charging), Some("pause_charging"));
    assert_eq!(model::switch_command("toggle", &charging), Some("pause_charging"));
    assert_eq!(model::switch_command("toggle", &paused), Some("resume_charging"));
    assert_eq!(model::switch_command("open", &paused), None);
    assert_eq!(model::named_command("stop"), Some("stop_charging"));
    assert_eq!(model::named_command("boost"), None);

    assert_eq!(model::current(Some(&json!(10.4)), 16.0), Ok(10.0));
    assert_eq!(model::current(Some(&json!("20")), 16.0), Ok(16.0));
    assert!(model::current(Some(&json!(-1)), 16.0).is_err());
    assert!(model::current(None, 16.0).is_err());

    let mut s = paused.clone();
    model::after_command(&mut s, "resume_charging");
    assert_eq!(model::status(&s), Some("charging"));
    let mut d = paused.clone();
    d.insert("chargerOpMode".into(), json!(1));
    model::after_command(&mut d, "start_charging");
    assert_eq!(model::status(&d), Some("disconnected"));
}

async fn cloud(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/login"))
        .and(body_json(json!({ "userName": "a@b.dk", "password": "secret" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("login.json")))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/login"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "errorCode": 100, "errorCodeName": "InvalidUserPassword" })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/chargers"))
        .and(header("Authorization", "Bearer ACCESS-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("chargers.json")))
        .mount(server)
        .await;
    for (id, file) in [("EH123456", "state_charging.json"), ("EH654321", "state_paused.json")] {
        Mock::given(method("GET"))
            .and(path(format!("/api/chargers/{id}/state")))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(file)))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/chargers/EH123456/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("config.json")))
        .mount(server)
        .await;
    // the second charger's configuration cannot be read; 32 A is used
    Mock::given(method("GET"))
        .and(path("/api/chargers/EH654321/config"))
        .respond_with(ResponseTemplate::new(500))
        .mount(server)
        .await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(202)).mount(server).await;
}

fn flow(data: Value) -> Value {
    json!({ "flow_id": "f1", "source": "user", "data": data })
}

#[tokio::test(flavor = "multi_thread")]
async fn flow_setup_and_commands() {
    let server = MockServer::start().await;
    cloud(&server).await;
    let h = host::FakeHost::start(Easee::default());
    let base = json!({ "base_url": server.uri() });

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": null })).await.unwrap();
    let keys: Vec<&String> = r["result"]["schema"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["username", "password"]);

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": { "username": "a@b.dk", "password": "wrong" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_auth");

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": { "username": " a@b.dk ", "password": "secret" } })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "Easee (a@b.dk)");
    assert_eq!(r["unique_id"], "a@b.dk");
    let data = r["result"]["data"].clone();

    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "easee", "data": data } })).await.unwrap();
    assert_eq!(h.rec.lock().unwrap().devices.len(), 2);
    let services: Vec<String> = h.rec.lock().unwrap().services.iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(services, ["set_charging_current", "send_command"]);

    assert_eq!(h.state(&h.entity("EH123456-status")).unwrap().0, "charging");
    assert_eq!(h.state(&h.entity("EH123456-power")).unwrap().0, "7360");
    assert_eq!(h.state(&h.entity("EH123456-session_energy")).unwrap().0, "12.48");
    assert_eq!(h.state(&h.entity("EH123456-lifetime_energy")).unwrap().0, "4521.73");
    assert_eq!(h.state(&h.entity("EH123456-online")).unwrap().0, "on");
    assert_eq!(h.state(&h.entity("EH123456-charging")).unwrap().0, "on");
    let (state, attrs) = h.state(&h.entity("EH123456-dynamic_current")).unwrap();
    assert_eq!(state, "16");
    assert_eq!(attrs["max"], 16.0);
    assert_eq!(h.state(&h.entity("EH654321-dynamic_current")).unwrap().1["max"], 32.0);
    assert_eq!(h.state(&h.entity("EH654321-charging")).unwrap().0, "off");
    let switch = h.entity("EH654321-charging");
    assert!(h.rec.lock().unwrap().entities[&switch].1);

    let call = |entity: String, service: &str, data: Value| json!({ "entry_id": "e1", "entity_id": entity, "service": service, "data": data });
    h.call("entity/call_service", call(switch.clone(), "turn_on", json!({}))).await.unwrap();
    assert_eq!(h.state(&switch).unwrap().0, "on");
    h.call("entity/call_service", call(h.entity("EH123456-charging"), "turn_off", json!({}))).await.unwrap();
    h.call("entity/call_service", call(h.entity("EH123456-dynamic_current"), "set_value", json!({ "value": 40 }))).await.unwrap();
    assert_eq!(h.state(&h.entity("EH123456-dynamic_current")).unwrap().0, "16");
    let err = h.call("entity/call_service", call(h.entity("EH123456-power"), "set_value", json!({ "value": 1 }))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "not_supported");

    // the domain services, by entity and by charger id
    let svc = |service: &str, data: Value, ids: Value| json!({ "entry_id": "e1", "service": service, "data": data, "entity_ids": ids, "return_response": false });
    h.call("service/call", svc("set_charging_current", json!({ "current": 8 }), json!([h.entity("EH654321-status")]))).await.unwrap();
    assert_eq!(h.state(&h.entity("EH654321-dynamic_current")).unwrap().0, "8");
    h.call("service/call", svc("send_command", json!({ "command": "stop", "charger_id": "eh123456" }), json!([]))).await.unwrap();
    // two chargers and no target
    let err = h.call("service/call", svc("set_charging_current", json!({ "current": 8 }), json!([]))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");
    let err = h.call("service/call", svc("send_command", json!({ "command": "boost", "charger_id": "EH123456" }), json!([]))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");

    let sent: Vec<(String, Value)> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().starts_with("/api/chargers/"))
        .map(|r| (r.url.path().to_string(), serde_json::from_slice(&r.body).unwrap_or(Value::Null)))
        .collect();
    let paths: Vec<&str> = sent.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/chargers/EH654321/commands/resume_charging",
            "/api/chargers/EH123456/commands/pause_charging",
            "/api/chargers/EH123456/settings",
            "/api/chargers/EH654321/settings",
            "/api/chargers/EH123456/commands/stop_charging",
        ]
    );
    assert_eq!(sent[2].1, json!({ "dynamicChargerCurrent": 16.0 }));
    assert_eq!(sent[3].1, json!({ "dynamicChargerCurrent": 8.0 }));

    let diag = h.call("diagnostics", json!({ "entry": { "entry_id": "e1", "data": { "username": "a@b.dk", "password": "secret" } } })).await.unwrap();
    assert_eq!(diag["entry"]["password"], "**REDACTED**");
    assert_eq!(diag["chargers"].as_array().unwrap().len(), 2);

    let opts = h.call("options_flow", json!({ "flow": { "flow_id": "o1", "source": "options", "entry": { "entry_id": "e1", "data": {} } }, "step": "init", "input": { "scan_interval": 5 } })).await.unwrap();
    assert_eq!(opts["result"]["data"]["scan_interval"], 30);
}

#[tokio::test(flavor = "multi_thread")]
async fn tokens_are_refreshed_and_401_logs_in_again() {
    let server = MockServer::start().await;
    // a token that is about to run out
    Mock::given(method("POST"))
        .and(path("/api/accounts/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "accessToken": "OLD", "refreshToken": "R", "expiresIn": 10 })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/refresh_token"))
        .and(body_json(json!({ "accessToken": "OLD", "refreshToken": "R" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "accessToken": "NEW", "refreshToken": "R2", "expiresIn": 86400 })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/chargers"))
        .and(header("Authorization", "Bearer NEW"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri(), "a@b.dk", "x");
    c.login().await.unwrap();
    assert_eq!(c.chargers().await.unwrap(), json!([]));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("login.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/api/chargers")).respond_with(ResponseTemplate::new(401)).up_to_n_times(1).mount(&server).await;
    Mock::given(method("GET")).and(path("/api/chargers")).respond_with(ResponseTemplate::new(200).set_body_json(json!([]))).mount(&server).await;
    let c = Client::new(&server.uri(), "a@b.dk", "x");
    assert_eq!(c.chargers().await.unwrap(), json!([]));
    let logins = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/api/accounts/login").count();
    assert_eq!(logins, 2);

    let limited = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/login"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
        .mount(&limited)
        .await;
    let c = Client::new(&limited.uri(), "a@b.dk", "x");
    assert_eq!(c.login().await.unwrap_err(), ApiError::RateLimited(Some(std::time::Duration::from_secs(120))));
}
