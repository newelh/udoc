#!/usr/bin/env bash
# check-public-api.sh --  alpha-development freeze gate.
#
# Per  §4 + : hard gate on facade public-API churn.
# The W0 spike (T0-PUBLICAPI-SPIKE) verified cargo-public-api 0.51 produces
# reviewable diffs. Per the spike finding, we baseline BOTH crates because
# cargo-public-api does not walk into inherent methods of re-exported types
# (e.g. `Document::diagnostics()` defined in udoc-core would slip past a
# udoc-only check).
#
# Bypass for intentional surface changes: regenerate the baselines via
#   cargo public-api --simplified -p udoc      > crates/udoc/.public-api-baseline.txt
#   cargo public-api --simplified -p udoc-core > crates/udoc-core/.public-api-baseline.txt
# and commit the diff with explicit acknowledgement in the PR description.

set -e

if ! command -v cargo-public-api > /dev/null; then
    echo "FAIL: cargo-public-api not installed."
    echo "  Install with: cargo install cargo-public-api --version 0.51.0 --locked"
    exit 1
fi

fail=0

for crate in udoc udoc-core; do
    baseline="crates/${crate}/.public-api-baseline.txt"
    if [ ! -f "$baseline" ]; then
        echo "FAIL: missing baseline for ${crate} at ${baseline}"
        fail=1
        continue
    fi
    current=$(cargo public-api --simplified -p "$crate" 2>/dev/null)
    if ! diff -u "$baseline" <(echo "$current") > /tmp/public-api-diff-${crate}.txt; then
        echo "FAIL: public-api drift on -p ${crate}:"
        cat /tmp/public-api-diff-${crate}.txt
        echo ""
        fail=1
    fi
done

if [ "$fail" -ne 0 ]; then
    echo ""
    echo "Public-API gate failed. The facade surface drifted from baseline."
    echo "If intentional: regenerate the baseline files and commit the diff with"
    echo "explicit acknowledgement in the PR description."
    exit 1
fi

echo "OK: public-api gate clean (no drift on udoc or udoc-core)."
exit 0
