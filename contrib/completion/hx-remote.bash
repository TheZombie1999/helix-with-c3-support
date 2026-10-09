#!/usr/bin/env bash
# Bash completion script for hx-remote, the CLI client for Helix's D-Bus
# remote-control interface (see helix-remote/src/cli.rs).

_hx_remote_instances() {
    hx-remote list-instances 2>/dev/null
}

_hx_remote_pids() {
    _hx_remote_instances | sed -n 's/^org\.helix_editor\.Helix\.pid//p'
}

_hx_remote() {
    local cur prev cmd
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD - 1]}"
    cmd="${COMP_WORDS[1]}"

    if [ "$COMP_CWORD" -eq 1 ]; then
        mapfile -t COMPREPLY < <(compgen -W "open edit changed close reload watch list-files ls list-instances ps help" -- "$cur")
        return 0
    fi

    case "$prev" in
    --instance)
        mapfile -t COMPREPLY < <(compgen -W "$(_hx_remote_instances)" -- "$cur")
        return 0
        ;;
    --pid)
        mapfile -t COMPREPLY < <(compgen -W "$(_hx_remote_pids)" -- "$cur")
        return 0
        ;;
    esac

    case "$cmd" in
    open | edit)
        case "$cur" in
        -*)
            mapfile -t COMPREPLY < <(compgen -W "--no-focus --wait --no-wait --all --instance --pid -h --help" -- "$cur")
            ;;
        *)
            mapfile -t COMPREPLY < <(compgen -fd -- "$cur")
            ;;
        esac
        ;;
    changed)
        mapfile -t COMPREPLY < <(compgen -W "--no-focus --wait --no-wait --all --instance --pid -h --help" -- "$cur")
        ;;
    close)
        case "$cur" in
        -*)
            mapfile -t COMPREPLY < <(compgen -W "--force --all --instance --pid -h --help" -- "$cur")
            ;;
        *)
            mapfile -t COMPREPLY < <(compgen -fd -- "$cur")
            ;;
        esac
        ;;
    reload)
        case "$cur" in
        -*)
            mapfile -t COMPREPLY < <(compgen -W "--all --instance --pid -h --help" -- "$cur")
            ;;
        *)
            mapfile -t COMPREPLY < <(compgen -fd -- "$cur")
            ;;
        esac
        ;;
    watch)
        mapfile -t COMPREPLY < <(compgen -W "--interval --all --instance --pid -h --help" -- "$cur")
        ;;
    list-files | ls)
        mapfile -t COMPREPLY < <(compgen -W "--instance --pid -h --help" -- "$cur")
        ;;
    list-instances | ps | help) ;;
    esac
} && complete -o filenames -F _hx_remote hx-remote
