# ZClients 1.5 hardening — plan and threat model

Baseline: commit `8457d84` (installed 1.4.0, build 14). The uncommitted
address-bar work in `src/ui.rs` / `src/selftest.rs` predates this plan and is
kept as is; this work builds on it (see "P1.1" below).

## What ZClients is (unchanged)

A native macOS (AppKit + WebKit, Rust/objc2) browser that keeps many Zoho
profiles isolated (one `WKWebsiteDataStore` per profile, keyed by the profile
UUID), with tabs, an app launcher, a separate Google Search workspace, Google
Meet, remembered sessions, downloads and PDF tabs.

There is **no profile or application lock** in this code base, in the
installed 1.4.0 binary, or in the Swift/Electron predecessors (searched for
lock/unlock/LAContext/Touch ID; the only hits are `security
unlock-keychain` in the build scripts). Nothing was removed or added there.

## Threat model

| Adversary | Can do | Must not be able to |
|---|---|---|
| Hostile web content inside WebKit (any non-Zoho page reached through a link, an ad frame inside a widget, an OAuth provider page in a popup) | Run JS in its own origin, open frames, call `window.open`, start downloads, request camera/mic/screen, post to `window.webkit.messageHandlers` from its own world | Get camera/mic/screen without a prompt, receive or trigger autofill, reach the app bridge, spawn popups or downloads without a user gesture, open external apps silently, inherit the opener's privileges |
| Customer-authored content on trusted SaaS domains (Zoho Sites, Creator portals, `*.zappsusercontent.*` widgets, `zohopublic`, user subdomains of `zoho.com`) | Everything above, but on a host that matches `*.zoho.*` | Be treated like Zoho's own meeting/sign-in origin. Privileges are **exact origins**, never domain families |
| A compromised trusted origin (e.g. XSS on `crm.zoho.com`) | Act as that origin | Escalate beyond that origin's capability: no media on CRM without a prompt, no credentials outside `accounts.zoho.*`, downloads still gesture/consent-gated, executables still confirmed |
| A malicious same-user local process | Read/write files in `~/Library/Application Support/ZPortSwift`, swap files, plant symlinks, race temp files | Read secrets (AES-GCM, key in Keychain), move a sealed record to another profile or kind (AAD binding), make the app follow a symlink when writing, make a PDF tab read outside its own directory. (It *can* delete data and can read the Keychain key if the user grants it access — out of scope, as for any app.) |
| Corrupted or partially written local state | Truncated `clients.json`, duplicate IDs, half-written vault records, crash mid-capture | Cause the app to overwrite the original, merge two profiles' data stores, restore a logged-out session, or claim a deletion succeeded |
| Release / update supply chain | Tamper with the build, dependencies, or an update | Ship unsigned/mis-identified builds (release script refuses), slip a dependency with a known advisory (cargo-audit/deny gate) |

## Phases

1. **P0 policy core** (`policy.rs`): one `Capability` enum and exact-origin
   tables; every privilege check in `web.rs` goes through it.
2. **P0 vault** (`keychain.rs`): versioned envelope with AAD (bundle id,
   schema version, record kind, profile/store UUID); one-way migration
   marker so swapped legacy files are refused afterwards.
3. **P0 sessions** (`session.rs`): bounded, per-profile retention;
   exact-expiry hashing; logout/empty-jar detection after the store is known
   loaded; cookie-store observer for prompt capture; tombstoned backups.
4. **P0 navigation/downloads/PDF** (`web.rs`, `downloads.rs`, `pdf.rs`):
   native-input gesture tracking, popup depth limit and privilege drop,
   risky-type confirmation, per-origin download permission, quarantine
   verification, PDF magic/size validation, private per-preview directories,
   pruning.
5. **P0 inspector**: release builds can never be inspectable.
6. **P1 correctness**: address bar bound to the tab where editing began;
   recoverable profile store (read-only recovery, `.bak`, duplicate-ID
   repair, durable atomic writes); persisted, verified, retryable deletion;
   saved-login UX; Search keeps its originating profile.
7. **P2 UX/accessibility**: Settings & Privacy window, media indicator with
   stop, accessible tab strip/tiles, tab overflow + switcher, shortcuts that
   leave page shortcuts alone, per-origin zoom, error overlay, diagnostics
   export with redaction.
8. **P3 release**: one version source (Cargo.toml), `scripts/check.sh` CI
   gate, `deny.toml`, release script that refuses anything but Developer ID +
   notarization, local dev path without plaintext keychain passwords.

Every phase keeps the on-disk formats readable by the previous phase or
documents the one-way step (see `docs/MIGRATION.md`).
