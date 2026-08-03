use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

//  Lo que se lee del chorro y el VT no cuenta: modos privados, título y
//  portapapeles. Lo usan las dos terminales, la de ventana y la de la isla.
pub mod modes;
pub mod pintura;
pub mod raton;

pub use modes::ModeTracker;
pub use pintura::repintar;
pub use raton::{Boton, Suceso as SucesoRaton, encode_mouse};

#[derive(Debug)]
pub enum Error {
    CreateFailed,
    FeedFailed(i32),
    ScrollFailed(i32),
    DumpFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CreateFailed => write!(f, "terminal create failed"),
            Error::FeedFailed(code) => write!(f, "terminal feed failed: {code}"),
            Error::ScrollFailed(code) => write!(f, "terminal scroll failed: {code}"),
            Error::DumpFailed => write!(f, "terminal dump failed"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Terminal {
    ptr: NonNull<c_void>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleRun {
    pub start_col: u16,
    pub end_col: u16,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
}

impl KeyModifiers {
    fn bits(self) -> u16 {
        let mut bits = 0u16;
        if self.shift {
            bits |= 0x0001;
        }
        if self.control {
            bits |= 0x0002;
        }
        if self.alt {
            bits |= 0x0004;
        }
        if self.super_key {
            bits |= 0x0008;
        }
        bits
    }
}

//  Las siete formas de DECSCUSR. `Default` es «la que traiga la terminal de
//  casa»: quien pinta decide, que es lo correcto — el programa no ha pedido
//  nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

impl From<u16> for CursorStyle {
    fn from(valor: u16) -> Self {
        match valor {
            1 => CursorStyle::BlinkingBlock,
            2 => CursorStyle::SteadyBlock,
            3 => CursorStyle::BlinkingUnderline,
            4 => CursorStyle::SteadyUnderline,
            5 => CursorStyle::BlinkingBar,
            6 => CursorStyle::SteadyBar,
            _ => CursorStyle::Default,
        }
    }
}

impl CursorStyle {
    //  La figura, sin el parpadeo: bloque, subrayado o barra.
    pub fn figura(self) -> Figura {
        match self {
            CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => Figura::Bloque,
            CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => Figura::Subrayado,
            CursorStyle::BlinkingBar | CursorStyle::SteadyBar => Figura::Barra,
            CursorStyle::Default => Figura::Barra,
        }
    }

    //  Si el programa ha pedido que parpadee. `Default` no pide nada.
    pub fn parpadea(self) -> bool {
        matches!(
            self,
            CursorStyle::BlinkingBlock | CursorStyle::BlinkingUnderline | CursorStyle::BlinkingBar
        )
    }
}

//  Un enlace de OSC 8 y las columnas que ocupa, ambas incluidas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hyperlink {
    pub start_col: u16,
    pub end_col: u16,
    pub uri: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Figura {
    Bloque,
    Subrayado,
    Barra,
}

pub fn encode_key_named(name: &str, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    if name.is_empty() {
        return None;
    }

    let bytes = unsafe {
        ghostty_vt_sys::ghostty_vt_encode_key_named(name.as_ptr(), name.len(), modifiers.bits())
    };
    if bytes.ptr.is_null() || bytes.len == 0 {
        return None;
    }

    let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
    let out = slice.to_vec();
    unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
    Some(out)
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let ptr = unsafe { ghostty_vt_sys::ghostty_vt_terminal_new(cols, rows) };
        let ptr = NonNull::new(ptr).ok_or(Error::CreateFailed)?;
        Ok(Self { ptr })
    }

    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_set_default_colors(
                self.ptr.as_ptr(),
                fg.r,
                fg.g,
                fg.b,
                bg.r,
                bg.g,
                bg.b,
            )
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let rc = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_feed(self.ptr.as_ptr(), bytes.as_ptr(), bytes.len())
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::FeedFailed(rc))
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        let rc =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_resize(self.ptr.as_ptr(), cols, rows) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }

