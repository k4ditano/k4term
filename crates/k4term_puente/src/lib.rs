//  El puente con la barra k4.
//
//  Todo lo que k4term sabe de la casa vive aquí y en ningún otro sitio: leer
//  el tema que publica la barra, entender los marcadores que manda la shell y
//  contarle a la barra lo que ha pasado. Si un día alguien usa esta terminal
//  sin k4 delante, este crate se queda callado y no pasa nada — de eso trata
//  tenerlo aparte.

pub mod barra;
pub mod osc;
pub mod tema;
pub mod trabajos;

pub use osc::{Escaner, Suceso};
pub use tema::Tema;
pub use trabajos::Aviso;
