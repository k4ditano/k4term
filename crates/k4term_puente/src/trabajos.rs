//  Qué se está cociendo, contado a la barra.
//
//  Entre el lector del PTY y el IPC hay este hilo a propósito: el lector no
//  puede pararse a lanzar procesos —es el que mantiene viva la pantalla— y
//  además hay una decisión que tomar con un reloj delante. La regla es que un
//  mandato solo se anuncia si CRUZA el umbral estando vivo, así que las
//  órdenes rápidas no cuestan ni un proceso.

use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use crate::barra;

pub enum Aviso {
    Empieza { mandato: String },
    Acaba { salida: i32 },
}

//  Un hilo por ventana. Devuelve por dónde hablarle; cuando el emisor se
//  cierra, el hilo se acaba solo.
pub fn notificador(pid: u32) -> Sender<Aviso> {
    let (tx, rx) = channel::<Aviso>();
    let umbral = barra::umbral_pildora();

    std::thread::spawn(move || {
        // (mandato, desde cuándo, ¿ya lo sabe la barra?)
        let mut curso: Option<(String, Instant, bool)> = None;

        loop {
            //  Sin nada pendiente de anunciar no hay prisa: se duerme hasta
            //  que pase algo. Con algo a medio cocer, justo lo que le quede.
            let espera = match &curso {
                Some((_, desde, false)) => umbral.saturating_sub(desde.elapsed()),
                _ => Duration::from_secs(3600),
            };

            match rx.recv_timeout(espera) {
                Ok(Aviso::Empieza { mandato }) => {
                    curso = Some((mandato, Instant::now(), false));
                }
                Ok(Aviso::Acaba { salida }) => {
                    if let Some((mandato, desde, anunciado)) = curso.take() {
                        if anunciado {
                            barra::avisar_fin(pid, &mandato, salida, desde.elapsed().as_secs());
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some((mandato, desde, anunciado)) = &mut curso {
                        if !*anunciado {
                            barra::avisar_inicio(pid, mandato, desde.elapsed().as_secs());
                            *anunciado = true;
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    tx
}
