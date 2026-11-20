# Building and releasing ZClients

## Version

`Cargo.toml` is the only source: `version` is `CFBundleShortVersionString`,
`[package.metadata.zclients] build` is `CFBundleVersion`. `build.rs` exposes
the build number to the app (About, Settings, diagnostics); `scripts/bundle.sh`
writes both into Info.plist. Bump both for every build you install.

## Gates (`scripts/check.sh`, also CI)

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked` (unit, migration, failure-injection and property tests)
4. Property tests at 20 000 cases (`FUZZ_CASES=` to raise) — the randomized
   "fuzz" pass over URL policy, cookie backups, vault envelopes, download names,
   profile decoding and redaction
5. `cargo audit --deny warnings`
6. `cargo deny check` (`deny.toml`: licences, bans, sources, advisories)
7. Universal build (arm64 + x86_64)
8. Miri over the pure modules, when a nightly toolchain with Miri is installed

## Local development builds (`./build.sh`)

* `./build.sh --test` — "ZClients Test", ad-hoc signed, never touches a
  keychain or real data. Use for every experiment.
* `scripts/setup-signing-identity.sh` — once per Mac: creates the
  "ZClients Local Signer" certificate in its own keychain (macOS asks for
  that keychain's password; nothing stores it).
* `./build.sh` — signs with the local "ZClients Local Signer" identity **only
  if its keychain is already unlocked**; the script never reads or passes a
  keychain password. Otherwise it signs ad hoc and says so. It asks before
  replacing /Applications/ZClients.app and keeps the replaced app in
  `backup/ZClients-<version>.app`.
* The old plaintext password file
  (`~/Library/Application Support/ZClients Signing/keychain-password`) is no
  longer used by anything; delete it when convenient.

Every build is checked by `scripts/verify-signature.sh`: bundle identifier,
hardened runtime, and exactly the camera + microphone entitlements.

## Production release (`scripts/release.sh`)

Needs a paid Apple Developer account. Provide, from CI secrets or the login
keychain (never files or command-line passwords):

* `DEVELOPER_ID` — "Developer ID Application: Name (TEAMID)"
* `TEAM_ID`
* `NOTARY_PROFILE` — created once with
  `xcrun notarytool store-credentials <name> --key … --key-id … --issuer …`

The script runs every gate, builds universal, signs with hardened runtime and a
secure timestamp, verifies identity/entitlements/Team ID, notarizes (fails
unless "Accepted"), staples, runs `spctl --assess`, and writes
`dist/ZClients-<version>.zip`, its SHA-256 and a CycloneDX SBOM. Any failure
stops the release.

In CI, import the Developer ID certificate into a temporary keychain created
for the job (random password generated in the job, deleted afterwards), only
in a protected environment for tagged builds.

## GitHub Releases (what people download today)

`.github/workflows/release.yml` builds the universal Mac app on a macOS runner,
signs it **ad hoc** (hardened runtime, same bundle id and entitlements, checked by
`scripts/verify-signature.sh`), packs `ZClients-<version>-mac.dmg` (drag to
Applications) plus its `.sha256`, and publishes a GitHub Release whose notes
explain the one-time "Open Anyway" step and list the tagged commit's message as
"What's new".

* New version: bump `Cargo.toml`, commit, then
  `git tag -a vX.Y.Z -m "ZClients X.Y.Z" && git push origin vX.Y.Z`.
* Existing tag: Actions › release › Run workflow, or
  `gh workflow run release.yml -f tag=vX.Y.Z` (re-running replaces the files).
* The tag must equal `Cargo.toml`'s version or the job stops.
* The repository is private: only people with access to it (collaborators)
  can see and download releases. Add someone under Settings › Collaborators.
* Mac only. The app is AppKit + WebKit; a Windows build needs a separate port.
* The owner's own Mac keeps using `./build.sh`: an ad-hoc-signed download is a
  different signer, so macOS would re-ask for camera/mic and Keychain access.
* Once there is a Developer ID, switch this workflow to `scripts/release.sh`
  (notarized, no Gatekeeper warning).

## Updates

ZClients has no self-updater. A signed update mechanism (e.g. Sparkle 2 with
EdDSA-signed appcasts, rollback protection by build number, release notes,
user-controlled checks) needs an HTTPS host for the appcast and a signing key;
it is not implemented. Until then updates are installed by hand from a
notarized zip whose checksum matches `.sha256`.
