//  El puente con la barra k4.
//
//  Todo lo que k4term sabe de la casa vive aquí y en ningún otro sitio: leer
//  el tema que publica la barra, entender los marcadores que manda la shell y
//  contarle a la barra lo que ha pasado. Si un día alguien usa esta terminal
//  sin k4 delante, este crate se queda callado y no pasa nada — de eso trata
//  tenerlo aparte.

pub mod ajustes;
pub mod barra;
pub mod edinot;
pub mod osc;
pub mod senal;
pub mod servidores;
pub mod tema;
pub mod trabajos;
pub mod traspaso;

pub use ajustes::Ajustes;
pub use osc::{Escaner, Suceso};
pub use servidores::Servidor;
pub use tema::Tema;
pub use trabajos::Aviso;
pub use traspaso::Equipaje;
