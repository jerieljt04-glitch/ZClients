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

## Updates

ZClients has no self-updater. A signed update mechanism (e.g. Sparkle 2 with
EdDSA-signed appcasts, rollback protection by build number, release notes,
user-controlled checks) needs an HTTPS host for the appcast and a signing key;
it is not implemented. Until then updates are installed by hand from a
notarized zip whose checksum matches `.sha256`.
