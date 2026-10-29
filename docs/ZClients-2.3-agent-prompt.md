# ZClients 2.3.0 work order: pages behave like Chrome (panels, animations, clicks) + audit fixes

You are working on **ZClients**, a native macOS multi-profile Zoho browser written
in Rust (objc2 + AppKit + WebKit). Repo: `~/Desktop/Combined Whatsapp/zclients-rs`
(branch `master`, tag `v2.2.0`, build 21, installed in `/Applications/ZClients.app`).
Read `README.md`, `docs/TESTING.md`, `docs/PRIVATE-API.md` and `docs/BACKLOG.md` first.

## Ground rules (do not break these)

- Branch `release-2.3.0` from `master`. **One commit per finding ID below**
  (e.g. `P1: …`, `A3: …`). Version 2.3.0, build 22.
- Never touch the user's real profiles. Every experiment uses the test app:
  `./build.sh --test` → `.build/ZClients Test.app` (bundle id
  `com.zport.swift.test`, own data). Self-tests always run with `HOME=$(mktemp -d)`.
- Bundle id `com.zport.swift` and client UUIDs are WebKit data-store ids;
  changing either signs every profile out. Don't.
- Every private WebKit selector must be guarded (`respondsToSelector:` or
  equivalent), fail closed / no-op, and be listed in `docs/PRIVATE-API.md`.
- Injected JavaScript runs in the isolated client world (`scripts.rs`). The one
  exception allowed by this work order is a **page-world compatibility shim**
  (A2) that only defines missing standard APIs and never messages the app.
- Gates before every commit: `cargo test`, `cargo clippy -- -D warnings`, and
  `scripts/check.sh` before the release commit.
- Install only after the user has quit ZClients: copy the current app to
  `backup/ZClients-2.2.0.app` first, then `./build.sh` (signs with the local
  identity; if signing fails with errSecInternalComponent unlock the keychain
  with the password file in `~/Library/Application Support/ZClients Signing/`).
- Report faithfully: if something could not be reproduced or verified, say so.
  Do not mark a finding fixed without the evidence asked for.

---

## Part A (P0): Zoho side panels don't open in ZClients but do in Chrome

### Symptom (from the user, with screenshots)

Zoho Analytics report editor:
`https://analytics.zoho.com/workspace/3179446000000033003/edit/3179446000001615004`
(Palma client). In Chrome, clicking the gear next to **View Mode** (top right)
slides in a **Settings** panel on the right (tabs General / Analysis / Zia
Insights; sections Basic, Overview, Axis, Borders & Colors, Format, Threshold,
Drill, Drill Actions) and the chart narrows. In ZClients the same click shows
nothing: no panel, chart keeps full width. The user believes in-page UI
animations in general don't run "like a normal browser", and wants every page's
panels, drawers and animations to behave as in Chrome.

### What is already established (measured 2026-10-29 on the user's Mac, macOS 27.0.1)

1. **Plain WebKit animates correctly.** A bare `WKWebView` (same UA suffix as
   ZClients) ran a CSS transition + `transitionend`, a keyframe animation +
   `animationend`, a Web Animations `finished` promise, and a
   `requestAnimationFrame` tween of `right:` — all completed in ~430 ms; a
   `transform: translateX(100%) → none` panel ended at `none`. So this is
   **not** "WebKit can't animate". It is either something ZClients does, or
   Zoho code that depends on a Chrome-only API / behaviour.
2. **Low Power Mode is on** (`pmset -g` → `lowpowermode 1`). Under it WebKit
   runs `requestAnimationFrame` at **30 fps** (measured); Chrome stays at 60.
   Animations look choppier than Chrome but still finish.
3. **APIs Chrome has and this WebKit does not** (measured in a `WKWebView`):
   `requestIdleCallback` (WebKit feature `RequestIdleCallbackEnabled`,
   default **off**), `scheduler.postTask`, `navigator.userAgentData`,
   `showOpenFilePicker`. `Notification` exists but permission is `default`
   with no app support behind it (see B2). Present: ResizeObserver,
   IntersectionObserver, PointerEvent, popover, `:has()`, container queries,
   View Transitions.
4. ZClients' UA is WebKit's + `Version/<installed Safari> Safari/605.1.15`, so
   Zoho serves its **Safari** code path.
