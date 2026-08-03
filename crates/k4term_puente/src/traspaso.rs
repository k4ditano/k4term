//  Pasar una sesión viva de un frontal a otro.
//
//  Mover una terminal «de la isla a una ventana» no es abrir otra igual: es
//  que el shell que tienes dentro —y el claude que esté pensando, y el `make`
//  a medias— sigan corriendo sin enterarse. Y se puede, porque lo que ata al
//  proceso de dentro es el ESCLAVO del PTY, que no se toca. Lo único que
//  cambia de manos es el maestro: quién está al otro lado leyendo y
//  escribiendo.
//
//  Un descriptor no se manda por una tubería como si fuera un número —en el
//  otro proceso ese número no significa nada—, así que va por un socket Unix
//  con SCM_RIGHTS, que es la forma que tiene el núcleo de decir «este
//  descriptor mío pasa a ser tuyo». Junto a él viaja el equipaje: dónde está
//  la sesión, cómo se llama, de qué tamaño iba y con qué repintar la pantalla.
//
//      quien la suelta            quien la coge
//      ──────────────             ─────────────
//      ofrecer(fd, equipaje)  →   recoger(ruta)
//              │                        │
//              └── espera a que          └── (fd, equipaje)
//                  alguien la coja
//
//  Lo que NO viaja: el estado del VT, que vive en la memoria del que suelta.
//  Por eso el equipaje lleva `pintura`: los bytes con los que el nuevo dueño
//  deja la pantalla como estaba. Y los programas de pantalla completa se
//  repintan solos en cuanto se les cambia el tamaño.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use serde::{Deserialize, Serialize};

//  Lo que hace falta saber de una sesión que llega, además de su descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Equipaje {
    pub cwd: String,
    pub titulo: String,
    pub cols: u16,
    pub filas: u16,
    //  Con qué dejar la pantalla como estaba: texto con sus secuencias de
    //  color, listo para meterlo por el VT del que la recibe.
    pub pintura: String,
}

impl Default for Equipaje {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            titulo: String::new(),
            cols: 80,
            filas: 24,
            pintura: String::new(),
        }
    }
}

//  Dónde se dejan los sockets del traspaso. En el directorio de ejecución del
//  usuario, que es lo que existe para esto y se limpia solo al cerrar sesión.
fn corral() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(base).join("k4term")
}

//  Ofrecer la sesión: se abre un socket, se espera a que alguien se conecte y
//  se le pasa el descriptor con su equipaje.
//
//  Devuelve la ruta —que hay que darle a quien la vaya a coger— y un aviso
//  para enterarse de cuándo se la han llevado: hasta ese momento la sesión
//  sigue siendo del que la ofrece, y solo después puede soltarla.
pub fn ofrecer(fd: RawFd, equipaje: &Equipaje) -> Result<(PathBuf, Receiver<bool>), String> {
    let dir = corral();
    std::fs::create_dir_all(&dir).map_err(|e| format!("no se pudo crear {dir:?}: {e}"))?;

    let ruta = dir.join(format!("traspaso-{}.sock", std::process::id()));
    //  Un socket viejo con el mismo nombre impediría escuchar; si está ahí es
    //  de un traspaso que no llegó a hacerse.
    let _ = std::fs::remove_file(&ruta);

    let escucha =
        UnixListener::bind(&ruta).map_err(|e| format!("no se pudo escuchar en {ruta:?}: {e}"))?;

    let (tx, rx) = channel::<bool>();
    let carga = serde_json::to_vec(equipaje).map_err(|e| e.to_string())?;
    let ruta_hilo = ruta.clone();

    thread::spawn(move || {
        let entregado = match escucha.accept() {
            Ok((flujo, _)) => mandar(&flujo, fd, &carga).is_ok(),
            Err(_) => false,
        };
        //  El socket no tiene por qué quedarse ahí: o se ha entregado, o no se
        //  va a entregar.
        let _ = std::fs::remove_file(&ruta_hilo);
        let _ = tx.send(entregado);
    });

    Ok((ruta, rx))
}

//  Coger una sesión ofrecida. Devuelve el descriptor —ya nuestro— y el
//  equipaje.
pub fn recoger(ruta: &Path) -> Result<(OwnedFd, Equipaje), String> {
    let flujo =
        UnixStream::connect(ruta).map_err(|e| format!("no se pudo conectar a {ruta:?}: {e}"))?;
    let (fd, carga) = recibir(&flujo)?;
    let equipaje: Equipaje = serde_json::from_slice(&carga).map_err(|e| e.to_string())?;
    Ok((fd, equipaje))
}

//  El descriptor va pegado a los CUATRO primeros bytes —el largo del
//  equipaje—, porque SCM_RIGHTS necesita viajar con datos de verdad: un
//  mensaje sin nada dentro no lo entrega el núcleo. El resto del equipaje se
//  escribe después, ya como un flujo normal.
fn mandar(flujo: &UnixStream, fd: RawFd, carga: &[u8]) -> Result<(), String> {
    let largo = (carga.len() as u32).to_le_bytes();

    let enviados = unsafe { mandar_con_fd(flujo.as_raw_fd(), fd, &largo) };
    if enviados != largo.len() as isize {
        return Err(format!("no se pudo pasar el descriptor ({enviados})"));
    }

    let mut flujo = flujo;
    flujo.write_all(carga).map_err(|e| e.to_string())?;
    flujo.flush().map_err(|e| e.to_string())
}

