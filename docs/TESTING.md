# Testing ZClients

## Automated

```sh
scripts/check.sh          # every CI gate (fmt, clippy, tests, property tests,
                          # audit, deny, universal build, Miri)
cargo test                # 110 unit / migration / failure-injection / property tests
./build.sh --test         # "ZClients Test": own bundle id + data, ad-hoc signed
```

Self-tests drive the real app. Always give them a scratch home so nothing real
is touched (Downloads, Application Support and the vault key all follow HOME):

```sh
H=$(mktemp -d); APP=".build/ZClients Test.app/Contents/MacOS/ZClients"
HOME=$H ZCLIENTS_SELFTEST=security ZCLIENTS_ALERTS=decline ZCLIENTS_MOCK_MEDIA=1 ZCLIENTS_MOCK_PROMPT=1 "$APP"
HOME=$H ZCLIENTS_SELFTEST=a11y ZCLIENTS_ALERTS=decline "$APP"
HOME=$H ZCLIENTS_SELFTEST=address2 "$APP"
HOME=$H ZCLIENTS_SELFTEST=downloads ZCLIENTS_ALERTS=accept "$APP"
```

`ZCLIENTS_ALERTS=accept|decline` (test builds only) answers every app
question with its first / last button and logs it.

| Mode | Proves |
|---|---|
| `security` | Inspector off; WebKit UA + installed Safari version; sign-in helper absent off accounts.zoho.*; popup / download / mailto without a click all ask (declined → nothing happens); camera on www.zoho.com is *asked*, on meet.google.com *allowed* with two live tracks; Stop ends capture and the keep-awake assertion; an HTML file named .pdf is not shown; a real PDF opens from a 0700/0600 private folder |
| `a11y` | Every toolbar/tab/sidebar control has a VoiceOver name; tabs are AXRadioButton/AXTabButton in an AXTabGroup with one selected; ←/→, Return, ⌥⇧→ work on the tab strip and focus stays there |
| `address2` | Typed address text is dropped on tab switch and never navigates on page click; Esc restores; a Return for a tab no longer in front is dropped; Return on the same tab navigates |
| `editor` | ⌘A, ⌘Z, ⇧⌘Z and Control-A reach a code editor's own key handler (CodeMirror/Ace/Monaco/hidden-textarea editors such as Zoho's Deluge editor); ⌘A in a plain text box still selects all |
| `downloads` | PDF popups become tabs with viewer controls; hidden-frame and popup zips download; PDF attachments open as tabs; the PDF viewer's download button saves; every file is quarantined |
| `meet`, `input`, `find`, `memory`, `perf`, `address`, `prep` | Unchanged from 1.4.0 (see README); `address`, `input`, `find` need System Events keystrokes |

Recovery can be exercised by writing a truncated
`$H/Library/Application Support/ZPortSwift Test/clients.json` (and a valid
`clients.json.bak`) before launching with `ZCLIENTS_ALERTS=decline` (Quit:
file untouched) or `accept` (Restore: damaged file kept as
`clients.json.damaged-<time>`).

## Manual verification checklist (real app, real accounts)

Run on the test app first; on the real app only after installing 1.5.0.

