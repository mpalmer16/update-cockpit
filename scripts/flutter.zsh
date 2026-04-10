#!/bin/zsh

source "$UC_REPO_ROOT/scripts/lib/common.zsh"

flutter_bin="$(command -v flutter || true)"

if [ -z "$flutter_bin" ]; then
    add_warning "Flutter not found"
    finish_task
    exit $?
fi

flutter_bin_real="${flutter_bin:A}"
flutter_root="$(cd "$(dirname "$flutter_bin_real")/.." && pwd)"

if [ -d "$flutter_root/.git" ]; then
    echo "Cleaning local Flutter SDK changes in $flutter_root..."
    run_cmd git -C "$flutter_root" reset --hard || exit $?
    run_cmd git -C "$flutter_root" clean -fd || exit $?
fi

run_cmd flutter channel stable || exit $?
run_cmd flutter upgrade || exit $?

finish_task
exit $?
