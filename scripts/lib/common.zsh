#!/bin/zsh

set -u
set -o pipefail

: "${UC_DRY_RUN:=0}"
: "${UC_VERBOSE:=0}"

typeset -a TASK_WARNINGS

run_cmd() {
    if [ "$UC_DRY_RUN" = "1" ]; then
        echo "[dry-run] $*"
        return 0
    fi

    if [ "$UC_VERBOSE" = "1" ]; then
        echo "+ $*"
    fi

    "$@"
}

add_warning() {
    local note="$1"
    TASK_WARNINGS+=("$note")
    echo "Warning: $note"
}

finish_task() {
    if [ "${#TASK_WARNINGS[@]}" -ne 0 ]; then
        return 10
    fi

    return 0
}