5. ZClients-specific things in the click path: `ZWebView` overrides
   `mouseDown:`/`rightMouseDown:`/`keyDown:`/`performKeyEquivalent:`
   (`web.rs`); client-world scripts `CONTEXT_HOOK` (capture-phase
   `contextmenu`/`mousedown`) and `EDITOR_HOOK` (`focusin`/`focusout`);
   `ui::refresh_content` calls `makeFirstResponder(view)`; KVO on title/URL/
   loading/progress feeds `app::web_state_changed`.
6. **First click on an inactive window**: WKWebView's `acceptsFirstMouse:`
   usually says NO, so when ZClients is not the active app the first click
   only activates the window and the page never sees it. Chrome delivers that
   click to the page. If the user clicked the gear while another app (Cliq,
   Chrome) was active, this alone explains "nothing happened".

### A0. Reproduce and bisect first (mandatory, no fix before this)

1. Ask the user to do two 30-second checks and report back:
   a. Open the same report in **Safari** (same account), click the gear.
      Does the Settings panel open?
   b. In ZClients, click once anywhere in the page first (so the window is
      active), then click the gear. Does it open now?
2. Ask the user to sign in to Palma in **ZClients Test** (Developer Mode on so
   Web Inspector can attach; see `settings::web_inspector_allowed`). Never use
   the real app for this. In the Inspector console, before clicking the gear:
   ```js
   window.__errs = [];
   addEventListener('error', e => __errs.push(String(e.message)), true);
   addEventListener('unhandledrejection', e => __errs.push('rej: ' + e.reason));
   new MutationObserver(m => console.log('mut', m.length)).observe(document.body, {subtree: true, childList: true, attributes: true});
   [typeof requestIdleCallback, document.visibilityState, document.hasFocus(), matchMedia('(prefers-reduced-motion: reduce)').matches]
   ```
   Click the gear, then record: `__errs`, whether a Settings panel element
   was added (find it via Elements), its computed `display / visibility /
   opacity / transform / width / right / z-index`, `el.getAnimations()`
   (stuck at `currentTime` 0?), and whether `document.timeline.currentTime`
   advances.
3. Decision tree:
   - **ReferenceError / TypeError for a missing API** (e.g.
     `requestIdleCallback is not defined`) → A2 is the fix.
   - **Fails in Safari too, no error** → Zoho's Safari path is broken; find
     the difference with Chrome (feature detection, UA sniffing) and decide
     between A2 (shim) and UA handling (A5). Report to the user before
     changing the UA.
   - **Works in Safari, fails in ZClients Test** → bisect ZClients: build the
     test app with each of these turned off in turn (behind a test-only env
     var, never in the shipped app): client-world user scripts; the
     `mouseDown:` override; `makeFirstResponder` in `refresh_content`; KVO
     observers; UA suffix. Keep the one that flips the result as the root
     cause and fix it properly.
   - **Panel is in the DOM but invisible/zero-width** → read why (stuck
     animation, `transitionend` never fired, wrong size because of the
     viewport) and fix that cause.
   - **Click never reaches the page** (no mousedown in a capture listener) →
     hit-testing / overlay view / first-mouse problem → A1.
4. Write the result (what failed, the evidence, the root cause) into the
   commit message of the fix and into `docs/TESTING.md`.

### A1. Clicks reach the page like Chrome (first mouse)

Override `acceptsFirstMouse:` in `ZWebView` to return YES so a click on an
inactive ZClients window both activates it and reaches the page (Chrome
behaviour). Keep drag-and-drop and text selection working. Add a setting
"Clicks in an inactive window go to the page" (default on) in Settings ›
Keyboard & mouse. Self-test: with another app frontmost, a synthetic click on a
button in the page fires its click handler exactly once.

### A2. Chrome-parity compatibility layer (only for APIs proven needed + cheap safe ones)

- Prefer WebKit's own implementation: enable `RequestIdleCallbackEnabled`
  through `WKPreferences` private `_features` / `_setEnabled:forFeature:`
  (guarded; listed in `PRIVATE-API.md`). If the selector or feature is
  missing, fall back to the shim.
- Shim (`scripts::PARITY_SHIM`, injected **at document start, all frames,
  page world**): defines `requestIdleCallback` / `cancelIdleCallback`
  (setTimeout-based, `IdleDeadline` with `didTimeout` and `timeRemaining()`,
  honours `options.timeout`) **only if missing**. Add any other API that A0
  proves Zoho calls unguarded (e.g. a minimal `scheduler.postTask`). Never
  overwrite an existing property; never post messages; no network.
