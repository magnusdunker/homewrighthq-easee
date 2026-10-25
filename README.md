# Easee til HomewrightHQ

En ekstern integration til [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Easee-ladebokse (Easee Home og Easee Charge) gennem Easees cloud.

## Installation

I HomewrightHQ: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/homewrighthq-easee
```

og tryk **Installér**. Tilføj derefter **Easee** under **Tilføj integration**, og log ind med det brugernavn (e-mail eller telefonnummer) og den adgangskode, du bruger i Easee-appen.

## Hvad du får

Hver ladeboks bliver en enhed med:

- status: `offline`, `disconnected`, `awaiting_start`, `charging`, `completed`, `error`, `ready_to_charge`, `awaiting_authorization` eller `de_authorizing`
- effekt i W
- energi i den aktuelle ladning og energi i alt, i kWh
- om ladeboksen er forbundet til Easees cloud
- en kontakt **Opladning**: tænd starter ladningen eller fortsætter en ladning på pause, sluk sætter den på pause
- en indstilling **Dynamisk strømgrænse** i ampere, fra 0 til ladeboksens maksimale strøm

Der er to tjenester til automationer:

- `easee.set_charging_current` med `current` (ampere) sætter den dynamiske strømgrænse. 0 A holder ladningen tilbage uden at afslutte den.
- `easee.send_command` med `command` (`start`, `stop`, `pause` eller `resume`).

Begge tager en entity fra ladeboksen som mål, eller `charger_id` (f.eks. `EH123456`). Har kontoen kun én ladeboks, kan begge udelades.

Et eksempel, der lader i de billige timer: en automation sætter `current` til 16 A, når elprisen er lav, og til 0 A, når den er høj.

## Opdatering og Easees grænser

Integrationen læser hver ladeboks hvert minut og konfigurationen (den maksimale strøm) en gang i timen. Under **Indstillinger** på integrationen kan tiden mellem opdateringer sættes fra 30 til 600 sekunder. Svarer Easee 429 (for mange forespørgsler), holder integrationen pause i fem minutter, eller så længe Easee beder om. Efter en kommando viser integrationen den forventede tilstand med det samme og læser ladeboksen igen efter 10 sekunder.

Adgangsnøglen fornyes med Easees refresh-token. Lykkes det ikke, logger integrationen ind igen med brugernavn og adgangskode. Afviser Easee dem, oprettes et problem, og integrationen skal godkendes igen.

## Ikke afprøvet

Integrationen er skrevet ud fra Easees offentlige API-beskrivelse og pyeasee, som Home Assistants Easee-integration bruger, og afprøvet mod en falsk Easee med svar formet som de rigtige. Intet af det følgende er kontrolleret mod en rigtig ladeboks eller Easees rigtige API:

- Login (`POST /api/accounts/login` med `userName` og `password`) og fornyelse (`POST /api/accounts/refresh_token` med `accessToken` og `refreshToken`), feltnavnene `accessToken`, `refreshToken` og `expiresIn`, og at forkert adgangskode giver HTTP 400.
- Listen over ladebokse (`GET /api/chargers`) med felterne `id` og `name`.
- Tilstanden (`GET /api/chargers/{id}/state`) med `chargerOpMode`, `totalPower` (antaget i kW), `sessionEnergy`, `lifetimeEnergy`, `dynamicChargerCurrent` og `isOnline`.
- Koderne for `chargerOpMode` 0 til 8 og deres betydning.
- Konfigurationen (`GET /api/chargers/{id}/config`) med `maxChargerCurrent`. Mangler den, bruges 32 A som øvre grænse.
- Kommandoerne `POST /api/chargers/{id}/commands/start_charging`, `stop_charging`, `pause_charging` og `resume_charging`, og at de svarer 200 eller 202.
- Den dynamiske strøm med `POST /api/chargers/{id}/settings` og `dynamicChargerCurrent`, og at 0 A holder ladningen tilbage.
- Hvor ofte Easee tillader forespørgsler, og om Easee sender `Retry-After`. Et minut er et forsigtigt gæt.

Strømgrænsen for et helt kredsløb (`POST /api/sites/{siteId}/circuits/{circuitId}/settings` med `dynamicCircuitCurrentP1` til `P3`) er ikke med, og heller ikke Easees SignalR-strøm med tilstand i realtid.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: login, tokens og Easees API
- `src/model.rs`: Easees data og hvordan de bliver til tilstande og kommandoer
- `src/lib.rs`: forbindelsen til HomewrightHQ
- `tests/easee.rs`: tests mod en falsk Easee med svarene i `tests/fixtures`, og en falsk HomewrightHQ i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.1.0 && git push origin v0.1.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne og `homewright.json` på releasen.

## Licens

MIT
