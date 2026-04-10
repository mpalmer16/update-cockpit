#!/bin/zsh

source "$UC_REPO_ROOT/scripts/lib/common.zsh"

run_cmd juliaup update || exit $?
finish_task
exit $?