**Login / logout / sessions**
- [ ] Sign in to Zoho in profile A; quit (⌘Q); relaunch → still signed in.
- [ ] Sign out of Zoho in A (Zoho's own Sign Out); wait 5 s; quit; relaunch → signed **out** (no old session comes back).
- [ ] Settings › Profile: turn off "Keep me signed in" for A; quit/relaunch → Zoho's own session rules apply.
- [ ] Saved login: sign in once, accept "Save"; force a sign-in page (Zoho › Sign Out, then open crm.zoho.com) → email filled and advanced, password filled, **you press Sign In**.
- [ ] Customise sheet: shows "Saved for <email>", never the password; Change… replaces, Remove removes, Cancel keeps.

**Multiple profiles**
- [ ] Two profiles signed in to different Zoho accounts; each tab shows its own account; switching profiles never mixes them.
- [ ] Search from profile B (⌘K, query, click a crm.zoho.com result) → opens in **B**. Delete B, click another Zoho result in the same Search tab → asked which profile.
- [ ] Meet button while the Search workspace is in front → asked which profile.

**Media**
- [ ] Join a Meet call: camera and mic work with no ZClients prompt (macOS asks once per app); red camera in the toolbar and on the tab; display stays awake.
- [ ] Present now → macOS picker; share a window; stop from Meet.
- [ ] Toolbar camera menu → Stop Camera and Microphone → Meet shows them off.
- [ ] Switch profile during the call → call continues; come back → same tab.
- [ ] Leave the call → indicator gone; display sleep allowed again.
- [ ] Zoho CRM or a CRM widget asking for the camera → WebKit asks first.

**Downloads / PDFs**
- [ ] CRM attachment PDF → downloads and opens as a tab; a .zip → asks once ("Always Allow Archives from This Site" stops further asks).
- [ ] WorkDrive folder download (slow zip) → asks "wants to download a file" if more than ~10 s passed; "Always Allow for This Site" works.
- [ ] A .dmg/.pkg/.exe link → always asks.
- [ ] Downloads menu: progress %, Cancel, Retry, ⌥-click Show in Finder.
- [ ] Settings › Clear PDF previews: open PDF tabs stay, others removed.

**Deletion**
- [ ] Delete a test profile → its WebKit folder under `~/Library/WebKit/com.zport.swift/WebsiteDataStore/` disappears; no `cleanup.json` remains. If anything is left, the alert names it and Retry works.

**Keyboard / VoiceOver** (see script below)
- [ ] Full Keyboard Access on: Tab reaches tab chips, profile tile, swatches; focus ring visible.
- [ ] ⌃⌘T focuses the tab strip; ←/→ moves; Return selects; ⌥⇧←/→ reorders; Delete closes; ⇧⌘A lists all tabs.
- [ ] ⌘L selects the address; type, then click a tab / the page → nothing navigates; Esc restores and returns to the page.
- [ ] In Zoho Writer ⌘S saves the document (not the sidebar); ⌃⌘S toggles the sidebar. In Sheet ⌘D fills down. In Meet ⌘D mutes.
- [ ] ⌘C/⌘X/⌘V/⇧⌘V/⌘Z/⇧⌘Z/⌘A in Mail compose, CRM notes, Desk reply, Writer, Sheet cells, Cliq.
- [ ] Context menu on a link: Open Link in New Tab / Background Tab; Copy Link; Download Linked File.

**Restart recovery**
- [ ] Make `clients.json` unreadable (`chmod 000`, on a copy-backed test install) → recovery alert; Quit leaves it untouched.

**Signing / notarization** (release machine)
- [ ] `scripts/release.sh` completes; `spctl -a -vvv -t exec dist/…/ZClients.app` says "Notarized Developer ID"; `stapler validate` OK; checksum matches.

## VoiceOver script

1. ⌘F5 to start VoiceOver; ⌘Tab to ZClients.
2. VO-U → Window rotor: one window "ZClients".
3. VO-Right through the toolbar: "Back, button", "Forward", "Reload", "App launcher", "Address and search, text field", "Google Search workspace", "Google Meet for this client", "Downloads", "New tab".
4. Into "Tabs, tab group": each tab is read as "<title>, tab, selected/…"; a PDF tab adds "PDF", a call adds "camera and microphone on", a sleeping tab "asleep". VO-Space selects. VO-Shift-M opens the tab menu (Move Left/Right, Duplicate, Close).
5. Sidebar: "Search clients, search text field"; the table reads "<name>, <n> tabs". VO-Shift-M → Customise, Move Up/Down, Delete.
6. Settings (⌘,): headings are navigable with VO-Command-H.

## Browser-interaction compatibility matrix

Status after this change set. "Auto" = covered by a self-test; "Manual" =
needs a signed-in account and was **not run** here.

| Area | Keyboard only | Mouse | VoiceOver | IME / layouts | Multi-profile | Narrow window | Status |
|---|---|---|---|---|---|---|---|
| Tab strip, sidebar, toolbar | Auto | Auto | Auto (tree) | n/a | Auto | Auto (tab window logic, unit) | Done |
| Address bar | Auto | Auto | Auto (label) | Manual | Auto | — | Done |
| Downloads / PDF | — | Auto | — | — | Auto | — | Done |
| Google Meet media | — | Auto (mock devices) | Manual | — | Manual | Manual | Partly |
| Zoho Mail compose, CRM, Desk, WorkDrive, Writer, Sheet, Cliq | Manual | Manual | Manual | Manual | Manual | Manual | **Not run** |
| Generic pages: inputs, contenteditable, frames, shadow DOM, window.open, file input, clipboard, drag/drop, history, print, find | Manual | Partly (window.open, downloads) | Manual | Manual | — | Manual | **Not run** |
| Light / dark / high contrast / larger text | Manual | | | | | | **Not run** |
