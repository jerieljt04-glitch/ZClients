# ZClients (Rust)

One Mac app for many signed-in Zoho profiles. Native AppKit + WebKit, driven
from Rust through the `objc2` bindings — no web view is spent on the app's own
interface.

## Build

```sh
./build.sh              # test, build universal release, sign, install to /Applications
./build.sh --no-install # build and sign into .build/
./build.sh --test       # "ZClients Test": separate bundle id and data, safe to experiment
cargo test              # 47 unit tests (URL policy, model, sessions, vault, downloads)
```

Signing uses the "ZClients Local Signer" identity made by
`../zport-swift/setup_signing_identity.sh`, so macOS treats every build as the
same app (camera/mic grants, Keychain access).

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

## Layout

| File | What it does |
|---|---|
| `main.rs` | App delegate: launch, background, quit |
| `app.rs` | The hub: profile store, saving, every user command |
| `model.rs` | Profiles, tabs, sorting; reads/writes the Swift-compatible `clients.json` |
| `policy.rs` | Which URLs load, which frames are trusted, which cookies are kept |
| `web.rs` | Web views, data stores, sleeping tabs, navigation/UI delegates, popups |
| `session.rs` | Keeps profiles signed in (promote, back up, restore cookies) |
| `keychain.rs` | AES-GCM vault for cookie backups and saved logins, one Keychain key |
| `tabstate.rs` | Each tab's exact place (history + scroll) across quits |
| `scripts.rs` | Isolated-world scripts: right-click downloads, sign-in helper |
| `downloads.rs` | Downloads to ~/Downloads; PDFs a page opens in a window become tabs |
| `ui.rs` | Window, sidebar, toolbar, tab strip, launcher, sheets, menus |
| `suggest.rs` | Address bar: Zoho app names and Google search |

## Data

`~/Library/Application Support/ZPortSwift/` — `clients.json`, `TabState/`,
`Vault/` (sealed). Per-profile WebKit stores live in
`~/Library/WebKit/com.zport.swift/WebsiteDataStore/<client UUID>`.

## Going back to the Swift app

Quit ZClients, then `rm -rf /Applications/ZClients.app && ditto backup/ZClients-Swift.app /Applications/ZClients.app`.
Profiles and tabs carry over; the Swift app reads `clients.json` unchanged.

To go back to 1.0.0 (before folder downloads and PDF tabs):
`rm -rf /Applications/ZClients.app && ditto backup/ZClients-1.0.0.app /Applications/ZClients.app`.
