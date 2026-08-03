mod config;
mod font;
mod session;

pub mod view;

//  El puente con la casa lo pone el anfitrión: la vista no sabe —ni tiene
//  por qué— si hay una barra o un Edinot detrás. Sin registrar nada, estas
//  puertas simplemente no existen.
type Anotador = dyn Fn(String, String) + Send + Sync + 'static;
static ANOTADOR: std::sync::OnceLock<Box<Anotador>> = std::sync::OnceLock::new();

pub fn registrar_anotador(f: impl Fn(String, String) + Send + Sync + 'static) {
    let _ = ANOTADOR.set(Box::new(f));
}

pub(crate) fn edinot_disponible() -> bool {
    ANOTADOR.get().is_some()
}

//  Devolver la sesión a la isla. Solo existe si hay barra que la reciba: una
//  terminal suelta no tiene isla a la que volver, y entonces la tecla no hace
//  nada — como con Edinot.
type Mudanza = dyn Fn(String, String) + Send + Sync + 'static;
static MUDANZA: std::sync::OnceLock<Box<Mudanza>> = std::sync::OnceLock::new();

pub fn registrar_mudanza(f: impl Fn(String, String) + Send + Sync + 'static) {
    let _ = MUDANZA.set(Box::new(f));
}

pub(crate) fn mudanza_disponible() -> bool {
    MUDANZA.get().is_some()
}

//  La pintura de la pantalla y el título, que es lo que hace falta para que
//  la sesión aparezca al otro lado tal como estaba.
pub(crate) fn mudar(pintura: String, titulo: String) {
    if let Some(f) = MUDANZA.get() {
        f(pintura, titulo);
    }
}

//  Abrir los Ajustes de la casa. Igual que con Edinot: si nadie lo registra,
//  el botón ni se pinta — una terminal suelta no tiene dónde mandarte.
type Abridor = dyn Fn() + Send + Sync + 'static;
static AJUSTES: std::sync::OnceLock<Box<Abridor>> = std::sync::OnceLock::new();

pub fn registrar_ajustes(f: impl Fn() + Send + Sync + 'static) {
    let _ = AJUSTES.set(Box::new(f));
}

pub(crate) fn ajustes_disponibles() -> bool {
    AJUSTES.get().is_some()
}

pub(crate) fn abrir_ajustes() {
    if let Some(f) = AJUSTES.get() {
        f();
    }
}

pub(crate) fn anotar_en_segundo_plano(titulo: String, texto: String) {
    if let Some(f) = ANOTADOR.get() {
        f(titulo, texto);
    }
}

pub use config::TerminalConfig;
// Quien use la vista necesita hablar de colores: que no tenga que enterarse
// de qué crate viene el tipo.
pub use font::{default_terminal_font, default_terminal_font_features};
pub use ghostty_vt::Rgb;
pub use session::TerminalSession;

#[cfg(test)]
mod tests;