- Unit test the shim text (only-if-missing guards present, no
  `webkit.messageHandlers`), and a self-test that `typeof requestIdleCallback
  === 'function'` in main frame and an iframe.

### A3. Animation smoothness under Low Power Mode

WebKit halves `requestAnimationFrame` to 30 fps in Low Power Mode. List
`WKPreferences._features` on this macOS and check whether any feature controls
that throttling (search keys for `Throttl`, `LowPower`, `60FPS`,
`RenderingUpdate`). If one exists and is safe, enable it (guarded, documented,
with a Settings toggle "Smooth animations in Low Power Mode", default off
because it costs battery). If none exists, document it in `docs/BACKLOG.md` as
a macOS limit and stop; do not invent a workaround.

### A4. Self-test `parity` (regression guard for all of Part A)

New `ZCLIENTS_SELFTEST=parity` mode. Load a fixture page with
`loadHTMLString:baseURL:` (base `https://www.zoho.com/`, test builds only) that
contains: a right-side panel opened by a button using a CSS transform
transition; a width transition; a keyframe animation; a Web Animations call; a
rAF tween; and a panel opened from inside `requestIdleCallback`. Drive the
button with a **real** click (System Events or a posted NSEvent into the web
view, see `docs/TESTING.md` on real input), including once with another app
frontmost. Assert within 1 s: `transitionend`, `animationend`, WAAPI
`finished`, rAF tween done, panel computed transform `none`, rAF ≥ 25 fps
(≥ 55 when not in Low Power Mode), `document.hasFocus()` still true after the
click, no `blur` fired on the page. Starting point (proved to pass in a bare
WKWebView):

```html
<style>
#p{position:fixed;top:0;right:0;width:300px;height:100%;background:#ccc;transform:translateX(100%);transition:transform .3s ease}
#p.open{transform:none}
#w{position:fixed;top:0;left:0;width:0;height:50px;background:red;transition:width .3s}
#w.open{width:300px}
@keyframes k{from{opacity:0}to{opacity:1}}
#k{animation:k .3s 1 forwards;opacity:0}
</style>
<button id=b>Settings</button><div id=p></div><div id=w></div><div id=k>k</div>
<script>
var R={};var frames=0,t0=performance.now();
(function loop(){frames++;if(performance.now()-t0<1000)requestAnimationFrame(loop);else R.rafPerSec=frames;})();
k.addEventListener('animationend',()=>R.animationend=performance.now()-t0);
p.addEventListener('transitionend',()=>R.transitionend=performance.now()-t0);
w.addEventListener('transitionend',()=>R.widthTransitionend=performance.now()-t0);
addEventListener('blur',()=>R.blurred=true);
b.addEventListener('click',()=>{p.classList.add('open');w.classList.add('open');
  document.body.animate([{opacity:1},{opacity:.99}],{duration:300}).finished.then(()=>R.waapi=performance.now()-t0);
  if(window.requestIdleCallback)requestIdleCallback(()=>R.idle=true);});
</script>
```

### A5. (Only if A0 shows Zoho's Safari path is the problem) UA handling

Do not switch to a Chrome UA silently: Zoho would then send Chrome-only code
to WebKit and break other things. If A0 proves the Safari path is broken and
the Chrome path works in WebKit, add a per-host UA override list (start with
`analytics.zoho.*` only), covered by policy tests, and verify sign-in, CRM,
Books, WorkDrive, Cliq and Meet still work. Ask the user before shipping it.

---

## Part B: other problems found in the 2.2.0 audit

Fix in this order. Each item says how it was found; items marked **verify**
must be reproduced in the test app first and dropped (with a note in
`docs/BACKLOG.md`) if they don't reproduce.

### B1 (P1, verify). `window.print()` from pages does nothing
The UI delegate has no `_webView:printFrame:` /
`_webView:printFrame:pdfFirstPageSize:completionHandler:` (only the menu
command `web::print`). WKWebView on macOS ignores `window.print()` without it,
so Print buttons in Zoho Analytics, CRM, Books, Invoice etc. are dead. Fix:
implement the delegate (guarded, `PRIVATE-API.md`), print **that frame** with
the standard panel as a sheet, same blank-page guard as `web::print`. Self-test
with a fixture calling `window.print()` (panel appears; `ZCLIENTS_ALERTS`
cancels it).