fn recibir(flujo: &UnixStream) -> Result<(OwnedFd, Vec<u8>), String> {
    let (fd, largo) = unsafe { recibir_con_fd(flujo.as_raw_fd())? };

    let mut carga = vec![0u8; largo];
    let mut flujo = flujo;
    flujo.read_exact(&mut carga).map_err(|e| e.to_string())?;

    Ok((fd, carga))
}

//  ── las dos llamadas que el núcleo pide en crudo ──────────────────
//
//  No hay forma de hacer esto sin `sendmsg`/`recvmsg`: la biblioteca estándar
//  no expone los mensajes auxiliares, que es donde viaja el descriptor.
unsafe fn mandar_con_fd(socket: RawFd, fd: RawFd, datos: &[u8]) -> isize {
    unsafe {
        let mut iov = libc::iovec {
            iov_base: datos.as_ptr() as *mut libc::c_void,
            iov_len: datos.len(),
        };

        let mut espacio = [0u8; 64];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = espacio.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;

        let cabecera = libc::CMSG_FIRSTHDR(&msg);
        if cabecera.is_null() {
            return -1;
        }
        (*cabecera).cmsg_level = libc::SOL_SOCKET;
        (*cabecera).cmsg_type = libc::SCM_RIGHTS;
        (*cabecera).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const RawFd as *const u8,
            libc::CMSG_DATA(cabecera),
            std::mem::size_of::<RawFd>(),
        );

        libc::sendmsg(socket, &msg, 0)
    }
}

unsafe fn recibir_con_fd(socket: RawFd) -> Result<(OwnedFd, usize), String> {
    unsafe {
        let mut largo = [0u8; 4];
        let mut iov = libc::iovec {
            iov_base: largo.as_mut_ptr() as *mut libc::c_void,
            iov_len: largo.len(),
        };

        let mut espacio = [0u8; 64];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = espacio.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;

        let leidos = libc::recvmsg(socket, &mut msg, 0);
        if leidos != largo.len() as isize {
            return Err(format!("el traspaso llegó a medias ({leidos})"));
        }

        let cabecera = libc::CMSG_FIRSTHDR(&msg);
        if cabecera.is_null()
            || (*cabecera).cmsg_level != libc::SOL_SOCKET
            || (*cabecera).cmsg_type != libc::SCM_RIGHTS
        {
            return Err("el traspaso llegó sin descriptor".to_string());
        }

        let mut fd: RawFd = -1;
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(cabecera),
            &mut fd as *mut RawFd as *mut u8,
            std::mem::size_of::<RawFd>(),
        );
        if fd < 0 {
            return Err("el descriptor que llegó no vale".to_string());
        }

        Ok((OwnedFd::from_raw_fd(fd), u32::from_le_bytes(largo) as usize))
    }
}

//  Cambiar el tamaño de un PTY que no es de `portable_pty` sino nuestro, por
//  descriptor. Es lo único que hacía falta de la biblioteca y no viene con el
//  descriptor pelado.
pub fn medir(fd: RawFd, cols: u16, filas: u16) {
    unsafe {
        let medida = libc::winsize {
            ws_row: filas,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        libc::ioctl(fd, libc::TIOCSWINSZ, &medida);
    }
}

//  Quién manda ahora mismo en ese PTY: el grupo de procesos que tiene el
//  frente. Con una sesión heredada no hay hijo al que preguntarle el
//  directorio —lo adoptó el sistema—, pero el núcleo sí sabe quién está
//  delante, y de ahí cuelga su `/proc/<pid>/cwd`.
pub fn grupo_al_frente(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    //  Un traspaso de verdad, con un descriptor de verdad: se ofrece una
    //  tubería, se recoge por el socket y se comprueba que lo que se escribe
    //  por un lado sale por el que llegó al otro proceso. Si el descriptor no
    //  cruzara, esto no leería nada.
    #[test]
    fn el_descriptor_cruza_con_su_equipaje() {
        let (leer, escribir) = std::io::pipe().expect("tubería");
        let equipaje = Equipaje {
            cwd: "/tmp".to_string(),
            titulo: "prueba".to_string(),
            cols: 90,
            filas: 16,
            pintura: "hola".to_string(),
        };

        let (ruta, aviso) = ofrecer(leer.as_raw_fd(), &equipaje).expect("ofrecer");
        let (fd, recibido) = recoger(&ruta).expect("recoger");

        assert_eq!(recibido.cols, 90);
        assert_eq!(recibido.titulo, "prueba");
        assert_eq!(recibido.pintura, "hola");
        assert!(aviso.recv().unwrap_or(false));

        //  El descriptor recibido es OTRO número que apunta al mismo sitio.
        let mut escribir = escribir;
        escribir.write_all(b"cruzado").expect("escribir");
        drop(escribir);

        let mut llegado = std::fs::File::from(fd);
        let mut texto = String::new();
        llegado.read_to_string(&mut texto).expect("leer");
        assert_eq!(texto, "cruzado");
    }
}
