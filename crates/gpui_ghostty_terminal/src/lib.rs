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

//  Los servidores de uno, para el selector de la ventana.
//
//  La vista pinta, filtra y decide qué tecla hace qué; de dónde salen los
//  hosts y qué significa guardarlos lo sabe el anfitrión, que es quien conoce
//  `~/.ssh/config`. Esta capa no tiene por qué saber nada de ssh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Servidor {
    pub alias: String,
    pub usuario: String,
    pub host: String,
    pub puerto: String,
    //  Lo que ssh entiende y no se ve en la lista, pero sí se configura.
    pub clave: String,
    pub salto: String,
    //  De qué color se pone la terminal mientras estás dentro, y qué túneles
    //  se levantan con la conexión. Nuestros los dos.
    pub tinte: String,
    pub tuneles: String,
    pub favorito: bool,
    //  Y lo nuestro. Las etiquetas van como texto con espacios porque así es
    //  como se escriben en el formulario; quien las guarde ya las partirá.
    pub etiquetas: String,
    pub al_conectar: String,
    //  Un destino escrito al vuelo, todavía sin guardar: se pinta distinto y
    //  es lo único que se puede guardar.
    pub rapido: bool,
}

impl Servidor {
    pub fn detalle(&self) -> String {
        let usuario = if self.usuario.is_empty() {
            String::new()
        } else {
            format!("{}@", self.usuario)
        };
        let puerto = if self.puerto.is_empty() || self.puerto == "22" {
            String::new()
        } else {
            format!(":{}", self.puerto)
        };
        format!("{usuario}{}{puerto}", self.host)
    }
}

//  Todo lo que el selector necesita del mundo de fuera, junto: cinco cierres
//  en vez de cinco huecos sueltos, que se registran de una vez y no puede
//  quedarse la mitad puesta.
//  Los tipos de cada cierre, con nombre: escritos a la cara son ilegibles y
//  clippy tiene razón en decirlo.
type Listar = dyn Fn() -> Vec<Servidor> + Send + Sync;
type AlVuelo = dyn Fn(&str) -> Option<Servidor> + Send + Sync;
type PorAlias = dyn Fn(&str) + Send + Sync;
type PorServidor = dyn Fn(&Servidor) + Send + Sync;

pub struct GestorServidores {
    pub listar: Box<Listar>,
    pub al_vuelo: Box<AlVuelo>,
    pub visitar: Box<PorAlias>,
    pub guardar: Box<PorServidor>,
    pub favorito: Box<PorAlias>,
    pub borrar: Box<PorAlias>,
    //  Entrar y salir de un sitio: quien lo sepa lo cuenta —hoy, la barra,
    //  que lo enseña en la píldora—. La vista no sabe que existe una barra.
    pub conectado: Box<PorAlias>,
    pub desconectado: Box<dyn Fn() + Send + Sync>,
    //  De nombre de color a color. Lo resuelve el anfitrión porque la paleta
    //  de la casa es suya: aquí solo se sabe mezclar.
    pub color: Box<dyn Fn(&str) -> Option<(u8, u8, u8)> + Send + Sync>,
}

static GESTOR: std::sync::OnceLock<GestorServidores> = std::sync::OnceLock::new();

pub fn registrar_servidores(gestor: GestorServidores) {
    let _ = GESTOR.set(gestor);
}

pub(crate) fn gestor_servidores() -> Option<&'static GestorServidores> {
    GESTOR.get()
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
