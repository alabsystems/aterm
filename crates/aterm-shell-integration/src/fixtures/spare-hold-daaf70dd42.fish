function __aterm_spare_hold
    set -l ahead (__aterm_prompt_handlers_ahead)
    __aterm_start_mark spare (count $ahead)
    set -l n 0
    while true
        if read -l go <"$__aterm_spare_fifo" 2>/dev/null
            test "$go" = go; and break
        else
            set n (math $n + 1)
            if test $n -ge 50; or not test -p "$__aterm_spare_fifo"
                exit 0
            end
        end
    end
    set -e __aterm_spare_fifo
    history merge 2>/dev/null
    for handler in $ahead
        $handler >/dev/null 2>&1
    end
end
