# Testing ZClients

## Automated

```sh
scripts/check.sh          # every CI gate (fmt, clippy, tests, property tests,
                          # audit, deny, universal build, Miri)
cargo test                # 139 unit / migration / failure-injection / property tests
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
HOME=$H ZCLIENTS_SELFTEST=search "$APP"
HOME=$H ZCLIENTS_SELFTEST=print "$APP"
HOME=$H ZCLIENTS_SELFTEST=window ZCLIENTS_ALERTS=accept "$APP"
HOME=$H ZCLIENTS_SELFTEST=addressbar "$APP"       # logs the window number twice: screencapture -l <n>
HOME=$H ZCLIENTS_SELFTEST=announce "$APP"         # watch AXAnnouncementRequested with an AX observer
HOME=$H ZCLIENTS_SELFTEST=parity "$APP"          # add ZCLIENTS_REAL_CLICK=1 + a posted mouse click, see below
HOME=$H ZCLIENTS_SELFTEST=stability ZCLIENTS_ALERTS=decline ZCLIENTS_MOCK_MEDIA=1 "$APP"
HOME=$H ZCLIENTS_SELFTEST=stability ZCLIENTS_ALERTS=accept ZCLIENTS_MOCK_MEDIA=1 "$APP"   # then: the logged partial file must not exist
```

Since 2.0.0, logs also go to the unified log:
`/usr/bin/log show --last 10m --predicate 'subsystem == "com.zport.swift"'` (redacted; in zsh plain `log` is a shell builtin),
and the last 300 lines are in Help › Export Diagnostics.

`ZCLIENTS_ALERTS=accept|decline` (test builds only) answers every app
question with its first / last button and logs it.

| Mode | Proves |
|---|---|
| `security` | Inspector off; WebKit UA + installed Safari version; sign-in helper absent off accounts.zoho.*; popup / download / mailto without a click all ask (declined → nothing happens); camera on www.zoho.com is *asked*, on meet.google.com *allowed* with two live tracks; Stop ends capture and the keep-awake assertion; an HTML file named .pdf is not shown; a real PDF opens from a 0700/0600 private folder |
| `a11y` | Every toolbar/tab/sidebar control has a VoiceOver name; tabs are AXRadioButton/AXTabButton in an AXTabGroup with one selected; ←/→, Return, ⌥⇧→ work on the tab strip and focus stays there |
| `address2` | Typed address text is dropped on tab switch and never navigates on page click; Esc restores; a Return for a tab no longer in front is dropped; Return on the same tab navigates |
| `editor` | ⌘A, ⌘Z, ⇧⌘Z and Control-A reach a code editor's own key handler (CodeMirror/Ace/Monaco/hidden-textarea editors such as Zoho's Deluge editor); ⌘A in a plain text box still selects all |
| `downloads` | PDF popups become tabs with viewer controls; hidden-frame and popup zips download; PDF attachments open as tabs; the PDF viewer's download button saves; every file is quarantined |
| `search` (2.1) | Google Search workspace: a Zoho page stays in the Search tab; example.com is blocked both by script and when typed (page kept, "can't be opened here" shown); a Meet link opens in a client, which comes to the front |
| `stability` (2.0) | U6 closing a tab in a call asks; U1 a newer download leaves an item's key on the same file; S1 quitting with a download and a call asks (decline: both continue; accept: real quit path, partial file deleted); S2 a page whose WebContent process is killed twice shows "This page stopped working" instead of reloading |
| `parity` (2.3) | In-page UI works like Chrome: one click on a fixture button (loaded with `loadHTMLString`, base www.zoho.com) opens a sliding panel; transform + width transitions, keyframes, Web Animations and a rAF tween all finish within 1 s, the panel ends at `transform: none`, rAF ≥ 25 fps in Low Power Mode (≥ 55 otherwise), the page keeps focus and gets no blur. Run once with ZClients active and once with Finder in front. With `ZCLIENTS_REAL_CLICK=1` the second click must come from outside: the log says `SELFTEST PARITY real click at X Y` (global points) and a real mouse click has to be posted there within 4 s (a CGEvent from a process with Accessibility access); without it the click is sent through `NSApp.sendEvent`, which never activates the app, so only `hasFocus` can fail. Third round (B7): HTML5 drag and drop, started with Finder in front: `dragstart`, `dragover` and a `drop` carrying the dragged data. It needs a real drag (`SELFTEST PARITY real drag from X Y to X Y`: post mouse down, ~30 dragged moves, up); without `ZCLIENTS_REAL_CLICK=1` it logs "not run" |
| `print` (2.3) | `window.print()` from a page's Print button (a real click) and from an iframe each show the macOS Print panel as a sheet (title "Print"); the test cancels it |
| `window` (2.3.1) | The red close button hides the window and the app keeps running (a page timer still ticks, tabs stay awake); a page `alert()` brings the window back; so does the Dock-icon reopen message; Window › ZClients exists |
| `announce` (2.3.1) | Save on the New Client sheet with no name keeps it open, shows "Enter a client name." and posts it as a VoiceOver announcement (priority high); an AXObserver on the app's pid for `AXAnnouncementRequested` receives it. AppKit does not quit while a sheet is open, so the test cancels the sheet first |
| `addressbar` (2.3.1) | Logs the window number with an https address shown and again while typing; `screencapture -x -o -l <n>` at each shows the lock, then a magnifying glass, *inside* the field with the text starting after it |
| `meet`, `input`, `find`, `memory`, `perf`, `address`, `prep` | Unchanged from 1.4.0 (see README); `address`, `input`, `find` need System Events keystrokes |

