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

//  Lo que el reloj decide contar, ya con los segundos hechos. Quien lo recibe
//  elige cómo se cuenta: la ventana lo manda por el IPC de la barra y la
//  sesión de la isla lo escribe en su salida, que es el canal que ya tiene.
pub enum Parte {
    Empezado { mandato: String, segundos: u64 },
    Acabado {
        mandato: String,
        salida: i32,
        segundos: u64,
    },
}

//  Un hilo por ventana. Devuelve por dónde hablarle; cuando el emisor se
//  cierra, el hilo se acaba solo.
pub fn notificador(pid: u32) -> Sender<Aviso> {
    notificador_con(move |parte| match parte {
        Parte::Empezado { mandato, segundos } => barra::avisar_inicio(pid, &mandato, segundos),
        Parte::Acabado {
            mandato,
            salida,
            segundos,
        } => barra::avisar_fin(pid, &mandato, salida, segundos),
    })
}

//  El mismo reloj, contándoselo a quien tú digas. Está separado porque la
//  decisión —«solo si cruza el umbral estando vivo»— es lo que tiene valor y
//  no queremos dos copias que se separen: la de la ventana y la de la isla
//  tienen que comportarse igual.
pub fn notificador_con(contar: impl Fn(Parte) + Send + 'static) -> Sender<Aviso> {
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
                            contar(Parte::Acabado {
                                mandato,
                                salida,
                                segundos: desde.elapsed().as_secs(),
                            });
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some((mandato, desde, anunciado)) = &mut curso {
                        if !*anunciado {
                            contar(Parte::Empezado {
                                mandato: mandato.clone(),
                                segundos: desde.elapsed().as_secs(),
                            });
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
