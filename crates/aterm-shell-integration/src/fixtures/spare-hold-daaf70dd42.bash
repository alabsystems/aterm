__aterm_spare_hold() {
    local __aterm_rc=$SECONDS __aterm_go="" __aterm_n=0 __aterm_own=${__aterm_spare_rc_list-}
    local __aterm_hf_set=${HISTFILE+x} __aterm_hf=${HISTFILE-}
    unset HISTFILE 2>/dev/null || :
    __aterm_start_mark spare
    while :; do
        if IFS= read -r __aterm_go < "$__aterm_spare_fifo"; then
            [[ $__aterm_go == go ]] && break
        else
            __aterm_n=$((__aterm_n + 1))
            { [[ $__aterm_n -lt 50 ]] && [[ -p $__aterm_spare_fifo ]]; } || exit 0
        fi
    done 2>/dev/null
    unset __aterm_spare_mode __aterm_spare_fifo __aterm_spare_rc_list
    if [[ -n $__aterm_hf_set ]]; then
        [[ -n ${HISTFILE+x} ]] || HISTFILE=$__aterm_hf
        if [[ -z $__aterm_own && -f $HISTFILE ]]; then
            [[ -z ${HISTSIZE+x} ]] || eval 'HISTSIZE=$HISTSIZE' 2>/dev/null || :
            [[ -z ${HISTFILESIZE+x} ]] || eval 'HISTFILESIZE=$HISTFILESIZE' 2>/dev/null || :
            history -c
            history -r
        fi
    fi
    SECONDS=$__aterm_rc
    return 0
}