Recovery can be exercised by writing a truncated
`$H/Library/Application Support/ZPortSwift Test/clients.json` (and a valid
`clients.json.bak`) before launching with `ZCLIENTS_ALERTS=decline` (Quit:
file untouched) or `accept` (Restore: damaged file kept as
`clients.json.damaged-<time>`).

## 2.3 finding: Zoho side panels "don't open" (A0)

Symptom: in the Zoho Analytics report editor the gear next to View Mode
slides in a Settings panel in Chrome, nothing in ZClients.

- Plain WebKit animates normally (bare WKWebView: CSS transition, keyframes,
  Web Animations, rAF all finish in ~430 ms; Low Power Mode holds rAF at
  30 fps, Chrome stays at 60, but every animation still finishes).
- User check, installed 2.2.0: click once on the page first (ZClients
  active), then the gear → **the panel opens**. Safari could not be checked
  (no signed-in Safari); the Web Inspector capture in ZClients Test was not
  needed once the next point reproduced it.
- `ZCLIENTS_SELFTEST=parity` on 2.2.0 code, another app (Finder) in front, a
  real mouse click posted at the button: the app comes forward but the page
  gets **0 mousedown, 0 click**; panel stays at `translateX(300px)`. Same run
  with ZClients active: 1 click, panel `none`, everything done in ~320 ms.
- Root cause: WKWebView's `acceptsFirstMouse:` answers NO for an ordinary
  click, so AppKit spends the first click on an inactive window on bringing
  it forward. Chrome passes that click to the page. Fixed in 2.3.0 (A1):
  `ZWebView` accepts the first mouse unless Settings › Keyboard & mouse ›
  "Clicks in an inactive window go to the page" is off. With the fix the
  same real click gives 1 click, panel open in ~305 ms, `document.hasFocus()`
  true, no blur.
