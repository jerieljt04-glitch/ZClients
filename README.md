# ZClients (Rust)

One Mac app for many signed-in Zoho profiles. Native AppKit + WebKit, driven
from Rust through the `objc2` bindings — no web view is spent on the app's own
interface.

## Build

```sh
scripts/check.sh        # every CI gate: fmt, clippy -D warnings, tests, property
                        # tests, cargo-audit, cargo-deny, universal build, Miri
./build.sh --test       # "ZClients Test": separate bundle id and data, ad-hoc signed
./build.sh              # test, build universal, sign (local identity), ask, install
./build.sh --no-install # build and sign into .build/
cargo test              # 110 unit, migration, failure-injection and property tests
```

The version lives only in `Cargo.toml` (`version` + `[package.metadata.zclients]
build`). Local builds sign with the "ZClients Local Signer" identity made by
`../zport-swift/setup_signing_identity.sh`, so macOS treats every build as the
same app (camera/mic grants, Keychain access). `build.sh` never unlocks a
keychain: unlock it yourself if codesign asks. Production releases (Developer
ID, notarized) are `scripts/release.sh` — see `docs/RELEASE.md`.

More: `docs/SECURITY-PLAN.md` (threat model), `docs/MIGRATION.md` (data
formats, rollback), `docs/TESTING.md` (self-tests, manual checklist, VoiceOver
script), `docs/PRIVATE-API.md`.

## Self-tests (test build only)

Run the test app with its own home so nothing real is touched:
`HOME=/tmp/zc ZCLIENTS_SELFTEST=<mode> ".build/ZClients Test.app/Contents/MacOS/ZClients"`

- `downloads`: PDF popups become tabs, hidden-frame and popup zips download,
  downloaded PDFs open as tabs, the PDF viewer's download button saves.
- `perf`: launch time, tab strip/toolbar cost, tab switches and saves with 12
  tabs open (main-thread milliseconds).
- `input`: ⌘A / Control+A in a text box and on the page, ⌘-click on a plain
  link and on a link a page script opens (background tabs). Add
  `ZCLIENTS_REAL_KEYS=1` and send the keys with System Events, because
  synthesized key events do not behave like a real keyboard here.
- `find`: logs the find bar and window (full screen, minimized) state twice a
  second while ⌘F / typing / Esc are sent with System Events
  (`ZCLIENTS_FULLSCREEN=1` starts in full screen).
- `meet`: with `ZCLIENTS_MOCK_MEDIA=1` (WebKit's fake camera, microphone and
  screens; add `ZCLIENTS_MOCK_PROMPT=1` to keep the app's permission code in
  the loop) a WebRTC call is measured in front, behind another tab and
  minimized (video frames / audio packets per second), then screen sharing;
  ⌘D is sent with System Events on a Zoho page and on a Meet page.
- `security`, `a11y`, `address2`: the 1.5 hardening, accessibility tree and
  address-bar rules (see `docs/TESTING.md`). `ZCLIENTS_ALERTS=accept|decline`
  answers the app's questions unattended.
- `memory`: with `ZCLIENTS_IDLE_SECS=6`, four tabs are shown once and Memory
  Saver should leave only the front tab awake (measure the WebContent
  processes with `footprint`).

## Layout

| File | What it does |
|---|---|
| `main.rs` | App delegate: launch, background, quit |
| `app.rs` | The hub: profile store, saving, every user command |
| `model.rs` | Profiles, tabs, sorting; reads/writes the Swift-compatible `clients.json` |
| `policy.rs` | Which URLs load; one `decide(Capability, Request)` for camera, mic, screen, autofill, bridge, downloads, file chooser, deep links, popups — by exact origin |
| `settings.rs` | settings.json: per-site download permission, zoom, keyboard choices, developer mode |
| `pdf.rs` | PDF validation (magic, size, no links) and private per-preview folders |
| `cleanup.rs` | Profile deletion as a persisted, verified, retryable job |
| `privacy.rs` | Settings & Privacy window (⌘,) and redacted diagnostics export |
| `web.rs` | Web views, data stores, sleeping tabs, navigation/UI delegates, popups |
| `session.rs` | "Keep me signed in": bounded per-profile retention, logout-aware backups |
| `keychain.rs` | AES-GCM vault (format 2, bound to app/kind/profile), one Keychain key |
| `tabstate.rs` | Each tab's exact place (history + scroll) across quits |
| `scripts.rs` | Isolated-world scripts: right-click downloads, sign-in helper |
| `downloads.rs` | Downloads to ~/Downloads; PDFs a page opens in a window become tabs |
| `ui.rs` | Window, sidebar, toolbar, tab strip, launcher, sheets, menus |
| `suggest.rs` | Address bar: Zoho app names and Google search |

## Data

`~/Library/Application Support/ZPortSwift/` — `clients.json` (+ `.bak`),
`settings.json`, `cleanup.json`, `TabState/`, `Vault/` (sealed),
`PDF Previews/` (one private folder per preview). Per-profile WebKit stores live in
`~/Library/WebKit/com.zport.swift/WebsiteDataStore/<client UUID>`.

## Going back to the Swift app

Quit ZClients, then `rm -rf /Applications/ZClients.app && ditto backup/ZClients-Swift.app /Applications/ZClients.app`.
Profiles and tabs carry over; the Swift app reads `clients.json` unchanged.

To go back to 1.0.0 (before folder downloads and PDF tabs):
`rm -rf /Applications/ZClients.app && ditto backup/ZClients-1.0.0.app /Applications/ZClients.app`.

To go back to 1.4.0 after installing 1.5.0:
`rm -rf /Applications/ZClients.app && ditto backup/ZClients-1.4.0.app /Applications/ZClients.app`.
Saved logins must then be re-entered (1.4.0 cannot read the 1.5 vault format);
see `docs/MIGRATION.md`.
