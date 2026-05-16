# Changelog

All notable changes to ModelMeter will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.3.0] - 2026-05-15

### Added
- **x.ai (Grok): Team ID input field** on the Add Provider modal. Previous releases
  embedded the developer's team UUID, which silently routed every other user's
  card at the developer's account. The Team ID is now required at provider
  creation, validated as a UUID, stored alongside the provider row, and used
  to address all x.ai management-api endpoints. Existing x.ai provider rows
  from an older install must be removed and re-added through the UI.

### Changed
- Shared `build_clients()` helper in `providers/mod.rs` replaces four identical
  `reqwest::Client::builder()` blocks across the OpenAI, Anthropic, x.ai, and
  ElevenLabs providers.
- `unix_now()` and `prune_old_records` no longer panic when the system clock
  is set before the UNIX epoch; they return `0` and continue.
- `check_status` caps the error-response body at 4 KiB before formatting it
  into `ClientError.detail`, defending against a hostile or misconfigured
  endpoint streaming an unbounded body.
- ElevenLabs card on the Dashboard hoists its `Intl.NumberFormat` to module
  scope instead of constructing four formatters per render.
- `PRAGMA key` interpolation in `db::open()` now has an inline comment
  explaining why it is safe (key is 64-char hex from `getrandom`, SQLCipher
  doesn't allow parameter binding for `PRAGMA key`).

### Fixed
- Dashboard's 90-day `Promise.all` over `getUsageSummary` no longer drops the
  entire provider's chart to `[]` when a single day's fetch fails; each daily
  fetch now catches its own error and degrades to a zero bar.
- GeneralSettings now clears the "Saved" toast timer if the component
  unmounts while the toast is still showing.

### Database
- Migration 7: adds a nullable `team_id` column to the `providers` table.

## [1.0.0] - 2026-05-03

### Added

#### Providers
- OpenAI integration: usage polling, cost calculation, and balance tracking via the `/v1/organization/usage` and `/v1/organization/costs` endpoints.
- Anthropic integration: usage polling and cost calculation via the Anthropic usage API, with ISO 8601 timestamp handling and cent-string cost parsing.

#### Sync engine
- Configurable sync interval (default 15 minutes, range 1 minute – 1 day).
- Automatic retry with exponential back-off on transient HTTP errors (429, 5xx).
- Sync status indicator in the dashboard header showing last-sync time and current state (idle / syncing / error).
- Manual refresh button (also bound to Ctrl+R / Cmd+R).

#### Dashboard and widgets
- Five v1 dashboard widgets:
  - **Current balance** — live credit balance per profile.
  - **Cost this month** — total spend for the current billing period with token count.
  - **Daily spend sparkline** — bar chart of per-day spend over a configurable time range.
  - **Top models** — ranked list of models by cost with proportional inline bars.
  - **Sync status** — last-sync timestamp and per-profile sync state.
- Drag-and-drop widget layout with 12-column CSS grid.
- Layout persisted locally per user; survives restarts.
- All widgets implement four UI states: loading skeleton, empty (with contextual hint), error, and populated.

#### Micro view
- Separate slim `WebviewWindow` for at-a-glance monitoring while working in other apps.
- Independently positionable and resizable.

#### Settings
- Profile and API key management (create, rename, remove; keys stored in OS secret store).
- Sync interval selector.
- Data-retention policy (configurable rolling window; enforcement runs on each sync).
- Light / dark / system theme selector.

#### Security and storage
- OS-native secret storage: Windows DPAPI via the `keyring` crate; macOS Keychain.
- Keys decrypted in memory only for the duration of an outbound API call; never written to disk in plaintext.
- SQLite local database with schema migrations; data-retention enforcement deletes records outside the configured window.
- Structured logging with `tracing`; all API key material redacted before log output.

#### Accessibility
- Full keyboard navigation across the dashboard, settings page, and setup wizard.
- ARIA roles and labels on interactive elements.
- Focus-visible ring on all focusable controls.
- Reduced-motion support: animations disabled when the OS prefers reduced motion.

#### Setup wizard
- Four-step wizard on first run: Welcome → Provider selection → Profile and key entry → Done.
- Live key validation against each provider's API before persisting.
- "Skip — add later" bypass for deferred setup.

#### Testing
- 107 Rust unit and integration tests covering the core crate (CRUD, config, sync engine, OpenAI provider, Anthropic provider, logging/redaction).
- Integration tests use `wiremock` to exercise success, 401, 429, and malformed-response paths.
- 17 frontend smoke tests with Vitest and `@testing-library/react` covering the setup wizard, dashboard, and settings pages.
- Performance budget verified: `get_usage_summary` over 10 000 records completes in under 200 ms.

[1.3.0]: https://github.com/rupprath/modelmeter/releases/tag/v1.3
[1.0.0]: https://github.com/rupprath/modelmeter/releases/tag/v1.0.0
