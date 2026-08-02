#  Integración de k4term con fish. Ver k4term.zsh para el porqué.
#
#  En ~/.config/fish/config.fish:
#
#      source ~/Proyectos/k4term/integracion/k4term.fish

if test "$TERM_PROGRAM" != k4term
    exit 0
end

function _k4term_preexec --on-event fish_preexec
    printf '\033]633;E;%s\007' "$argv"
    printf '\033]133;C\007'
end

function _k4term_postexec --on-event fish_postexec
    #  $status es el del mandato que acaba de correr: hay que capturarlo en la
    #  primera línea o lo pisa cualquier otra cosa.
    set -l salida $status
    printf '\033]133;D;%s\007' $salida
    printf '\033]7;file://%s%s\007' (hostname) "$PWD"
end
