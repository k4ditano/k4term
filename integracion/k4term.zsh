#  Integración de k4term con zsh.
#
#  Marca dónde empieza y dónde acaba cada mandato para que la terminal pueda
#  medirlo y avisar cuando uno largo termina. Son las convenciones de siempre
#  —OSC 133 de iTerm2, OSC 633 de VS Code, OSC 7 para el directorio—, así que
#  esto no le hace daño a ninguna otra terminal.
#
#  En ~/.zshrc:
#
#      [ -n "$K4TERM_INTEGRACION" ] && source "$K4TERM_INTEGRACION"
#
#  o, sin variables:
#
#      source ~/Proyectos/k4term/integracion/k4term.zsh

[[ "$TERM_PROGRAM" == "k4term" ]] || return 0

_k4term_precmd() {
    local salida=$?
    printf '\033]133;D;%s\007' "$salida"
    printf '\033]7;file://%s%s\007' "${HOST:-$(hostname)}" "$PWD"
    printf '\033]133;A\007'
}

_k4term_preexec() {
    printf '\033]633;E;%s\007' "$1"
    printf '\033]133;C\007'
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _k4term_precmd
add-zsh-hook preexec _k4term_preexec
