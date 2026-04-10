#!/bin/zsh

ensure_nvm_loaded() {
    export NVM_DIR="$HOME/.nvm"

    if [ ! -s "$NVM_DIR/nvm.sh" ]; then
        echo "nvm not found at $NVM_DIR/nvm.sh"
        return 1
    fi

    if [ "$UC_DRY_RUN" = "1" ]; then
        echo "[dry-run] source $NVM_DIR/nvm.sh"
        return 0
    fi

    set +u
    source "$NVM_DIR/nvm.sh"
    set -u
    return 0
}
