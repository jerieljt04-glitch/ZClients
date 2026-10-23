#!/bin/bash
# The CI gate. Every step must pass; run it before any release.
#   scripts/check.sh            all gates
#   FUZZ_CASES=200000 scripts/check.sh   longer randomized (property) runs
set -euo pipefail
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env" 2>/dev/null || true
step() { echo "==> $*"; }

step "rustfmt";            cargo fmt --check
step "clippy";             cargo clippy --locked --all-targets -- -D warnings
step "tests";              cargo test --locked
step "property tests (${FUZZ_CASES:-20000} cases)"
PROPTEST_CASES="${FUZZ_CASES:-20000}" cargo test --locked --release proptests
step "dependency advisories"; cargo audit --deny warnings
step "licenses, bans, sources"; cargo deny check
step "universal build";    MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --locked --release --target aarch64-apple-darwin --target x86_64-apple-darwin
if rustup toolchain list | grep -q nightly && rustup component list --toolchain nightly --installed | grep -q miri; then
  step "miri (pure-Rust tests; Objective-C and SecRandom calls cannot run under Miri)"
  PURE="$(cargo test --locked --quiet -- --list 2>/dev/null | grep -E '^(policy|session)::tests::' | sed 's/: test$//')"
  # shellcheck disable=SC2086
  MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --locked -- --exact $PURE \
    keychain::tests::legacy_records_open_until_the_vault_is_migrated keychain::tests::wipe_zeroes \
    model::tests::duplicate_ids_are_split_never_merged model::tests::unreadable_or_damaged_profiles_are_problems_not_empty_lists \
    model::tests::reads_and_round_trips_the_swift_session_file pdf::tests::mime_types \
    downloads::tests::risky_types_are_recognised_whatever_the_case downloads::tests::server_names_are_reduced_to_one_plain_file
else
  echo "    (miri skipped: install with 'rustup toolchain install nightly --component miri')"
fi
step "all checks passed"
