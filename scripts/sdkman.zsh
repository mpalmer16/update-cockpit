#!/bin/zsh

source "$UC_REPO_ROOT/scripts/lib/common.zsh"

if [ ! -s "$HOME/.sdkman/bin/sdkman-init.sh" ]; then
    add_warning "SDKMAN not found"
    finish_task
    exit $?
fi

if [ "$UC_DRY_RUN" = "1" ]; then
    echo "[dry-run] source $HOME/.sdkman/bin/sdkman-init.sh"
    echo "[dry-run] sdk selfupdate"
    echo "[dry-run] sdk update"
    echo "[dry-run] sdk upgrade"
    finish_task
    exit $?
fi

set +u
source "$HOME/.sdkman/bin/sdkman-init.sh"

if [ "$UC_VERBOSE" = "1" ]; then
    echo "+ sdk selfupdate"
fi
sdk selfupdate
self_rc=$?

if [ "$UC_VERBOSE" = "1" ]; then
    echo "+ sdk update"
fi
sdk update
update_rc=$?

if [ "$UC_VERBOSE" = "1" ]; then
    echo "+ sdk upgrade"
fi
sdk upgrade
upgrade_rc=$?
set -u

if [ $self_rc -ne 0 ] || [ $update_rc -ne 0 ] || [ $upgrade_rc -ne 0 ]; then
    add_warning "SDKMAN non-zero codes (selfupdate=$self_rc, update=$update_rc, upgrade=$upgrade_rc)"
fi

finish_task
exit $?