### B2 (P1, verify). Web notifications never appear
`Notification` exists, `Notification.permission` is `default`, and the app has
no notification permission/display path, so Cliq messages, Mail and CRM
reminders never notify. Implement per-profile permission (asked once per
origin, remembered in Settings › Privacy like camera/mic, Zoho hosts only) and
show them through `UNUserNotificationCenter` (already used for downloads),
title prefixed with the client name; clicking one brings that client/tab to
the front. Use public API if the macOS SDK has it, otherwise the guarded
private WebKit notification delegate; document either way.

### B3 (P2). Gesture credit is app-wide, not per page
`LAST_INPUT` (`web.rs` `note_input` / `recent_gesture`) is one global
timestamp: a click in tab A gives any foreground page 5 s (popups) / 10 s
(downloads) of "the user just clicked" credit. `ZWebView` already records
`last_click`; make `recent_gesture` take the requesting web view and use its
own last input. Policy tests for both cases.

### B4 (P2). CoreUI asset-catalog error spam
The unified log shows ~156 `CUICatalog … unable to find a bundle … with
identifier 'com.zport.swift'` errors in 2 days: something looks up named
images/colours in an asset catalog the bundle doesn't have. Find the lookups
(`NSImage imageNamed:`, `NSColor colorNamed:` or similar), use SF Symbols /
theme.rs values instead (or ship an `Assets.car`), and confirm the error is
gone with `/usr/bin/log show --last 10m --predicate 'process == "ZClients"'`.

### B5 (P2). Memory pressure on an 8 GB Mac
"memory pressure 2: keeping N tabs awake" was logged 58 times in 3 days, so
tabs are slept often and reload when revisited (the user's "pages refresh by
themselves"). Don't change the caps blindly. Add: a small "asleep" mark on
slept tab chips (tooltip "Asleep to save memory — click to wake"), and a
diagnostics line with the WebContent footprint per tab (via the existing
unified-log PIDs) so the user can see which page costs most.

### B6 (P2, verify). Swipe-back inside horizontally scrolling Zoho grids
`setAllowsBackForwardNavigationGestures(true)` is on. Check in the test app on
a wide Zoho Analytics pivot / Sheet: a two-finger swipe right at the grid's
left edge must not navigate the tab back while the user is editing. If it
does, keep the gesture but ignore it while the page has an editable element
focused, or add a Settings toggle.

### B7 (P2). No test exercises in-page UI
None of the self-tests (`security`, `a11y`, `address2`, `editor`,
`downloads`, `search`, `stability`) checks that ordinary page UI works
(panels, drawers, drag-and-drop, first click). A4 adds `parity`; also add a
drag-and-drop check (HTML5 `dragstart`/`drop` between two elements with a
real drag) because Zoho Analytics' column panel is drag-and-drop. Add the new
modes to `docs/TESTING.md`.

### B8. Backlog carried from 2.0 (do not implement without the user's go-ahead; list them in the final report)
- Closing the main window quits (behind the ⌘Q confirm) instead of hiding.
- Custom SAML identity providers (an organisation's own ADFS/Ping) are not trusted.
- Log360 Cloud SA/SG/CN not trusted (no HTTPS answer when checked).
- Lock/globe glyph sits before, not inside, the address field.
- No VoiceOver announcement for the profile sheet's inline error.
- Manual passes never run with real accounts (Light/Dark + accent, Larger
  Text, VoiceOver, EU/IN/UK accounts, SSO, Wi-Fi off/on). This gap is how the
  Part A bug shipped: add "open Zoho Analytics report → Settings panel opens"
  and "Print button prints" to the manual checklist.

---

## Definition of done

1. A0 findings written down with evidence (console errors, Safari result,
   bisect result). Root cause named, not guessed.
2. On the user's Mac, in the **installed** 2.3.0, the user confirms: the
   Zoho Analytics Settings panel opens from the gear (with ZClients active and
   with another app active), and the slide animation looks like Chrome.
3. `ZCLIENTS_SELFTEST=parity` passes, plus every existing self-test mode, plus
   `cargo test` / clippy / `scripts/check.sh`.
4. `docs/PRIVATE-API.md`, `docs/TESTING.md`, `docs/BACKLOG.md`, `README.md`
   updated. One commit per ID, tag `v2.3.0`, merged to `master` only after the
   user confirms point 2, 2.2.0 kept in `backup/`.
5. Final report to the user: each ID → fixed / not reproduced / deferred,
   with one line of evidence each.
