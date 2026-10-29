# Private WebKit/AppKit API in ZClients

Every private selector is listed here with its purpose, its guard, and what
happens when WebKit stops calling or answering it. None can grant a
permission by failing: each fails closed or to a no-op.

| Selector | Where | Why | Guard / fallback |
|---|---|---|---|
| `_webView:requestDisplayCapturePermissionForOrigin:initiatedByFrame:withSystemAudio:decisionHandler:` (UI delegate, called *by* WebKit) | `web.rs` `Coordinator` | WebKit offers `getDisplayMedia` (Meet "Present now") only to apps that answer this; Safari does. | The decision comes from `policy::decide(ScreenCapture)`: exact call origins get the macOS picker, other shown pages get a confirmation first, everything else is denied. If WebKit stops calling it, screen sharing is refused (WebKit's default) — nothing is granted. |
| `_webView:saveDataToFile:suggestedFilename:mimeType:originatingURL:` (UI delegate, called *by* WebKit) | `web.rs` `Coordinator` | The PDF viewer's download button hands the document here; without it the button does nothing. | Data is saved through `downloads::save_from_tab` (safe file name, new file only, quarantined or removed). If WebKit stops calling it, the button does nothing; File › Download Current Page still works. |
| `_webView:printFrame:` (UI delegate, called *by* WebKit) | `web.rs` `Coordinator::print_frame` (2.3) | WebKit ignores `window.print()` unless the UI delegate answers this; Zoho's Print buttons call it. The `pdfFirstPageSize:completionHandler:` variant is deliberately not implemented: WebKit then calls this one and lets the page continue at once. | Only the tab in front or a key popup, never after "Don't allow more dialogs", one panel at a time. If WebKit stops calling it, `window.print()` does nothing again (File › Print still works). |
| `_printOperationWithPrintInfo:forFrame:` (WKWebView) | `web.rs` `print_view` (2.3) | Prints exactly the frame that called `window.print()` (a report iframe), not the whole tab. | `respondsToSelector:` first; if missing, or it returns nil, the public `printOperationWithPrintInfo:` prints the whole page. |
| `_webProcessIdentifier` (WKWebView) | `web.rs` `tab_footprints` (2.3, Help › Export Diagnostics); `selftest.rs` `kill_web_content` | The pid of each awake tab's WebContent process, so diagnostics can show which page costs the most memory (`proc_pid_rusage` → `ri_phys_footprint`, public). | `respondsToSelector:` first; a tab whose pid is unknown is left out of the list. Not used for any decision. |
| `_hasSleepDisabler` (WKWebView) | `web.rs` `has_sleep_disabler`, self-tests only | Diagnostics: is WebKit itself holding a sleep assertion. | `respondsToSelector:` first; false if missing. Not used for any decision. |
| `_setMockCaptureDevicesEnabled:` / `_setMockCaptureDevicesPromptEnabled:` (WKPreferences) | `web.rs` `configuration` | Self-tests use WebKit's fake camera/mic/screens. | Only when `util::is_test_build()` **and** `ZCLIENTS_MOCK_MEDIA` is set. Never in the real app. |
| `_performActionForControl:` (PDF HUD) | `selftest.rs` | Presses the PDF viewer's download button in a self-test. | Test builds only. |

Public replacements already used: camera/microphone permission
(`webView:requestMediaCapturePermissionForOrigin:…` → `WKPermissionDecision`),
stopping capture (`setCameraCaptureState:` / `setMicrophoneCaptureState:`),
Web Inspector (`isInspectable`), data-store removal and listing
(`removeDataStoreForIdentifier:` / `fetchAllDataStoreIdentifiers:`), find
(`findString:withConfiguration:`), user agent
(`WKWebViewConfiguration.applicationNameForUserAgent`).

Not available publicly, so not implemented: a find-in-page **match count**
(WebKit's public `WKFindResult` only says found / not found), and a public
"stop screen sharing" call (macOS's own screen-sharing indicator in the menu
bar stops it).
