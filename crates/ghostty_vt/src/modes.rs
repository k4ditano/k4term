//  Lo que la aplicación de dentro pide sin decirlo en voz alta.
//
//  El VT resuelve la pantalla, pero hay cosas suyas que su API C no cuenta: si
//  quiere el pegado entre corchetes (DECSET 2004), si quiere el ratón (1000,
//  1002, 1003, 1006), cómo se llama la ventana (OSC 0/2) o si ha pedido copiar
//  algo al portapapeles (OSC 52). Todo eso viaja por el mismo chorro del PTY,
//  así que se mira al vuelo y los bytes siguen intactos hacia el terminal.
//
//  Vive aquí y no dentro de cada terminal porque hay DOS —la ventana y la de
//  la isla— y una sola respuesta correcta: con una copia en cada una, el mismo
//  programa acabaría portándose distinto según dónde lo abras.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

//  Lo que se guarda de una lectura para la siguiente. Una secuencia puede
//  venir partida entre dos lecturas del PTY —pasa a menudo— y sin cola se
//  perdería justo la que cambia el modo.
const TOPE_COLA: usize = 2048;

#[derive(Debug, Default)]
pub struct ModeTracker {
    bracketed_paste: bool,
    mouse_x10: bool,
    mouse_button_event: bool,
    mouse_any_event: bool,
    mouse_sgr: bool,
    title: Option<String>,
    clipboard_write: Option<String>,
    tail: Vec<u8>,
}