    //  Lo que el terminal tiene que CONTESTAR, y que hay que escribirle al
    //  PTY. Un terminal no solo recibe: le preguntan quién es (DA), dónde está
    //  el cursor (DSR) o si tiene puesto un modo (DECRQM), y quien pregunta se
    //  queda esperando por el mismo sitio por donde escribe.
    //
    //  Hay que llamarlo DESPUÉS de cada `feed` y escribir lo que devuelva. Si
    //  no, esas preguntas caen al vacío y el programa de delante se queda
    //  esperando algo que no va a llegar — que es lo que hace, por ejemplo,
    //  que un TUI se coloque donde no debe.
    pub fn take_responses(&mut self) -> Vec<u8> {
        let bytes =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_take_responses(self.ptr.as_ptr()) };
        if bytes.ptr.is_null() {
            return Vec::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let salida = slice.to_vec();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        salida
    }

    //  La forma de cursor que pide el programa de dentro (DECSCUSR, CSI Ps SP
    //  q). Vim la usa para decirte en qué modo estás sin escribirlo, y un
    //  terminal que la ignora pinta siempre la misma barra.
    pub fn cursor_style(&self) -> CursorStyle {
        CursorStyle::from(unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_cursor_style(self.ptr.as_ptr())
        })
    }

    //  Los enlaces (OSC 8) de una fila del hueco visible, ya en tramos. La
    //  fila se cuenta desde 0, como en el volcado de estilos.
    //
    //  Va por fila entera y no por celda porque un enlace no tiene por qué
    //  traer estilo propio: preguntar solo por el principio de cada tramo de
    //  color se dejaría fuera los enlaces colgados de texto corriente.
    pub fn row_hyperlinks(&self, row: u16) -> Vec<Hyperlink> {
        let bytes =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_row_hyperlinks(self.ptr.as_ptr(), row) };
        if bytes.ptr.is_null() || bytes.len == 0 {
            return Vec::new();
        }
        let crudo = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) }.to_vec();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };

        let mut salida = Vec::new();
        let mut i = 0usize;
        while i + 6 <= crudo.len() {
            let ini = u16::from_ne_bytes([crudo[i], crudo[i + 1]]);
            let fin = u16::from_ne_bytes([crudo[i + 2], crudo[i + 3]]);
            let largo = u16::from_ne_bytes([crudo[i + 4], crudo[i + 5]]) as usize;
            i += 6;
            if i + largo > crudo.len() {
                break;
            }
            salida.push(Hyperlink {
                start_col: ini,
                end_col: fin,
                uri: String::from_utf8_lossy(&crudo[i..i + largo]).into_owned(),
            });
            i += largo;
        }
        salida
    }

    //  El título que ha pedido la aplicación de dentro (OSC 0/2). `None` si
    //  no ha pedido ninguno. Es lo único que distingue de un vistazo una
    //  sesión de otra: sin esto, un selector de terminales solo puede decir
    //  «terminal 1, terminal 2».
    pub fn title(&self) -> Option<String> {
        let bytes = unsafe { ghostty_vt_sys::ghostty_vt_terminal_title(self.ptr.as_ptr()) };
        if bytes.ptr.is_null() {
            return None;
        }
        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Some(s)
    }

    pub fn dump_viewport(&self) -> Result<String, Error> {
        let bytes = unsafe { ghostty_vt_sys::ghostty_vt_terminal_dump_viewport(self.ptr.as_ptr()) };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(s)
    }

    pub fn dump_viewport_row(&self, row: u16) -> Result<String, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_viewport_row(self.ptr.as_ptr(), row)
        };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(s)
    }

    //  (fila por la que empieza lo que se ve, filas que hay en total), ambas
    //  en coordenadas del historial completo. De aquí sale la barra de
    //  desplazamiento y la colocación de cualquier marca apuntada en
    //  absoluto.
    pub fn viewport_position(&self) -> Option<(u32, u32)> {
        let mut arriba = 0u32;
        let mut total = 0u32;
        let ok = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_viewport_position(
                self.ptr.as_ptr(),
                &mut arriba,
                &mut total,
            )
        };
        ok.then_some((arriba, total))
    }

    //  Una fila del historial entero (0 = lo más viejo que se recuerda), no
    //  solo de lo que se ve. `None` cuando esa fila ya no existe: es la
    //  forma de saber dónde acaba el historial sin preguntar el tamaño.
    pub fn dump_screen_row(&self, row: u32) -> Option<String> {
        let bytes =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_dump_screen_row(self.ptr.as_ptr(), row) };
        if bytes.ptr.is_null() {
            return None;
        }
        if bytes.len == 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return None;
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let salida = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Some(salida)
    }

    pub fn dump_viewport_row_cell_styles(&self, row: u16) -> Result<Vec<CellStyle>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_viewport_row_cell_styles(
                self.ptr.as_ptr(),
                row,
            )
        };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }
        if bytes.len == 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Ok(Vec::new());
        }
        if bytes.len % 8 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 8);
        for chunk in slice.chunks_exact(8) {
            out.push(CellStyle {
                fg: Rgb {
                    r: chunk[0],
                    g: chunk[1],
                    b: chunk[2],
                },
                bg: Rgb {
                    r: chunk[3],
                    g: chunk[4],
                    b: chunk[5],
                },
                flags: chunk[6],
            });
        }

        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(out)
    }

    pub fn dump_viewport_row_style_runs(&self, row: u16) -> Result<Vec<StyleRun>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_viewport_row_style_runs(self.ptr.as_ptr(), row)
        };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }
        if bytes.len == 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Ok(Vec::new());
        }
        if bytes.len % 12 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 12);
        for chunk in slice.chunks_exact(12) {
            out.push(StyleRun {
                start_col: u16::from_ne_bytes([chunk[0], chunk[1]]),
                end_col: u16::from_ne_bytes([chunk[2], chunk[3]]),
                fg: Rgb {
                    r: chunk[4],
                    g: chunk[5],
                    b: chunk[6],
                },
                bg: Rgb {
                    r: chunk[7],
                    g: chunk[8],
                    b: chunk[9],
                },
                flags: chunk[10],
            });
        }

        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(out)
    }

    pub fn take_dirty_viewport_rows(&mut self, rows: u16) -> Result<Vec<u16>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_take_dirty_viewport_rows(self.ptr.as_ptr(), rows)
        };
        if bytes.ptr.is_null() || bytes.len == 0 {
            return Ok(Vec::new());
        }
        if bytes.len % 2 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 2);
        for chunk in slice.chunks_exact(2) {
            out.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(out)
    }

    pub fn take_viewport_scroll_delta(&mut self) -> i32 {
        unsafe { ghostty_vt_sys::ghostty_vt_terminal_take_viewport_scroll_delta(self.ptr.as_ptr()) }
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        let mut col: u16 = 0;
        let mut row: u16 = 0;
        let ok = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_cursor_position(
                self.ptr.as_ptr(),
                &mut col as *mut u16,
                &mut row as *mut u16,
            )
        };
        ok.then_some((col, row))
    }

    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<String> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_hyperlink_at(self.ptr.as_ptr(), col, row)
        };
        if bytes.ptr.is_null() || bytes.len == 0 {
            return None;
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Some(s)
    }

    pub fn scroll_viewport(&mut self, delta_lines: i32) -> Result<(), Error> {
        let rc = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_scroll_viewport(self.ptr.as_ptr(), delta_lines)
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }

    pub fn scroll_viewport_top(&mut self) -> Result<(), Error> {
        let rc =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_scroll_viewport_top(self.ptr.as_ptr()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }

    pub fn scroll_viewport_bottom(&mut self) -> Result<(), Error> {
        let rc = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_scroll_viewport_bottom(self.ptr.as_ptr())
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { ghostty_vt_sys::ghostty_vt_terminal_free(self.ptr.as_ptr()) }
    }
}

pub fn terminal_new(cols: u16, rows: u16) -> Result<Terminal, Error> {
    Terminal::new(cols, rows)
}
