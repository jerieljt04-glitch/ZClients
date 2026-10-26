# Data migration and rollback

## 2.0.0 → 2.1.0

No data format change. Behaviour: the Google Search workspace now opens
Zoho pages in its own tab (with its own, separate sign-in) instead of
sending them to a client, and blocks every website other than Google
Search and Zoho (they no longer open in the default browser). Google Meet
links still open in a client. A Search tab left on a Zoho page reopens in
the Search workspace; Search tabs saved by the Swift build without a
workspace are repaired as before.

## 1.5.0 → 2.0.0

2.0.0 changes no data format. The only addition:

| Data | 2.0.0 change | Automatic? | Readable by 1.5.0 afterwards? |
|---|---|---|---|
| `clients.json` | New optional per-client field `zohoDataCentre` (e.g. `"zoho.eu"`), learned from the last Zoho *app* page the client loaded (never www.zoho.* or the shared US sign-in page). Absent until learned; `version` stays 4. | Yes | **Yes.** 1.5.0 and the Swift build ignore unknown keys (and drop it if they save; 2.0.0 learns it again on the next app page). Unit test `data_centre_is_learned_saved_and_optional` round-trips the file with the field present and absent. |
| `settings.json`, vault, session backups, PDF previews, `cleanup.json` | Unchanged | — | Yes |

Rollback to 1.5.0: quit ZClients, then
`rm -rf /Applications/ZClients.app && ditto backup/ZClients-1.5.0.app /Applications/ZClients.app`.
Clients, tabs, saved logins (vault format 2) and sign-ins all carry over.

Visible changes that are not data: the Blue swatch is one step darker
(#2470dd) for readable initials; clients that already use the old blue
keep it.

## 1.4.0 → 1.5.0

All data lives in `~/Library/Application Support/ZPortSwift/` (the test app:
`ZPortSwift Test/`). WebKit's per-profile stores
(`~/Library/WebKit/com.zport.swift/WebsiteDataStore/<profile UUID>`) are not
migrated or touched by the upgrade.

| Data | 1.5.0 change | Automatic? | Readable by 1.4.0 afterwards? |
|---|---|---|---|
| `clients.json` | New optional fields: `keepSignedIn`, `sessionRetentionDays` (per profile), `originClientId` (Search tabs). Written only when set. Duplicate profile/tab ids are split (the duplicate profile gets a new, empty store and "(recovered)" in its name). | Yes, on load | **Yes.** 1.4.0 and the Swift build ignore the new keys (and drop them if they save; 1.5.0 then uses the defaults: keep signed in, 90 days). |
| `clients.json.bak` | New: a copy of the last file that loaded cleanly, refreshed at each launch. Used by Restore in recovery. | Yes | n/a |
| Vault records (`Vault/*.sealed`) | Format 2: AES-GCM with associated data (bundle id, format, kind, profile UUID). Format-1 records are re-sealed 2 s after launch, each with an atomic write; then `Vault/format` = `2` is written and format-1 files are refused. Records that do not open with the vault key are renamed `*.rejected`, never trusted. | Yes, needs the Keychain key (one "Always Allow" after updating) | **No.** 1.4.0 cannot open format-2 records: it shows no saved login and has no session backup to restore (WebKit's own cookies still keep profiles signed in). |
| Session backup content | Jar version 3 (`savedAt`, `promoted` ledger). Versions 1/2 decode; their 400-day expiries are capped at the profile's retention on restore. | Yes | Only through the vault (above) |
| PDF previews | Each preview moves from the shared flat folder into its own random 0700 folder (file 0600). A preview that is not a real PDF is dropped from its tab (the tab keeps its source address). Unreferenced previews are deleted at launch and on quit. `PDF Previews/` becomes 0700. | Yes, 4 s after launch | **Yes** (1.4.0 grants WebKit the whole previews folder, which contains the new sub-folders). |
| `settings.json` | New (download permissions, zoom levels, keyboard choices, developer mode). | — | Ignored |
| `cleanup.json` | New: unfinished profile deletions, retried at launch. | — | Ignored (1.4.0 will not retry them) |

Nothing is migrated while the profile file is unreadable: recovery mode is
read-only until you choose Restore / Start with No Clients / Quit (2.0.0
wording; 1.5.0 said "Profiles").

## Rollback to 1.4.0

1. Quit ZClients.
2. `rm -rf /Applications/ZClients.app && ditto backup/ZClients-1.4.0.app /Applications/ZClients.app`
   (`./build.sh` keeps the replaced app in `backup/ZClients-<version>.app`).
3. Profiles, tabs, PDF tabs and sign-ins (WebKit's cookies) carry over.
4. Saved logins are not readable by 1.4.0: re-enter them in Customise if needed.

## Rolling forward again after a rollback

* Records 1.4.0 wrote (format 1) are **refused** by 1.5.0 once the vault is
  marked migrated — that is the protection against an old record being planted
  back. Saved logins entered under 1.4.0 must be entered again; session
  backups are rebuilt automatically from the live cookie store.
* If you deliberately want 1.5.0 to import format-1 records again (only on a
  Mac you trust), quit ZClients and delete `Vault/format`; the next launch
  re-seals every format-1 record and marks the vault again.

## Recovery of a damaged profile file

At launch, a `clients.json` that exists but cannot be read (permissions, I/O
error), is damaged (truncated, not JSON, wrong shape) or was written by a newer
ZClients is never replaced. ZClients asks:

* **Restore Last Good Copy** (when `clients.json.bak` is valid): the damaged
  file is renamed `clients.json.damaged-<time>` and the backup is used.
* **Start with No Profiles**: the damaged file is kept aside the same way.
  WebKit data stores are not deleted; restoring the old file later brings the
  signed-in profiles back.
* **Quit**: nothing is written.
