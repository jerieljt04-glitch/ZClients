# ZClients (Rust)

One Mac app for many signed-in Zoho profiles. Native AppKit + WebKit, driven
from Rust through the `objc2` bindings — no web view is spent on the app's own
interface.

## Build

```sh
./build.sh              # test, build universal release, sign, install to /Applications
./build.sh --no-install # build and sign into .build/
./build.sh --test       # "ZClients Test": separate bundle id and data, safe to experiment
cargo test              # 44 unit tests (URL policy, model, sessions, vault, downloads)
```

Signing uses the "ZClients Local Signer" identity made by
`../zport-swift/setup_signing_identity.sh`, so macOS treats every build as the
same app (camera/mic grants, Keychain access).

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
