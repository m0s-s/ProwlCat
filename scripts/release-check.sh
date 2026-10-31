#!/usr/bin/env bash
# Run before tagging a release:   scripts/release-check.sh [X.Y.Z]
# Every check must pass; the tag is only pushed afterwards. The release workflow repeats the
# important ones on all platforms and publishes only if they all pass.
set -u
cd "$(dirname "$0")/.."

version=${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}
version=${version#v}
fail=0
step() { printf '\n== %s\n' "$1"; }
ok()   { printf '   ok: %s\n' "$1"; }
bad()  { printf '   FAIL: %s\n' "$1"; fail=1; }
warn() { printf '   warning: %s\n' "$1"; }

step "version $version"
manifest=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ "$manifest" = "$version" ] && ok "Cargo.toml says $manifest" || bad "Cargo.toml says $manifest"
top=$(sed -n 's/^## \(.*\)/\1/p' CHANGELOG.md | head -1)
[ "$top" = "$version" ] && ok "CHANGELOG.md top section is $top" || bad "CHANGELOG.md top section is '$top' (expected '$version', not Unreleased)"
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && bad "tag v$version already exists" || ok "tag v$version is free"

step "README screenshots"
shot=$(cat assets/screenshots/VERSION 2>/dev/null || echo none)
[ "$shot" = "$version" ] && ok "screenshots were made with $shot" || bad "screenshots are for $shot, not $version (run: cargo build --release -p prowlcat-cli && scripts/screenshots.sh, then look at them and commit)"

step "README animations"
gif=$(cat assets/gifs/VERSION 2>/dev/null || echo none)
[ "$gif" = "$version" ] && ok "animations were made with $gif" || bad "animations are for $gif, not $version (run: cargo build --release -p prowlcat-cli && scripts/gifs.sh, then look at them and commit)"

step "working tree"
[ -z "$(git status --porcelain --untracked-files=no)" ] && ok "no uncommitted changes to tracked files" || bad "uncommitted changes (commit them first)"
git fetch -q origin 2>/dev/null
if [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main 2>/dev/null)" ]; then ok "HEAD is origin/main"; else warn "HEAD differs from origin/main (push first, the tag must point at what is on GitHub)"; fi

step "lockfile"
cargo metadata --locked --format-version 1 >/dev/null 2>&1 && ok "Cargo.lock is up to date" || bad "Cargo.lock out of date (run: cargo update --workspace)"

step "tests"
cargo test --workspace --locked >/tmp/release-check-tests.log 2>&1 && ok "cargo test --workspace" || { bad "cargo test (see /tmp/release-check-tests.log)"; tail -20 /tmp/release-check-tests.log; }

step "clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings >/tmp/release-check-clippy.log 2>&1 && ok "clippy, warnings denied" || { bad "clippy (see /tmp/release-check-clippy.log)"; tail -20 /tmp/release-check-clippy.log; }

step "end-to-end on the release binary"
if cargo build --release --locked -p prowlcat-cli >/tmp/release-check-build.log 2>&1; then
  bin="$PWD/target/release/prowlcat"
  PROWLCAT_E2E_BIN="$bin" PROWLCAT_E2E_EXPECT_VERSION="$version" cargo test -p prowlcat-cli --test e2e --locked >/tmp/release-check-e2e.log 2>&1 \
    && ok "e2e passes against $bin" || { bad "e2e (see /tmp/release-check-e2e.log)"; tail -25 /tmp/release-check-e2e.log; }
else
  bad "release build (see /tmp/release-check-build.log)"
fi

step "third-party notices"
scripts/gen-third-party.sh >/dev/null 2>&1
git diff --quiet -- THIRD-PARTY-NOTICES.md && ok "THIRD-PARTY-NOTICES.md is current" || bad "THIRD-PARTY-NOTICES.md was out of date (regenerated: commit it)"

step "licenses"
if command -v cargo-deny >/dev/null 2>&1 && [ -f deny.toml ]; then
  cargo deny check licenses bans sources >/tmp/release-check-deny.log 2>&1 && ok "cargo deny licenses" || { bad "cargo deny (see /tmp/release-check-deny.log)"; tail -20 /tmp/release-check-deny.log; }
else
  warn "cargo-deny or deny.toml missing, license check skipped"
fi

printf '\n'
if [ "$fail" = 0 ]; then
  printf 'READY: tag with   git tag -a v%s -m "ProwlCat %s" && git push origin v%s\n' "$version" "$version" "$version"
else
  printf 'NOT READY: fix the failures above, then run this again.\n'
  exit 1
fi
