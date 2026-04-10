#!/bin/zsh

source "$UC_REPO_ROOT/scripts/lib/common.zsh"
source "$UC_REPO_ROOT/scripts/lib/nvm.zsh"

ensure_nvm_loaded || {
    add_warning "nvm not found"
    finish_task
    exit $?
}

run_cmd nvm use stable || exit $?
run_cmd npm install -g @anthropic-ai/claude-code @openai/codex || exit $?

if [ "$UC_DRY_RUN" = "0" ]; then
    echo "Installed CLI package versions:"
    npm list -g --depth=0 @anthropic-ai/claude-code @openai/codex
else
    echo "[dry-run] npm list -g --depth=0 @anthropic-ai/claude-code @openai/codex"
fi

if [ "${UC_NPM_AUDIT:=0}" = "1" ]; then
    if [ "$UC_DRY_RUN" = "1" ]; then
        echo "[dry-run] npm audit --location=global"
    else
        npm audit --location=global
        audit_rc=$?
        if [ $audit_rc -ne 0 ]; then
            add_warning "npm audit reported issues (exit $audit_rc)"
        fi
    fi
fi

finish_task
exit $?
