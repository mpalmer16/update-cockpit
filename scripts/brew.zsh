#!/bin/zsh

source "$UC_REPO_ROOT/scripts/lib/common.zsh"

run_cmd brew update || exit $?
run_cmd brew upgrade || exit $?

if [ "${UC_BREW_CLEANUP:=0}" = "1" ]; then
    run_cmd brew autoremove || exit $?
    run_cmd brew cleanup || exit $?
fi

finish_task
exit $?
