#!/usr/bin/env bash
# Entrypoint for the quint-payments Docker image.
#
# All quint invocations go through `nix shell` so the exact binary baked into
# the image layer is always used, regardless of PATH.
#
# Commands
# --------
#  test [quint-test-flags]
#      Run every named test run in the paymentsTests module.
#      Extra flags (e.g. --match=chargeback) are forwarded to `quint test`.
#
#  check [<invariant>] [quint-run-flags]
#      Simulate with the random explorer and check an invariant.
#      Invariant defaults to `safetyInv` when omitted.
#      e.g.  check heldLeqTotalInv --max-samples=50000
#
#  check-all [quint-run-flags]
#      Check every invariant in the spec one by one.
#
#  run [quint-run-flags]
#      Forward all flags to `quint run` (--main is pre-set).
#      e.g.  run --max-samples=5000 --max-steps=30 --seed=0xdeadbeef
#
#  -- [args...]
#      Raw escape hatch: execute the given command directly (no nix shell wrapping).
#      e.g.  -- quint parse /spec/payments.qnt
#
#  (anything else)
#      Executed as-is.

set -euo pipefail

SPEC="/spec/payments.qnt"
MAIN="paymentsTests"
NIX_SHELL=(
    nix shell
    --extra-experimental-features "nix-command flakes"
    "github:NixOS/nixpkgs#quint"
    --command
)

ALL_INVARIANTS=(
    totalEqAvailablePlusHeldInv
    availableNonNegativeInv
    heldNonNegativeInv
    totalNonNegativeInv
    heldLeqTotalInv
    disputedAmountNonNegativeInv
    chargebackClearsDisputeInv
    terminalStatesClearDisputeInv
    disputedHasPositiveAmountInv
    nonExistentTxIsCleanInv
)

cmd="${1:-test}"

case "$cmd" in

    # ------------------------------------------------------------------
    # test [quint-test-flags]
    # ------------------------------------------------------------------
    test)
        shift || true
        echo "==> quint test --main=${MAIN} ${SPEC} $*"
        exec "${NIX_SHELL[@]}" quint test --main="${MAIN}" "$@" "${SPEC}"
        ;;

    # ------------------------------------------------------------------
    # check [<invariant>] [quint-run-flags]
    # ------------------------------------------------------------------
    check)
        shift || true
        # First positional argument is the invariant name (optional)
        if [[ $# -gt 0 && "$1" != --* ]]; then
            invariant="$1"; shift
        else
            invariant="safetyInv"
        fi
        echo "==> quint run --main=${MAIN} --invariant=${invariant} ${SPEC} $*"
        exec "${NIX_SHELL[@]}" quint run \
            --main="${MAIN}" \
            --invariant="${invariant}" \
            "$@" "${SPEC}"
        ;;

    # ------------------------------------------------------------------
    # check-all [quint-run-flags]
    # ------------------------------------------------------------------
    check-all)
        shift || true
        failed=0
        for inv in "${ALL_INVARIANTS[@]}"; do
            echo ""
            echo "==> Checking: ${inv}"
            if "${NIX_SHELL[@]}" quint run \
                    --main="${MAIN}" \
                    --invariant="${inv}" \
                    --max-samples=2000 \
                    "$@" "${SPEC}"; then
                echo "    [PASS] ${inv}"
            else
                echo "    [FAIL] ${inv}" >&2
                failed=1
            fi
        done
        echo ""
        if [[ $failed -eq 0 ]]; then
            echo "All ${#ALL_INVARIANTS[@]} invariants passed."
        else
            echo "One or more invariants failed." >&2
            exit 1
        fi
        ;;

    # ------------------------------------------------------------------
    # run [quint-run-flags]
    # ------------------------------------------------------------------
    run)
        shift || true
        echo "==> quint run --main=${MAIN} ${SPEC} $*"
        exec "${NIX_SHELL[@]}" quint run --main="${MAIN}" "$@" "${SPEC}"
        ;;

    # ------------------------------------------------------------------
    # -- [args...]   (raw, no nix shell wrapping)
    # ------------------------------------------------------------------
    --)
        shift || true
        echo "==> $*"
        exec "$@"
        ;;

    # ------------------------------------------------------------------
    # Fallback
    # ------------------------------------------------------------------
    *)
        exec "$@"
        ;;
esac
