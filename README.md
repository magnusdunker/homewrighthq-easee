# Easee for HomewrightHQ

An external integration for [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Easee chargers (Easee Home and Easee Charge) through Easee's cloud.

## Installation

In HomewrightHQ: **Settings → Integrations → From GitHub**. Paste

```
https://github.com/magnusdunker/homewrighthq-easee
```

and press **Install**. Then add **Easee** under **Add integration**, and log in with the username (email or phone number) and password you use in the Easee app.

## What you get

Each charger becomes a device with:

- status: `offline`, `disconnected`, `awaiting_start`, `charging`, `completed`, `error`, `ready_to_charge`, `awaiting_authorization` or `de_authorizing`
- power in W
- energy in the current charging session and total energy, in kWh
- whether the charger is connected to Easee's cloud
- a switch **Opladning** (charging): on starts charging or resumes a paused session, off pauses it
- a setting **Dynamisk strømgrænse** (dynamic current limit) in amperes, from 0 to the charger's maximum current

There are two services for automations:

- `easee.set_charging_current` with `current` (amperes) sets the dynamic current limit. 0 A holds charging back without ending the session.
- `easee.send_command` with `command` (`start`, `stop`, `pause` or `resume`).

Both take an entity from the charger as target, or `charger_id` (e.g. `EH123456`). If the account has only one charger, both can be left out.

An example that charges in the cheap hours: an automation sets `current` to 16 A when the electricity price is low, and to 0 A when it is high.

## Updates and Easee's limits

The integration reads each charger every minute and the configuration (the maximum current) once an hour. Under **Options** on the integration, the time between updates can be set from 30 to 600 seconds. If Easee answers 429 (too many requests), the integration pauses for five minutes, or as long as Easee asks. After a command the integration shows the expected state right away and reads the charger again after 10 seconds.

The access token is renewed with Easee's refresh token. If that fails, the integration logs in again with username and password. If Easee rejects them, a repair issue is created, and the integration must be authorized again.

## Not tested

The integration is written from Easee's public API description and pyeasee, which Home Assistant's Easee integration uses, and tested against a fake Easee with responses shaped like the real ones. None of the following is checked against a real charger or Easee's real API:

- Login (`POST /api/accounts/login` with `userName` and `password`) and renewal (`POST /api/accounts/refresh_token` with `accessToken` and `refreshToken`), the field names `accessToken`, `refreshToken` and `expiresIn`, and that a wrong password gives HTTP 400.
- The list of chargers (`GET /api/chargers`) with the fields `id` and `name`.
- The state (`GET /api/chargers/{id}/state`) with `chargerOpMode`, `totalPower` (assumed to be in kW), `sessionEnergy`, `lifetimeEnergy`, `dynamicChargerCurrent` and `isOnline`.
- The codes for `chargerOpMode` 0 to 8 and what they mean.
- The configuration (`GET /api/chargers/{id}/config`) with `maxChargerCurrent`. If it is missing, 32 A is used as the upper limit.
- The commands `POST /api/chargers/{id}/commands/start_charging`, `stop_charging`, `pause_charging` and `resume_charging`, and that they answer 200 or 202.
- The dynamic current with `POST /api/chargers/{id}/settings` and `dynamicChargerCurrent`, and that 0 A holds charging back.
- How often Easee allows requests, and whether Easee sends `Retry-After`. One minute is a cautious guess.

The current limit for a whole circuit (`POST /api/sites/{siteId}/circuits/{circuitId}/settings` with `dynamicCircuitCurrentP1` to `P3`) is not included, and neither is Easee's SignalR stream with real-time state.

## Development

Built in Rust on the `thehome-plugin` SDK from [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: login, tokens and Easee's API
- `src/model.rs`: Easee's data and how it becomes states and commands
- `src/lib.rs`: the connection to HomewrightHQ
- `tests/easee.rs`: tests against a fake Easee with the responses in `tests/fixtures`, and a fake HomewrightHQ in `tests/host`

```
cargo test
cargo build --release
```

A new version is released with a tag, e.g. `git tag v0.1.0 && git push origin v0.1.0`. The workflow builds for Windows, Linux (x86_64 and arm64, static musl) and macOS and puts the binaries and `homewright.json` on the release.

## License

MIT
