# shellcheck shell=bash
if [ -n "$S" ]; then
    case " $S" in
        *" Reconciled=True "*) pass_test "E-21" ;;
        *) fail_test "E-21" "x" ;;
    esac
    if [ "$S" = "ok" ]; then
        pass_test "E-22"
    fi
    pass_test "E-23" # want-flag
fi