- Not the cause: missing `requestIdleCallback` / `scheduler.postTask` (no
  page error involved; Zoho's panel opens once the click arrives), the user
  agent, the client-world scripts, KVO.

## Manual verification checklist (real app, real accounts)

Run on the test app first; on the real app only after installing 2.0.0.

**New in 2.3.0** (the gap that let the Analytics panel bug ship: no manual pass ever clicked through real page UI)
- [ ] Zoho Analytics report editor → gear next to View Mode → the Settings panel slides in and the chart narrows, (a) with ZClients already active, (b) clicking the gear straight from another app (Cliq, Chrome) with ZClients behind it. The slide looks like Chrome's (30 fps in Low Power Mode is a macOS limit, see BACKLOG A3).
- [ ] Settings › Keyboard & mouse › "Clicks in an inactive window go to the page" off → (b) only brings ZClients forward; on again → (b) opens the panel.
- [ ] A Print button in Zoho Analytics, CRM, Books or Invoice opens the macOS Print panel as a sheet; Cancel closes it; printing/PDF shows the report, not a blank page.
- [ ] Drag a column in Zoho Analytics' column panel onto the report.
- [ ] Hover an asleep tab (moon) → "Asleep to save memory — click to wake"; Help › Export Diagnostics lists each awake tab with its memory.

**New in 2.0.0**
- [ ] ⌘Q during a download → "Quit and cancel 1 download?"; Cancel keeps the window; Quit leaves no partial file in ~/Downloads.
- [ ] (2.3.1) Red close button during a download or a call → the window hides, nothing is asked, the download and the call continue; the Dock icon or Window › ZClients brings the window back.
- [ ] ⌘W on a Meet tab during a call → "Close … and end the call?".
- [ ] A page `alert()` appears as a sheet titled "<client> — <host>"; a second one within 10 s offers "Don't allow more dialogs from this page".
- [ ] Customise: email without password → red message under Password, sheet stays open; empty name → "Enter a client name."
- [ ] ⇧⌘O switcher (type, ↑/↓, ↩, Esc); ⌃⌘↑/↓; ⌥⌘1…9.
- [ ] (2.2) The sidebar button at the top left, before the client name, hides the client list and shows it again (same as ⌃S); with the list hidden it sits just right of the window buttons.
- [ ] EU/IN/UK/AU account: after one page loads, the launcher opens `https://<app>.zoho.<dc>`; a CRM widget on zappsusercontent.<dc> can show `confirm()`.
- [ ] Microsoft/Okta SSO sign-in completes in a tab.
- [ ] Turn Wi-Fi off, reload (offline screen), turn it on → the page reloads by itself.
- [ ] Light ↔ Dark and a new accent colour in System Settings with the app open: tab strip wash, separators, progress line, picker border and sidebar bar all update.
- [ ] Accessibility › Display › Text size larger: chrome text follows.

**Login / logout / sessions**
- [ ] Sign in to Zoho in profile A; quit (⌘Q); relaunch → still signed in.
- [ ] Sign out of Zoho in A (Zoho's own Sign Out); wait 5 s; quit; relaunch → signed **out** (no old session comes back).
- [ ] Settings › Profile: turn off "Keep me signed in" for A; quit/relaunch → Zoho's own session rules apply.
- [ ] Saved login: sign in once, accept "Save"; force a sign-in page (Zoho › Sign Out, then open crm.zoho.com) → email filled and advanced, password filled, **you press Sign In**.
- [ ] Customise sheet: shows "Saved for <email>", never the password; Change… replaces, Remove removes, Cancel keeps.

**Multiple profiles**
- [ ] Two profiles signed in to different Zoho accounts; each tab shows its own account; switching profiles never mixes them.
- [ ] Google Search workspace (2.1): click a crm.zoho.com result → it opens **in the Search tab** (its own sign-in, separate from every client). Click any other site's result, or type example.com → "This page can't be opened here" over the page, nothing opens in another browser; Back to Search returns to the results.
- [ ] Search from client B (⌘K), click a Google Meet link → Meet opens in **B**, which comes to the front. Delete B and try again → asked which client.
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
- [ ] Downloads menu: size and % while running, size and "N min ago" when done, Cancel, Retry, Show in Finder; Dock badge while downloading; a notification when one finishes in the background.
- [ ] Settings › Clear PDF previews: open PDF tabs stay, others removed.

**Deletion**
- [ ] Delete a test profile → its WebKit folder under `~/Library/WebKit/com.zport.swift/WebsiteDataStore/` disappears; no `cleanup.json` remains. If anything is left, the alert names it and Retry works.

**Keyboard / VoiceOver** (see script below)
- [ ] Full Keyboard Access on: Tab reaches tab chips, profile tile, swatches; focus ring visible.
- [ ] ⌃⌘T focuses the tab strip; ←/→ moves; Return selects; ⌥⇧←/→ reorders; Delete closes; ⇧⌘A lists all tabs.
- [ ] ⌘L selects the address; type, then click a tab / the page → nothing navigates; Esc restores and returns to the page.
- [ ] In Zoho Writer ⌘S saves the document (not the sidebar); ⌃S toggles the sidebar. In Sheet ⌘D fills down. In Meet ⌘D mutes.
- [ ] ⌘C/⌘X/⌘V/⇧⌘V/⌘Z/⇧⌘Z/⌘A in Mail compose, CRM notes, Desk reply, Writer, Sheet cells, Cliq.
- [ ] Context menu on a link: Open Link in New Tab / Background Tab; Copy Link; Download Linked File.

**Restart recovery**
- [ ] Make `clients.json` unreadable (`chmod 000`, on a copy-backed test install) → recovery alert; Quit leaves it untouched.

**Signing / notarization** (release machine)
- [ ] `scripts/release.sh` completes; `spctl -a -vvv -t exec dist/…/ZClients.app` says "Notarized Developer ID"; `stapler validate` OK; checksum matches.

## VoiceOver script

1. ⌘F5 to start VoiceOver; ⌘Tab to ZClients.
2. VO-U → Window rotor: one window "ZClients".
3. VO-Right through the toolbar: "Hide Clients, button" (or "Show Clients" when the sidebar is hidden), "Back, button", "Forward", "Reload", "App launcher", "Address and search, text field", "Google Search workspace", "Google Meet for this client", "Downloads", "New tab".
4. Into "Tabs, tab group": each tab is read as "<title>, tab, selected/…"; a PDF tab adds "PDF", a call adds "camera and microphone on", a sleeping tab "asleep". VO-Space selects. VO-Shift-M opens the tab menu (Move Left/Right, Duplicate, Close).
5. Sidebar: "Search clients, search text field"; the table reads "<name>, <n> tabs". VO-Shift-M → Customise, Move Up/Down, Delete Client.
5a. ⇧⌘O: "Switch to client, search text field" and a "Clients" table read the same way.
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
| Generic pages: inputs, contenteditable, frames, shadow DOM, window.open, file input, clipboard, drag/drop, history, print, find | Manual | Partly (window.open, downloads; 2.3: panels/animations, first click, drag/drop, print) | Manual | Manual | — | Manual | **Not run** |
| Light / dark / high contrast / larger text | Manual | | | | | | **Not run** |
