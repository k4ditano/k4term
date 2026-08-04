//! Lo que NO se le pasa a la shell.
//!
//! Una terminal hereda el entorno de quien la abrió, y eso normalmente está
//! bien. Con un agente por medio, no: cuando Claude Code lanza un proceso le
//! deja unas marcas —`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PID`…—
//! que dicen «vienes de dentro de una sesión». Si esa cadena llega a otra
//! terminal, todo lo que se abra ahí sigue creyéndolo.
//!
//! Y tiene consecuencia de verdad: un `claude` arrancado con esas marcas se
//! toma por sesión hija, y una sesión hija no escribe su transcripción. El
//! historial deja de guardarse y `/resume` no lista nada. Pasó: una sesión
//! abrió una kitty, y desde entonces todo lo que descendía de ella —barra
//! incluida, y cada terminal que la barra abría— arrastraba la herencia.
//!
//! k4term nunca es una subshell de un agente: lo que abre es la terminal de
//! alguien, así que estas marcas se quitan siempre.

/// Las variables con las que un agente marca a sus hijos.
pub const MARCAS_DE_AGENTE: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
];

/// Si el proceso actual las lleva puestas; para poder avisarlo.
pub fn heredadas() -> Vec<&'static str> {
    MARCAS_DE_AGENTE
        .iter()
        .copied()
        .filter(|v| std::env::var_os(v).is_some())
        .collect()
}