impl ModeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    //  Cualquiera de los tres modos de ratón: con uno solo que esté puesto, la
    //  aplicación espera que le contemos los clics en vez de que seleccionemos
    //  nosotros.
    pub fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_x10 || self.mouse_button_event || self.mouse_any_event
    }

    pub fn mouse_sgr_enabled(&self) -> bool {
        self.mouse_sgr
    }

    pub fn mouse_button_event_enabled(&self) -> bool {
        self.mouse_button_event
    }

    pub fn mouse_any_event_enabled(&self) -> bool {
        self.mouse_any_event
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn take_clipboard_write(&mut self) -> Option<String> {
        self.clipboard_write.take()
    }

    //  Lo que hay que escribirle al PTY para pegar `texto`.
    //
    //  Dos cuidados que no son adorno: los corchetes cuando la aplicación los
    //  pide —sin ellos, vim en modo inserción autoindenta cada línea pegada y
    //  sale una escalera— y el salto de línea como RETORNO. Un `\n` crudo lo
    //  aceptan unos editores de línea y otros no; el retorno es lo que manda
    //  la tecla Intro de verdad, así que es lo que espera todo el mundo.
    pub fn paste_payload(&self, texto: &str) -> Vec<u8> {
        let limpio = texto.replace("\r\n", "\r").replace('\n', "\r");
        if !self.bracketed_paste {
            return limpio.into_bytes();
        }

        let mut bytes = Vec::with_capacity(limpio.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(limpio.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > TOPE_COLA {
            let sobra = self.tail.len() - TOPE_COLA;
            self.tail.drain(0..sobra);
        }

        let buf = std::mem::take(&mut self.tail);
        self.scan_modes(&buf);
        self.scan_osc(&buf);
        self.tail = buf;
    }

    //  CSI ? Ps [;Ps…] h|l — poner y quitar modos privados.
    fn scan_modes(&mut self, buf: &[u8]) {
        let mut i = 0usize;
        while i + 2 < buf.len() {
            if buf[i] != 0x1b || buf[i + 1] != b'[' || buf[i + 2] != b'?' {
                i += 1;
                continue;
            }

            let mut k = i + 3;
            let mut nums: Vec<u32> = Vec::new();
            let mut num: u32 = 0;
            let mut hubo_digito = false;

            let fin = loop {
                let Some(&b) = buf.get(k) else {
                    //  Se acabó el trozo a media secuencia: lo que quede en la
                    //  cola se volverá a mirar con la lectura siguiente.
                    break None;
                };

                if b.is_ascii_digit() {
                    hubo_digito = true;
                    num = num.saturating_mul(10).saturating_add((b - b'0') as u32);
                    k += 1;
                    continue;
                }

                if b == b';' {
                    if hubo_digito {
                        nums.push(num);
                    }
                    num = 0;
                    hubo_digito = false;
                    k += 1;
                    continue;
                }

                if b == b'h' || b == b'l' {
                    if hubo_digito {
                        nums.push(num);
                    }
                    break Some((k, b == b'h'));
                }

                //  Cualquier otra cosa no es un modo privado: se abandona y se
                //  sigue buscando desde el byte siguiente al escape.
                break None;
            };

            match fin {
                Some((k, puesto)) => {
                    for ps in nums {
                        match ps {
                            2004 => self.bracketed_paste = puesto,
                            1000 => self.mouse_x10 = puesto,
                            1002 => self.mouse_button_event = puesto,
                            1003 => self.mouse_any_event = puesto,
                            1006 => self.mouse_sgr = puesto,
                            _ => {}
                        }
                    }
                    i = k + 1;
                }
                None => i += 1,
            }
        }
    }

    //  OSC Ps ; cuerpo (BEL | ESC \) — título y portapapeles.
    fn scan_osc(&mut self, buf: &[u8]) {
        let mut titulo: Option<String> = None;
        let mut portapapeles: Option<String> = None;

        let mut j = 0usize;
        while j + 1 < buf.len() {
            if buf[j] != 0x1b || buf[j + 1] != b']' {
                j += 1;
                continue;
            }

            let mut k = j + 2;
            let mut ps: u32 = 0;
            let mut hubo_digito = false;
            while let Some(&b) = buf.get(k) {
                if b.is_ascii_digit() {
                    hubo_digito = true;
                    ps = ps.saturating_mul(10).saturating_add((b - b'0') as u32);
                    k += 1;
                    continue;
                }
                if b == b';' {
                    k += 1;
                }
                break;
            }
            if !hubo_digito || k >= buf.len() {
                j += 1;
                continue;
            }

            let inicio = k;
            while k < buf.len() {
                match buf[k] {
                    0x07 => {
                        anotar(ps, &buf[inicio..k], &mut titulo, &mut portapapeles);
                        k += 1;
                        break;
                    }
                    0x1b if buf.get(k + 1) == Some(&b'\\') => {
                        anotar(ps, &buf[inicio..k], &mut titulo, &mut portapapeles);
                        k += 2;
                        break;
                    }
                    _ => k += 1,
                }
            }

            j = k.max(j + 1);
        }

        if let Some(t) = titulo {
            self.title = Some(t);
        }
        if let Some(p) = portapapeles {
            self.clipboard_write = Some(p);
        }
    }
}

fn anotar(ps: u32, cuerpo: &[u8], titulo: &mut Option<String>, portapapeles: &mut Option<String>) {
    match ps {
        0 | 2 => *titulo = Some(String::from_utf8_lossy(cuerpo).into_owned()),
        52 => {
            if let Some(texto) = decode_osc_52(cuerpo) {
                *portapapeles = Some(texto);
            }
        }
        _ => {}
    }
}

//  OSC 52 ; <selección> ; <base64>. Solo se atiende la selección del
//  portapapeles («c»): las demás —primaria, cortar— no las pide casi nadie y
//  atenderlas a medias sería peor que no hacerlo.
fn decode_osc_52(payload: &[u8]) -> Option<String> {
    let mut partes = payload.splitn(2, |b| *b == b';');
    let seleccion = partes.next()?;
    let datos = partes.next()?;

    if !seleccion.contains(&b'c') || datos.is_empty() {
        return None;
    }

    let crudo = STANDARD.decode(datos).ok()?;
    Some(String::from_utf8_lossy(&crudo).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_follows_decset_2004() {
        let mut m = ModeTracker::new();
        assert!(!m.bracketed_paste_enabled());
        m.feed(b"\x1b[?2004h");
        assert!(m.bracketed_paste_enabled());
        m.feed(b"\x1b[?2004l");
        assert!(!m.bracketed_paste_enabled());
    }

    #[test]
    fn mouse_modes_follow_decset() {
        let mut m = ModeTracker::new();
        m.feed(b"\x1b[?1002h\x1b[?1006h");
        assert!(m.mouse_reporting_enabled());
        assert!(m.mouse_button_event_enabled());
        assert!(m.mouse_sgr_enabled());
        m.feed(b"\x1b[?1002l");
        assert!(!m.mouse_reporting_enabled());
    }

    #[test]
    fn split_sequences_survive_between_feeds() {
        let mut m = ModeTracker::new();
        m.feed(b"\x1b[?20");
        m.feed(b"04h");
        assert!(m.bracketed_paste_enabled());
    }

    #[test]
    fn paste_wraps_and_normalizes_newlines() {
        let mut m = ModeTracker::new();
        assert_eq!(m.paste_payload("a\r\nb\nc"), b"a\rb\rc".to_vec());
        m.feed(b"\x1b[?2004h");
        assert_eq!(m.paste_payload("hola"), b"\x1b[200~hola\x1b[201~".to_vec());
    }

    #[test]
    fn osc_52_decodes_clipboard_writes() {
        let mut m = ModeTracker::new();
        m.feed(b"\x1b]52;c;aG9sYQ==\x07");
        assert_eq!(m.take_clipboard_write().as_deref(), Some("hola"));
        assert_eq!(m.take_clipboard_write(), None);
    }

    #[test]
    fn osc_title_is_read_with_both_terminators() {
        let mut m = ModeTracker::new();
        m.feed(b"\x1b]0;uno\x07");
        assert_eq!(m.title(), Some("uno"));
        m.feed(b"\x1b]2;dos\x1b\\");
        assert_eq!(m.title(), Some("dos"));
    }
}
