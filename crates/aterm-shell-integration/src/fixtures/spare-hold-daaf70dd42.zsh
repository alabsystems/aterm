__aterm_spare_hold() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    local __aterm_rc=$SECONDS __aterm_hs0="" __aterm_go="" __aterm_ss=${SAVEHIST-}
    local -i __aterm_n=0 __aterm_ss_set=${+SAVEHIST}
    [[ ${(t)SAVEHIST} != *readonly* ]] || __aterm_ss_set=2
    local -A __aterm_h
    (( __aterm_zstat_loaded )) || { zmodload -F zsh/stat b:zstat 2>/dev/null && __aterm_zstat_loaded=1 }
    if [[ -n ${HISTFILE-} ]] && (( __aterm_zstat_loaded )) &&
       zstat -H __aterm_h -- "$HISTFILE" 2>/dev/null; then
        __aterm_hs0="${__aterm_h[mtime]}:${__aterm_h[size]}:${__aterm_h[inode]}"
    fi
    (( __aterm_ss_set == 2 )) || SAVEHIST=0
    __aterm_start_mark spare
    while :; do
        if IFS= read -r __aterm_go < "$__aterm_spare_fifo"; then
            [[ $__aterm_go == go ]] && break
        elif (( ++__aterm_n >= 50 )) || [[ ! -p $__aterm_spare_fifo ]]; then
            exit 0
        fi
    done 2>/dev/null
    unset __aterm_spare_mode __aterm_spare_fifo
    case $__aterm_ss_set in
        1) SAVEHIST=$__aterm_ss ;;
        0) unset SAVEHIST ;;
    esac
    typeset -g __aterm_spare_reload=""
    if [[ -n ${HISTFILE-} ]]; then
        __aterm_h=()
        if [[ -z $__aterm_hs0 ]] || ! zstat -H __aterm_h -- "$HISTFILE" 2>/dev/null ||
           [[ "${__aterm_h[mtime]}:${__aterm_h[size]}:${__aterm_h[inode]}" != $__aterm_hs0 ]]; then
            __aterm_spare_reload=1
        fi
    fi
    local -i __aterm_i=${precmd_functions[(ie)__aterm_first_precmd]}
    typeset -ga __aterm_spare_hooks
    __aterm_spare_hooks=( ${precmd_functions[1,__aterm_i-1]} )
    (( ! ${+functions[precmd]} )) || __aterm_spare_hooks[1,0]=( precmd )
    [[ ${(t)SECONDS} == *readonly* ]] || SECONDS=$__aterm_rc
    return 0
}
