# Backlog after 2.0.0

Every P0, P1 and P2 finding of the 2.0 work order was implemented (see the
commit log, one commit per finding ID). What was deliberately left, and why:

| Item | Status | Why |
|---|---|---|
| S1 option: closing the main window hides it instead of quitting | Not done | The work order makes this the owner's call. Closing still quits, through the same confirmation as ⌘Q. |
| U3: custom SAML identity providers (an organisation's own ADFS/Ping host) | Not done | Only a fixed, tested list (Microsoft, Apple, Okta, OneLogin; Google was already allowed) is trusted. Adding an arbitrary IdP host needs a per-client setting with its own policy tests. |
| U4: Log360 Cloud in SA, SG and CN | Not trusted | `log360cloud.manageengine.{sa,sg,com.cn}` gave no HTTPS answer on 2026-10-26. Remove the entry from `LOG360_UNAVAILABLE` (policy.rs) once Zoho serves it there. |
| U4: data centre for a brand-new client | By design | Unknown until the client loads its first Zoho app page; until then the launcher uses `.zoho.com`, which Zoho redirects. |
| U10: notification permission | By design | macOS asks once ("ZClients would like to send notifications"); if declined, nothing is shown. |
| D4: darkening all seven weak palette colours | Owner choice | Text colour now follows luminance (always ≥ 4.5:1); only Blue had to change (no text colour reached 4.5:1 on it). If white initials are preferred everywhere, darken Amber, Teal, Lime, Green, Orange, Cyan one step. |
| D7: lock/globe glyph *inside* the address field | Partly | The glyph sits just before the field; drawing it inside needs a custom NSTextFieldCell. |
| U2: VoiceOver announcement of the sheet's inline error | Partly | The red line has an accessibility label and focus moves to the field to fix; no spoken announcement is posted. |
| Manual passes (Light/Dark with a changed accent, "Larger Text", VoiceOver script, real Zoho accounts in EU/IN/UK, SSO, Wi-Fi off/on) | Not run here | Need a signed-in account / a person at the Mac. Checklist in `docs/TESTING.md` › New in 2.0.0. |

# Backlog after 2.3.0

| Item | Status | Why |
|---|---|---|
| A2: Chrome-parity shim (`requestIdleCallback`, `scheduler.postTask`) | Not done | A0 found no page error from a missing API: the Analytics panel failed only because the first click on an inactive window never reached the page (fixed by A1). WebKit's own `RequestIdleCallbackEnabled` exists (private `WKPreferences _setEnabled:forFeature:`), but its status on macOS 27.0.1 is "Testable" and Safari ships it off, so turning it on, or shimming it in the page world, would change the code path Zoho tests against Safari. Add it only when a page is shown to fail without it. |
