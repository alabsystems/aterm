# aterm warm spare (docs/DESIGN-warm-spare-2026-10-16.md): a spare waits here, before any startup file.
if [ -n "${ATERM_REKEY_PATH:-}" ]; then
  __aterm_spare_fifo="${ATERM_REKEY_PATH%/rekey/*}/spare/${ATERM_REKEY_PATH##*/}"
  if [ -p "$__aterm_spare_fifo" ]; then
    printf '\033]633;P;AtermSpare=parked;id=%s\a' "${ATERM_SHELL_NONCE:-}" >/dev/tty 2>/dev/null
    __aterm_spare_n=0 __aterm_spare_cd=
    while :; do
      if IFS= read -r __aterm_spare_go < "$__aterm_spare_fifo"; then
        case $__aterm_spare_go in
          run) __aterm_spare_mode=warm; break ;;
          go) break ;;
          'go /'*) __aterm_spare_cd=${__aterm_spare_go#go }; break ;;
        esac
      else
        __aterm_spare_n=$((__aterm_spare_n + 1))
        { [ "$__aterm_spare_n" -lt 50 ] && [ -p "$__aterm_spare_fifo" ]; } || exit 0
      fi
    done 2>/dev/null
    if [ -n "${__aterm_spare_cd:-}" ]; then
      if [ -n "${ZSH_VERSION:-}" ]; then
        builtin cd -q -L -- "$__aterm_spare_cd" 2>/dev/null && OLDPWD=$PWD
      elif [ -n "${OLDPWD+1}" ]; then
        __aterm_spare_o=$OLDPWD
        builtin cd -L -- "$__aterm_spare_cd" 2>/dev/null && OLDPWD=$__aterm_spare_o
      else
        builtin cd -L -- "$__aterm_spare_cd" 2>/dev/null && { unset OLDPWD; export OLDPWD; }
      fi
    fi
    SECONDS=0
  fi
fi
[ -n "${__aterm_spare_mode:-}" ] || unset __aterm_spare_fifo __aterm_spare_mode
unset __aterm_spare_n __aterm_spare_go __aterm_spare_cd __aterm_spare_o
