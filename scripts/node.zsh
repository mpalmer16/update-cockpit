#!/bin/zsh

source "$UC_REPO_ROOT/scripts/lib/common.zsh"
source "$UC_REPO_ROOT/scripts/lib/nvm.zsh"

ensure_nvm_loaded || {
    add_warning "nvm not found"
    finish_task
    exit $?
}

run_cmd nvm install stable --reinstall-packages-from=current || exit $?
run_cmd nvm alias default stable || exit $?

if [ "$UC_DRY_RUN" = "0" ]; then
    nvm use stable >/dev/null || exit $?
    echo "Active Node: $(node -v)"
    echo "Active npm:  $(npm -v)"
else
    echo "[dry-run] nvm use stable"
    echo "[dry-run] node -v"
    echo "[dry-run] npm -v"
fi

finish_task
exit $?
