//  El timbre de una ventana.
//
//  La barra no tiene forma de hablarle a una ventana de GPUI: no escucha en
//  ningún socket y sus atajos son suyos. Pero el sistema sí sabe llamarla, y
//  para esto basta el timbre más viejo que hay — una señal.
//
//  Se usa para una sola cosa: que `SUPER+ALT+T` valga en los dos sentidos. Si
//  estás en la isla, se lleva la sesión a una ventana; si estás en la ventana,
//  la barra le toca el timbre y la ventana la devuelve. Un gesto, no dos.
//
//  SIGUSR1 se bloquea en todos los hilos y se espera con `sigwait`, que es la
//  forma sin sorpresas: nada de manejadores corriendo en mitad de cualquier
//  cosa, solo un hilo esperando.

use std::sync::mpsc::{Receiver, channel};
use std::thread;

//  Llamar a la ventana de ese pid.
pub fn llamar(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGUSR1);
    }
}

//  Y ponerse a la escucha. Hay que llamarlo PRONTO —antes de que se levanten
//  los demás hilos— porque la máscara de señales se hereda: un hilo que nazca
//  antes de bloquearla podría comerse la llamada.
pub fn escuchar() -> Receiver<()> {
    let (tx, rx) = channel::<()>();

    unsafe {
        let mut mascara: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mascara);
        libc::sigaddset(&mut mascara, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &mascara, std::ptr::null_mut());

        thread::spawn(move || {
            loop {
                let mut cual: libc::c_int = 0;
                if libc::sigwait(&mascara, &mut cual) != 0 {
                    break;
                }
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
    }

    rx
}
