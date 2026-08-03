use super::TerminalSession;
use ghostty_vt::{KeyModifiers, Rgb, StyleRun, encode_key_named};
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler,
    EntityInputHandler, FocusHandle, GlobalElementId, IntoElement, KeyBinding, KeyDownEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Render,
    ScrollDelta, ScrollWheelEvent, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle,
    Window, actions, div, fill, hsla, point, prelude::*, px, relative, rgba, size,
};
use std::ops::Range;
use std::sync::Once;
use std::time::{Duration, Instant};

actions!(
    terminal_view,
    [
        Copy,
        Paste,
        SelectAll,
        Tab,
        TabPrev,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        Find,
        PreviousBlock,
        NextBlock,
        CopyLastOutput,
        ToggleQuiet,
        SendBlockToNote,
        SendSessionToNote,
        ToIsland,
        Servers
    ]
);

const KEY_CONTEXT: &str = "Terminal";
static KEY_BINDINGS: Once = Once::new();

fn ensure_key_bindings(cx: &mut App) {
    KEY_BINDINGS.call_once(|| {
        cx.bind_keys([
            KeyBinding::new("tab", Tab, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-tab", TabPrev, Some(KEY_CONTEXT)),
        ]);
    });
}

fn split_viewport_lines(viewport: &str) -> Vec<String> {
    let viewport = viewport.strip_suffix('\n').unwrap_or(viewport);
    if viewport.is_empty() {
        return Vec::new();
    }
    viewport.split('\n').map(|line| line.to_string()).collect()
}

pub(crate) fn should_skip_key_down_for_ime(has_input: bool, keystroke: &gpui::Keystroke) -> bool {
    if !has_input || !keystroke.is_ime_in_progress() {
        return false;
    }

    !matches!(
        keystroke.key.as_str(),
        "enter" | "return" | "kp_enter" | "numpad_enter"
    )
}

pub(crate) fn ctrl_byte_for_keystroke(keystroke: &gpui::Keystroke) -> Option<u8> {
    let candidate = keystroke
        .key_char
        .as_deref()
        .or_else(|| (!keystroke.key.is_empty()).then_some(keystroke.key.as_str()))?;

    if candidate == "space" {
        return Some(0x00);
    }

    let bytes = candidate.as_bytes();
    if bytes.len() != 1 {
        return None;
    }

    let b = bytes[0];
    if (b'@'..=b'_').contains(&b) {
        Some(b & 0x1f)
    } else if b.is_ascii_lowercase() {
        Some(b - b'a' + 1)
    } else if b.is_ascii_uppercase() {
        Some(b - b'A' + 1)
    } else {
        None
    }
}

pub(crate) fn sgr_mouse_button_value(
    base_button: u8,
    motion: bool,
    shift: bool,
    alt: bool,
    control: bool,
) -> u8 {
    let mut value = base_button;
    if motion {
        value = value.saturating_add(32);
    }
    if shift {
        value = value.saturating_add(4);
    }
    if alt {
        value = value.saturating_add(8);
    }
    if control {
        value = value.saturating_add(16);
    }
    value
}

fn window_position_to_local(
    last_bounds: Option<Bounds<Pixels>>,
    position: gpui::Point<gpui::Pixels>,
) -> gpui::Point<gpui::Pixels> {
    let origin = last_bounds
        .map(|bounds| bounds.origin)
        .unwrap_or_else(|| point(px(0.0), px(0.0)));
    point(position.x - origin.x, position.y - origin.y)
}

pub(crate) fn sgr_mouse_sequence(button_value: u8, col: u16, row: u16, pressed: bool) -> String {
    let suffix = if pressed { 'M' } else { 'm' };
    format!("\x1b[<{};{};{}{}", button_value, col, row, suffix)
}

fn is_url_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9')
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'%'
        )
}

//  Con lo que el escritorio tenga puesto. Se suelta y no se espera: si no
//  hay xdg-open, el clic simplemente no hace nada, que es mejor que colgar
//  la terminal esperando a un proceso que no existe.
fn abrir_enlace(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn url_at_byte_index(text: &str, index: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut idx = index.min(bytes.len().saturating_sub(1));

    if !is_url_byte(bytes[idx]) && idx > 0 && is_url_byte(bytes[idx - 1]) {
        idx -= 1;
    }

    if !is_url_byte(bytes[idx]) {
        return None;
    }

    let mut start = idx;
    while start > 0 && is_url_byte(bytes[start - 1]) {
        start -= 1;
    }

    let mut end = idx + 1;
    while end < bytes.len() && is_url_byte(bytes[end]) {
        end += 1;
    }

    while end > start
        && matches!(
            bytes[end - 1],
            b'.' | b',' | b')' | b']' | b'}' | b';' | b':' | b'!' | b'?'
        )
    {
        end -= 1;
    }

    let candidate = std::str::from_utf8(&bytes[start..end]).ok()?;
    if candidate.starts_with("https://") || candidate.starts_with("http://") {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn url_at_column_in_line(line: &str, col: u16) -> Option<String> {
    if line.is_empty() {
        return None;
    }

    let local = byte_index_for_column_in_line(line, col).min(line.len().saturating_sub(1));
    url_at_byte_index(line, local)
}

type TerminalSendFn = dyn Fn(&[u8]) + Send + Sync + 'static;

pub struct TerminalInput {
    send: Box<TerminalSendFn>,
}

impl TerminalInput {
    pub fn new(send: impl Fn(&[u8]) + Send + Sync + 'static) -> Self {
        Self {
            send: Box::new(send),
        }
    }

    pub fn send(&self, bytes: &[u8]) {
        (self.send)(bytes);
    }
}

pub struct TerminalView {
    session: TerminalSession,
    viewport_lines: Vec<String>,
    viewport_line_offsets: Vec<usize>,
    viewport_total_len: usize,
    viewport_style_runs: Vec<Vec<StyleRun>>,
    line_layouts: Vec<Option<gpui::ShapedLine>>,
    line_layout_key: Option<(Pixels, Pixels)>,
    last_bounds: Option<Bounds<Pixels>>,
    focus_handle: FocusHandle,
    last_window_title: Option<String>,
    input: Option<TerminalInput>,
    pending_output: Vec<u8>,
    pending_refresh: bool,
    selection: Option<ByteSelection>,
    marked_text: Option<SharedString>,
    marked_selected_range_utf16: Range<usize>,
    font: gpui::Font,
    on_resize: Option<std::rc::Rc<dyn Fn(u16, u16)>>,
    padding: Pixels,
    //  Sin poner, manda el tamaño del anfitrión. Con él, manda esto — y de
    //  ahí sale también el alto de línea, que si no se queda del tamaño
    //  viejo y las letras se montan.
    font_size: Option<Pixels>,
    font_size_base: Option<Pixels>,
    busqueda: Option<Busqueda>,
    servidores: Option<Servidores>,
    //  Hasta cuándo dura el destello de la campana.
    campana_hasta: Option<std::time::Instant>,
    //  Los mandatos que han pasado por aquí, en coordenadas del historial.
    bloques: Vec<Bloque>,
    tranquilo: bool,
    opacidad: f32,
    radio: f32,
    //  Dónde está pintado el cursor ahora mismo, que no es lo mismo que
    //  dónde está: entre los dos sitios hay una animación.
    cursor_pintado: Option<gpui::Point<Pixels>>,
    cursor_moviendose: bool,
    //  Por dónde acaba de pasar el cursor, para la estela.
    cursor_estela: Vec<CursorTrailGhost>,
    estela_largo: u8,
    //  El depósito de chispa de la barra, seco.
    seco: bool,
}

pub struct Appearance {
    pub font: gpui::Font,
    pub size: Pixels,
    pub padding: Pixels,
    pub opacity: f32,
    pub radius: f32,
    pub trail: u8,
}

//  Un mandato: dónde empezó, dónde acabó y con qué código. Las filas van en
//  coordenadas del historial y no de la pantalla, que es lo único que
//  sobrevive a que la salida siga subiendo.
#[derive(Debug, Clone, Copy)]
pub struct Bloque {
    pub inicio: u32,
    pub fin: Option<u32>,
    pub salida: Option<i32>,
}

//  El selector de servidores: lo mismo que la caja de buscar, pero eligiendo
//  a dónde ir en vez de qué encontrar. Los hosts se piden UNA vez al abrirlo
//  —leer un fichero en cada tecla no tiene ningún sentido— y el filtrado se
//  hace sobre lo que ya está en memoria.
struct Servidores {
    patron: String,
    todos: Vec<crate::Servidor>,
    indice: usize,
    //  Con formulario abierto, la lista se aparta: es la misma ventana
    //  cambiando de cara, no un diálogo encima de otro.
    editando: Option<Formulario>,
}

//  Los campos de un servidor, en el orden en que se rellenan. Los cinco
//  primeros van a `~/.ssh/config` —los entiende ssh y los aprovechan scp, git
//  y todo lo demás—; los dos últimos son nuestros.
const CAMPOS: [(&str, &str); 8] = [
    ("Nombre", "como lo vas a llamar"),
    ("Máquina", "dominio o IP"),
    ("Usuario", "vacío = el tuyo"),
    ("Puerto", "vacío = 22"),
    ("Clave", "ruta de la privada, si no la de siempre"),
    ("Salto", "pasar por otro servidor (ProxyJump)"),
    ("Etiquetas", "separadas por espacios, para buscar"),
    ("Al entrar", "un mandato que se teclea al conectar"),
];

struct Formulario {
    valores: [String; 8],
    indice: usize,
    favorito: bool,
    //  Cómo se llamaba antes, para poder renombrar sin dejar el bloque viejo.
    original: String,
}

impl Formulario {
    fn de(servidor: &crate::Servidor) -> Self {
        Self {
            valores: [
                servidor.alias.clone(),
                servidor.host.clone(),
                servidor.usuario.clone(),
                servidor.puerto.clone(),
                servidor.clave.clone(),
                servidor.salto.clone(),
                servidor.etiquetas.clone(),
                servidor.al_conectar.clone(),
            ],
            indice: 0,
            favorito: servidor.favorito,
            original: if servidor.rapido {
                String::new()
            } else {
                servidor.alias.clone()
            },
        }
    }

    fn servidor(&self) -> crate::Servidor {
        crate::Servidor {
            alias: self.valores[0].trim().to_string(),
            host: self.valores[1].trim().to_string(),
            usuario: self.valores[2].trim().to_string(),
            puerto: self.valores[3].trim().to_string(),
            clave: self.valores[4].trim().to_string(),
            salto: self.valores[5].trim().to_string(),
            etiquetas: self.valores[6].trim().to_string(),
            al_conectar: self.valores[7].trim().to_string(),
            favorito: self.favorito,
            rapido: false,
        }
    }
}

impl Servidores {
    //  Lo que se ve: los guardados que casan, y por delante el destino escrito
    //  al vuelo si lo que hay en la caja parece un sitio y no es ninguno de
    //  ellos. Es lo que uno hace la primera vez, antes de tener nada guardado.
    fn filtrados(&self) -> Vec<crate::Servidor> {
        let q = self.patron.trim().to_lowercase();
        let mut salida: Vec<crate::Servidor> = self
            .todos
            .iter()
            .filter(|s| {
                q.is_empty()
                    || s.alias.to_lowercase().contains(&q)
                    || s.detalle().to_lowercase().contains(&q)
            })
            .cloned()
            .collect();

        if let Some(gestor) = crate::gestor_servidores()
            && let Some(mut destino) = (gestor.al_vuelo)(&self.patron)
        {
            let ya_esta = salida
                .iter()
                .any(|s| s.alias == destino.alias || s.host == destino.host);
            if !ya_esta {
                destino.rapido = true;
                salida.insert(0, destino);
            }
        }

        salida
    }
}

//  Buscar en el historial: el patrón que se teclea, las filas donde aparece
//  —en coordenadas de historial, no de pantalla— y por cuál vamos.
#[derive(Debug, Default)]
struct Busqueda {
    patron: String,
    resultados: Vec<u32>,
    indice: usize,
    //  Con qué patrón se calcularon los resultados, para no rebuscar el
    //  historial entero en cada pulsación.
    calculado: String,
}

#[derive(Clone, Copy, Debug)]
struct ByteSelection {
    anchor: usize,
    active: usize,
}

// A Kitty-style trail is an after-image, not a delayed cursor. Keeping the
// cursor itself snapped to the terminal position avoids making fast typing
// look as if characters have extra spacing.
#[derive(Clone, Copy)]
struct CursorTrailGhost {
    position: gpui::Point<Pixels>,
    started: Instant,
    lifetime: Duration,
}

impl ByteSelection {
    fn range(self) -> Range<usize> {
        if self.anchor <= self.active {
            self.anchor..self.active
        } else {
            self.active..self.anchor
        }
    }
}

impl TerminalView {
    pub fn new(session: TerminalSession, focus_handle: FocusHandle) -> Self {
        Self {
            session,
            viewport_lines: Vec::new(),
            viewport_line_offsets: Vec::new(),
            viewport_total_len: 0,
            viewport_style_runs: Vec::new(),
            line_layouts: Vec::new(),
            line_layout_key: None,
            last_bounds: None,
            focus_handle,
            last_window_title: None,
            input: None,
            pending_output: Vec::new(),
            pending_refresh: false,
            selection: None,
            marked_text: None,
            marked_selected_range_utf16: 0..0,
            font: crate::default_terminal_font(),
            on_resize: None,
            padding: px(0.),
            font_size: None,
            font_size_base: None,
            busqueda: None,
            servidores: None,
            campana_hasta: None,
            bloques: Vec::new(),
            tranquilo: false,
            opacidad: 1.0,
            radio: 0.,
            cursor_pintado: None,
            cursor_moviendose: false,
            cursor_estela: Vec::new(),
            estela_largo: 0,
            seco: false,
        }
        .with_refreshed_viewport()
    }

    fn on_tab(&mut self, _: &Tab, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(false, cx);
    }

    fn on_tab_prev(&mut self, _: &TabPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(true, cx);
    }

    fn send_tab(&mut self, reverse: bool, cx: &mut Context<Self>) {
        if reverse {
            self.send_input_parts(&[b"\x1b[Z"], cx);
        } else {
            self.send_input_parts(&[b"\t"], cx);
        }
    }

    pub fn new_with_input(
        session: TerminalSession,
        focus_handle: FocusHandle,
        input: TerminalInput,
    ) -> Self {
        Self {
            session,
            viewport_lines: Vec::new(),
            viewport_line_offsets: Vec::new(),
            viewport_total_len: 0,
            viewport_style_runs: Vec::new(),
            line_layouts: Vec::new(),
            line_layout_key: None,
            last_bounds: None,
            focus_handle,
            last_window_title: None,
            input: Some(input),
            pending_output: Vec::new(),
            pending_refresh: false,
            selection: None,
            marked_text: None,
            marked_selected_range_utf16: 0..0,
            font: crate::default_terminal_font(),
            on_resize: None,
            padding: px(0.),
            font_size: None,
            font_size_base: None,
            busqueda: None,
            servidores: None,
            campana_hasta: None,
            bloques: Vec::new(),
            tranquilo: false,
            opacidad: 1.0,
            radio: 0.,
            cursor_pintado: None,
            cursor_moviendose: false,
            cursor_estela: Vec::new(),
            estela_largo: 0,
            seco: false,
        }
        .with_refreshed_viewport()
    }

    // El aviso de redimensión: la vista ya ajusta el VT sola al pintarse; el
    // anfitrión registra aquí cómo enterarse para ajustar también su PTY.
    pub fn set_on_resize(&mut self, cb: impl Fn(u16, u16) + 'static) {
        self.on_resize = Some(std::rc::Rc::new(cb));
    }

    pub fn set_font(&mut self, font: gpui::Font) {
        self.font = font;
        self.line_layouts.clear();
        self.line_layout_key = None;
    }

    // Aire alrededor del texto. La rejilla se calcula sobre los bounds del
    // elemento ya acolchado, así que cols/rows se descuentan solos.
    pub fn set_padding(&mut self, padding: Pixels) {
        self.padding = padding;
    }

    //  Cristal: cuánto deja pasar el fondo, y con cuánto radio se recorta la
    //  lámina. Con 1 y 0 se comporta como una ventana opaca de toda la vida.
    //  Cuántos fantasmas deja el cursor detrás. Cero, ninguno.
    pub fn set_estela(&mut self, largo: u8) {
        self.estela_largo = largo.min(24);
        if largo == 0 {
            self.cursor_estela.clear();
            self.cursor_moviendose = false;
        }
    }

    pub fn set_cristal(&mut self, opacidad: f32, radio: f32) {
        self.opacidad = opacidad.clamp(0.2, 1.0);
        self.radio = radio.clamp(0., 40.);
    }

    //  El tamaño de letra de partida. Se recuerda aparte para que «volver al
    //  normal» sepa a dónde volver.
    pub fn set_font_size(&mut self, size: Pixels) {
        self.font_size = Some(size);
        self.font_size_base = Some(size);
        self.invalidate_layout();
    }

    //  Todo lo que se puede cambiar sin reabrir la ventana, de una vez: es lo
    //  que hace que tocar un deslizador en Ajustes se note aquí al momento.
    pub fn set_apariencia(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.set_font(appearance.font);
        self.set_font_size(appearance.size);
        self.set_padding(appearance.padding);
        self.set_cristal(appearance.opacity, appearance.radius);
        self.set_estela(appearance.trail);
        cx.notify();
    }

    fn invalidate_layout(&mut self) {
        self.line_layouts.clear();
        self.line_layout_key = None;
    }

    fn current_font_size(&self, window: &Window) -> Pixels {
        self.font_size
            .unwrap_or_else(|| window.text_style().font_size.to_pixels(window.rem_size()))
    }

    fn on_increase_font_size(
        &mut self,
        _: &IncreaseFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.zoom(1.0, window, cx);
    }

    fn on_decrease_font_size(
        &mut self,
        _: &DecreaseFontSize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.zoom(-1.0, window, cx);
    }

    fn on_reset_font_size(
        &mut self,
        _: &ResetFontSize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(base) = self.font_size_base {
            self.font_size = Some(base);
            self.invalidate_layout();
            cx.notify();
        }
    }

    // ── bloques de mandato ────────────────────────────────────────
    //
    //  Quien lee los marcadores es el anfitrión —los ve pasar en el chorro
    //  del PTY antes que nadie— y aquí solo se apuntan, en coordenadas del
    //  historial, para poder pintarlos donde toque aunque la salida siga
    //  subiendo.

    pub fn empieza_bloque(&mut self, cx: &mut Context<Self>) {
        let Some(fila) = self.fila_absoluta_del_cursor() else {
            return;
        };
        //  Un tope generoso: son doce bytes por bloque y así una sesión de
        //  semanas no se come la memoria por apuntar mandatos.
        if self.bloques.len() > 2000 {
            self.bloques.drain(..500);
        }
        self.bloques.push(Bloque {
            inicio: fila,
            fin: None,
            salida: None,
        });
        cx.notify();
    }

    pub fn acaba_bloque(&mut self, salida: i32, cx: &mut Context<Self>) {
        let fila = self.fila_absoluta_del_cursor();
        if let Some(b) = self.bloques.last_mut()
            && b.fin.is_none()
        {
            b.fin = fila;
            b.salida = Some(salida);
            cx.notify();
        }
    }

    fn fila_absoluta_del_cursor(&self) -> Option<u32> {
        let (arriba, _) = self.session.viewport_position()?;
        let (_, fila) = self.session.cursor_position()?;
        Some(arriba + fila.saturating_sub(1) as u32)
    }

    //  Atenuar todo lo anterior al último mandato. En una sesión larga con un
    //  agente, saber dónde empieza lo nuevo vale más que cualquier color.
    //  Lo que la sesión dice que está pasando; el anfitrión lo usa para decir
    //  quién llama cuando suena la campana.
    pub fn titulo_actual(&self) -> String {
        self.session.title().unwrap_or("k4term").to_string()
    }

    //  Que la barra se ha quedado sin chispa. Se enseña con un punto y no
    //  con un cartel: enterarse sin que te interrumpan es todo lo que se
    //  pide de un aviso así.
    pub fn set_seco(&mut self, valor: bool, cx: &mut Context<Self>) {
        if self.seco != valor {
            self.seco = valor;
            cx.notify();
        }
    }

    pub fn set_tranquilo(&mut self, valor: bool, cx: &mut Context<Self>) {
        self.tranquilo = valor;
        cx.notify();
    }

    fn on_toggle_quiet(&mut self, _: &ToggleQuiet, _window: &mut Window, cx: &mut Context<Self>) {
        self.tranquilo = !self.tranquilo;
        cx.notify();
    }

    //  Saltar de un prompt al anterior o al siguiente: en una sesión larga es
    //  la diferencia entre buscar y encontrar.
    fn on_previous_block(
        &mut self,
        _: &PreviousBlock,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saltar_bloque(true, cx);
    }

    fn on_next_block(&mut self, _: &NextBlock, _window: &mut Window, cx: &mut Context<Self>) {
        self.saltar_bloque(false, cx);
    }

    fn saltar_bloque(&mut self, hacia_atras: bool, cx: &mut Context<Self>) {
        let Some((arriba, _)) = self.session.viewport_position() else {
            return;
        };
        let destino = if hacia_atras {
            self.bloques
                .iter()
                .rev()
                .find(|b| b.inicio < arriba)
                .map(|b| b.inicio)
        } else {
            self.bloques
                .iter()
                .find(|b| b.inicio > arriba)
                .map(|b| b.inicio)
        };
        let Some(destino) = destino else {
            return;
        };

        let _ = self.session.scroll_viewport_top();
        let _ = self.session.scroll_viewport(destino as i32);
        self.sync_viewport_scroll_tracking();
        self.refresh_viewport();
        cx.notify();
    }

    //  El texto del último mandato, del principio al final —o hasta donde
    //  esté el cursor si todavía corre.
    fn texto_del_ultimo_bloque(&self) -> Option<(String, String)> {
        let bloque = self.bloques.last().copied()?;
        let hasta = bloque
            .fin
            .or_else(|| self.fila_absoluta_del_cursor())
            .unwrap_or(bloque.inicio);

        let mut texto = String::new();
        for fila in bloque.inicio..=hasta {
            if let Some(linea) = self.session.dump_screen_row(fila) {
                texto.push_str(linea.trim_end());
                texto.push('\n');
            }
        }
        let texto = texto.trim_end().to_string();
        if texto.is_empty() {
            return None;
        }
        //  La primera línea es el prompt con el mandato: sirve de título.
        let titulo = texto.lines().next().unwrap_or("mandato").trim().to_string();
        Some((titulo, texto))
    }

    //  Toda la sesión, del principio del historial a donde estemos.
    fn texto_de_la_sesion(&self) -> Option<String> {
        let (_, total) = self.session.viewport_position()?;
        let mut texto = String::new();
        for fila in 0..total {
            if let Some(linea) = self.session.dump_screen_row(fila) {
                texto.push_str(linea.trim_end());
                texto.push('\n');
            }
        }
        let texto = texto.trim().to_string();
        (!texto.is_empty()).then_some(texto)
    }

    //  Copiar la salida del último mandato: la que se pega en el chat cuando
    //  algo ha fallado, que es el 90 % de los copiados de una terminal.
    fn on_copy_last_output(
        &mut self,
        _: &CopyLastOutput,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, texto)) = self.texto_del_ultimo_bloque() else {
            return;
        };
        let item = ClipboardItem::new_string(texto);
        cx.write_to_clipboard(item.clone());
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        cx.write_to_primary(item);
    }

    //  A la nota del día de Edinot — **si lo tienes**. Quien no lo tenga no
    //  se entera de que esta puerta existe: la tecla no hace nada y no se le
    //  da la lata con un error por algo que nunca pidió.
    //
    //  Va en su propio hilo porque levantar el servidor cuesta un segundo
    //  largo, y una terminal que se congela al pulsar una tecla no la quiere
    //  nadie.
    fn on_send_block_to_note(
        &mut self,
        _: &SendBlockToNote,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if !crate::edinot_disponible() {
            return;
        }
        let Some((titulo, texto)) = self.texto_del_ultimo_bloque() else {
            return;
        };
        crate::anotar_en_segundo_plano(titulo, texto);
    }

    //  Devolver la sesión a la isla: se le pasa a la barra con qué repintarla
    //  y ella se encarga del resto. Quien de verdad cambia de manos es el
    //  descriptor del PTY, y de eso sabe el anfitrión, no la vista.
    fn on_to_island(&mut self, _: &ToIsland, _window: &mut Window, _cx: &mut Context<Self>) {
        self.a_la_isla();
    }

    //  Lo mismo, pero llamable desde fuera de una tecla: la barra toca el
    //  timbre de la ventana cuando el gesto se hace desde el compositor, y
    //  entonces no hay ninguna acción de por medio.
    pub fn a_la_isla(&mut self) {
        if !crate::mudanza_disponible() {
            return;
        }
        crate::mudar(self.session.pintura(), self.titulo_actual());
    }

    fn on_send_session_to_note(
        &mut self,
        _: &SendSessionToNote,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if !crate::edinot_disponible() {
            return;
        }
        let Some(texto) = self.texto_de_la_sesion() else {
            return;
        };
        let titulo = format!("Sesión de terminal · {}", self.titulo_actual());
        crate::anotar_en_segundo_plano(titulo, texto);
    }

    //  La campana, vista: un destello de 120 ms sobre la pantalla. Devuelve
    //  si sigue encendida, que es lo que el anfitrión necesita para saber si
    //  tiene que volver a pedir un pintado.
    pub fn tocar_campana(&mut self, cx: &mut Context<Self>) {
        self.campana_hasta =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(120));
        cx.notify();
    }

    //  ¿Hay algo animándose que pida otro fotograma? El anfitrión lo consulta
    //  en su bucle: sin esto, el cursor se quedaría a medio camino hasta que
    //  llegara salida nueva.
    pub fn animando(&self) -> bool {
        self.cursor_moviendose || !self.cursor_estela.is_empty()
    }

    pub fn campana_encendida(&mut self, cx: &mut Context<Self>) -> bool {
        match self.campana_hasta {
            Some(hasta) if std::time::Instant::now() < hasta => true,
            Some(_) => {
                self.campana_hasta = None;
                cx.notify();
                false
            }
            None => false,
        }
    }

    // ── buscar en el historial ────────────────────────────────────

    fn on_find(&mut self, _: &Find, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busqueda.is_none() {
            self.busqueda = Some(Busqueda::default());
            cx.notify();
        }
    }

    //  ── el selector de servidores ─────────────────────────────
    //
    //  En la isla esto es un plugin de la barra; aquí hace falta lo mismo sin
    //  salir de la ventana, porque cuando estás trabajando en ella lo último
    //  que quieres es irte a mirar a otro sitio. La lista sale de los mismos
    //  ficheros: cada frontal la lee con sus ojos, pero la verdad es una.
    fn on_servers(&mut self, _: &Servers, _window: &mut Window, cx: &mut Context<Self>) {
        if self.servidores.is_some() {
            self.servidores = None;
            cx.notify();
            return;
        }
        let todos = crate::gestor_servidores()
            .map(|g| (g.listar)())
            .unwrap_or_default();
        self.servidores = Some(Servidores {
            patron: String::new(),
            todos,
            indice: 0,
            editando: None,
        });
        cx.notify();
    }

    fn tecla_de_servidores(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        if self
            .servidores
            .as_ref()
            .is_some_and(|s| s.editando.is_some())
        {
            self.tecla_de_formulario(keystroke, cx);
            return;
        }

        match keystroke.key.as_str() {
            "escape" => {
                self.servidores = None;
                cx.notify();
                return;
            }
            "enter" => {
                self.conectar_al_elegido(cx);
                return;
            }
            "up" | "down" => {
                if let Some(s) = self.servidores.as_mut() {
                    let cuantos = s.filtrados().len();
                    if cuantos > 0 {
                        let arriba = keystroke.key == "up";
                        s.indice = if arriba {
                            s.indice.saturating_sub(1)
                        } else {
                            (s.indice + 1).min(cuantos - 1)
                        };
                    }
                }
                cx.notify();
                return;
            }
            "backspace" => {
                if let Some(s) = self.servidores.as_mut() {
                    s.patron.pop();
                    s.indice = 0;
                }
                cx.notify();
                return;
            }
            "delete" => {
                self.borrar_elegido(cx);
                return;
            }
            "s" | "e" if keystroke.modifiers.control => {
                self.editar_elegido(cx);
                return;
            }
            "f" if keystroke.modifiers.control => {
                self.favorito_elegido(cx);
                return;
            }
            _ => {}
        }

        if keystroke.modifiers.control || keystroke.modifiers.alt {
            return;
        }
        if let Some(texto) = keystroke.key_char.as_ref()
            && let Some(s) = self.servidores.as_mut()
        {
            s.patron.push_str(texto);
            s.indice = 0;
            cx.notify();
        }
    }

    //  Conectar aquí es TECLEAR el mandato en la sesión que tienes delante.
    //  No abrir otra ventana ni sustituir nada: estás en un prompt y lo que
    //  quieres es entrar, igual que si lo hubieras escrito tú.
    fn conectar_al_elegido(&mut self, cx: &mut Context<Self>) {
        let Some(elegido) = self.elegido() else {
            return;
        };

        //  Un guardado se llama por su alias y ya está: lo demás lo pone el
        //  propio ssh leyendo su configuración. Solo el destino al vuelo lleva
        //  el usuario y el puerto a cuestas.
        let mandato = if elegido.rapido {
            let usuario = if elegido.usuario.is_empty() {
                String::new()
            } else {
                format!("{}@", elegido.usuario)
            };
            let puerto = if elegido.puerto.is_empty() {
                String::new()
            } else {
                format!("-p {} ", elegido.puerto)
            };
            format!("ssh {puerto}{usuario}{}", elegido.host)
        } else {
            format!("ssh {}", elegido.alias)
        };

        if !elegido.rapido
            && let Some(gestor) = crate::gestor_servidores()
        {
            (gestor.visitar)(&elegido.alias);
        }

        self.servidores = None;
        self.send_input_parts(&[format!("{mandato}\r").as_bytes()], cx);
        cx.notify();
    }

    fn elegido(&self) -> Option<crate::Servidor> {
        self.servidores
            .as_ref()
            .and_then(|s| s.filtrados().get(s.indice).cloned())
    }

    //  Guardar el destino que acabas de escribir, marcar favorito y borrar:
    //  lo que faltaba para que esto no fuera media pieza. Después de tocar el
    //  fichero se vuelve a pedir la lista, que es la única forma de que lo
    //  que se ve sea lo que hay.
    //  Guardar no es escribirlo y ya: se abre el formulario con lo que se
    //  sabe y se completa el resto, que es lo que uno quiere hacer justo
    //  después. Y editar es lo mismo con un host que ya existe.
    fn editar_elegido(&mut self, cx: &mut Context<Self>) {
        let Some(elegido) = self.elegido() else {
            return;
        };
        if let Some(s) = self.servidores.as_mut() {
            s.editando = Some(Formulario::de(&elegido));
        }
        cx.notify();
    }

    fn tecla_de_formulario(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        match keystroke.key.as_str() {
            "escape" => {
                if let Some(s) = self.servidores.as_mut() {
                    s.editando = None;
                }
                cx.notify();
                return;
            }
            "enter" => {
                self.guardar_formulario(cx);
                return;
            }
            "up" | "down" | "tab" => {
                if let Some(f) = self.servidores.as_mut().and_then(|s| s.editando.as_mut()) {
                    let atras = keystroke.key == "up"
                        || (keystroke.key == "tab" && keystroke.modifiers.shift);
                    f.indice = if atras {
                        f.indice.saturating_sub(1)
                    } else {
                        (f.indice + 1).min(CAMPOS.len() - 1)
                    };
                }
                cx.notify();
                return;
            }
            "backspace" => {
                if let Some(f) = self.servidores.as_mut().and_then(|s| s.editando.as_mut()) {
                    let i = f.indice;
                    f.valores[i].pop();
                }
                cx.notify();
                return;
            }
            //  Favorito es parte de cómo quieres el servidor, no una acción
            //  aparte: se marca aquí mismo.
            "f" if keystroke.modifiers.control => {
                if let Some(f) = self.servidores.as_mut().and_then(|s| s.editando.as_mut()) {
                    f.favorito = !f.favorito;
                }
                cx.notify();
                return;
            }
            _ => {}
        }

        if keystroke.modifiers.control || keystroke.modifiers.alt {
            return;
        }
        if let Some(texto) = keystroke.key_char.as_ref()
            && let Some(f) = self.servidores.as_mut().and_then(|s| s.editando.as_mut())
        {
            let i = f.indice;
            f.valores[i].push_str(texto);
            cx.notify();
        }
    }

    fn guardar_formulario(&mut self, cx: &mut Context<Self>) {
        let Some(gestor) = crate::gestor_servidores() else {
            return;
        };
        let (nuevo, original) = {
            let Some(f) = self.servidores.as_ref().and_then(|s| s.editando.as_ref()) else {
                return;
            };
            (f.servidor(), f.original.clone())
        };

        //  Sin nombre ni máquina no hay nada que guardar, y decírselo a gritos
        //  sobraría: se queda donde está hasta que los rellene.
        if nuevo.alias.is_empty() || nuevo.host.is_empty() {
            return;
        }

        //  Renombrar es guardar el nuevo y quitar el viejo, en ese orden: si
        //  fallara lo primero, no se habría perdido nada.
        (gestor.guardar)(&nuevo);
        if !original.is_empty() && original != nuevo.alias {
            (gestor.borrar)(&original);
        }

        if let Some(s) = self.servidores.as_mut() {
            s.editando = None;
        }
        self.refrescar_servidores(cx);
    }

    fn favorito_elegido(&mut self, cx: &mut Context<Self>) {
        let (Some(elegido), Some(gestor)) = (self.elegido(), crate::gestor_servidores()) else {
            return;
        };
        if elegido.rapido {
            return;
        }
        (gestor.favorito)(&elegido.alias);
        self.refrescar_servidores(cx);
    }

    fn borrar_elegido(&mut self, cx: &mut Context<Self>) {
        let (Some(elegido), Some(gestor)) = (self.elegido(), crate::gestor_servidores()) else {
            return;
        };
        if elegido.rapido {
            return;
        }
        (gestor.borrar)(&elegido.alias);
        self.refrescar_servidores(cx);
    }

    fn refrescar_servidores(&mut self, cx: &mut Context<Self>) {
        if let Some(gestor) = crate::gestor_servidores()
            && let Some(s) = self.servidores.as_mut()
        {
            s.todos = (gestor.listar)();
            s.patron.clear();
            s.indice = 0;
        }
        cx.notify();
    }

    fn cerrar_busqueda(&mut self, cx: &mut Context<Self>) {
        self.busqueda = None;
        cx.notify();
    }

    fn tecla_de_busqueda(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        match keystroke.key.as_str() {
            "escape" => {
                self.cerrar_busqueda(cx);
                return;
            }
            "enter" => {
                self.saltar(keystroke.modifiers.shift, cx);
                return;
            }
            "backspace" => {
                if let Some(b) = self.busqueda.as_mut() {
                    b.patron.pop();
                }
                cx.notify();
                return;
            }
            _ => {}
        }

        //  Solo texto: los atajos con control mientras se busca no significan
        //  nada aquí y no tienen por qué colarse en el patrón.
        if keystroke.modifiers.control || keystroke.modifiers.alt {
            return;
        }
        if let Some(texto) = keystroke.key_char.as_ref() {
            if let Some(b) = self.busqueda.as_mut() {
                b.patron.push_str(texto);
            }
            cx.notify();
        }
    }

    //  Recorre el historial de arriba abajo apuntando en qué filas está el
    //  patrón. Se hace al saltar y no al teclear: rebuscar miles de líneas en
    //  cada letra se nota, y buscar es algo que se pide, no que se sufre.
    fn recalcular_busqueda(&mut self) {
        let Some(b) = self.busqueda.as_mut() else {
            return;
        };
        if b.calculado == b.patron {
            return;
        }

        let patron = b.patron.to_lowercase();
        b.calculado = b.patron.clone();
        b.resultados.clear();
        b.indice = 0;
        if patron.is_empty() {
            return;
        }

        let mut resultados = Vec::new();
        let mut fila = 0u32;
        while let Some(texto) = self.session.dump_screen_row(fila) {
            if texto.to_lowercase().contains(&patron) {
                resultados.push(fila);
            }
            fila += 1;
        }
        if let Some(b) = self.busqueda.as_mut() {
            b.resultados = resultados;
        }
    }

    fn saltar(&mut self, hacia_atras: bool, cx: &mut Context<Self>) {
        self.recalcular_busqueda();

        let destino = {
            let Some(b) = self.busqueda.as_mut() else {
                return;
            };
            if b.resultados.is_empty() {
                cx.notify();
                return;
            }
            //  La primera vez se queda en el primero; a partir de ahí se va
            //  moviendo, y da la vuelta al llegar al final.
            if b.calculado == b.patron && b.indice < b.resultados.len() {
                let n = b.resultados.len();
                b.indice = if hacia_atras {
                    (b.indice + n - 1) % n
                } else {
                    (b.indice + 1) % n
                };
            }
            b.resultados[b.indice]
        };

        //  Al principio de todo y luego bajar: es la única forma de plantarse
        //  en una fila concreta con lo que la API da hoy, y sale exacta.
        let _ = self.session.scroll_viewport_top();
        let _ = self.session.scroll_viewport(destino as i32);
        self.sync_viewport_scroll_tracking();
        self.refresh_viewport();
        cx.notify();
    }

    //  De punto en punto y con topes: por debajo de seis no se lee y por
    //  encima de setenta y dos una rejilla de terminal deja de tener sentido.
    fn zoom(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        let actual = f32::from(self.current_font_size(window));
        let nuevo = (actual + delta).clamp(6.0, 72.0);
        self.font_size = Some(px(nuevo));
        self.invalidate_layout();
        cx.notify();
    }

    // Cambiar el ambiente en marcha. Hay que rehacer el viewport: las celdas
    // que heredan el color por defecto lo llevan ya resuelto dentro.
    pub fn set_default_colors(
        &mut self,
        fg: ghostty_vt::Rgb,
        bg: ghostty_vt::Rgb,
        cx: &mut Context<Self>,
    ) {
        self.session.set_default_colors(fg, bg);
        self.refresh_viewport();
        cx.notify();
    }

    fn utf16_len(s: &str) -> usize {
        s.chars().map(|ch| ch.len_utf16()).sum()
    }

    fn utf16_range_to_utf8(s: &str, range_utf16: Range<usize>) -> Option<Range<usize>> {
        let mut utf16_count = 0usize;
        let mut start_utf8: Option<usize> = None;
        let mut end_utf8: Option<usize> = None;

        if range_utf16.start == 0 {
            start_utf8 = Some(0);
        }
        if range_utf16.end == 0 {
            end_utf8 = Some(0);
        }

        for (utf8_index, ch) in s.char_indices() {
            if start_utf8.is_none() && utf16_count >= range_utf16.start {
                start_utf8 = Some(utf8_index);
            }
            if end_utf8.is_none() && utf16_count >= range_utf16.end {
                end_utf8 = Some(utf8_index);
            }

            utf16_count = utf16_count.saturating_add(ch.len_utf16());
        }

        if start_utf8.is_none() && utf16_count >= range_utf16.start {
            start_utf8 = Some(s.len());
        }
        if end_utf8.is_none() && utf16_count >= range_utf16.end {
            end_utf8 = Some(s.len());
        }

        Some(start_utf8?..end_utf8?)
    }

    fn cell_offset_for_utf16(text: &str, utf16_offset: usize) -> usize {
        use unicode_width::UnicodeWidthChar as _;

        let mut cells = 0usize;
        let mut utf16_count = 0usize;
        for ch in text.chars() {
            if utf16_count >= utf16_offset {
                break;
            }

            let len_utf16 = ch.len_utf16();
            if utf16_count.saturating_add(len_utf16) > utf16_offset {
                break;
            }
            utf16_count = utf16_count.saturating_add(len_utf16);

            let width = ch.width().unwrap_or(0);
            if width > 0 {
                cells = cells.saturating_add(width);
            }
        }
        cells
    }

    fn clear_marked_text(&mut self, cx: &mut Context<Self>) {
        self.marked_text = None;
        self.marked_selected_range_utf16 = 0..0;
        cx.notify();
    }

    fn set_marked_text(
        &mut self,
        text: String,
        selected_range_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            self.clear_marked_text(cx);
            return;
        }

        let total_utf16 = Self::utf16_len(&text);
        let selected = selected_range_utf16.unwrap_or(total_utf16..total_utf16);
        let selected = selected.start.min(total_utf16)..selected.end.min(total_utf16);

        self.marked_text = Some(SharedString::from(text));
        self.marked_selected_range_utf16 = selected;
        cx.notify();
    }

    fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }

        self.send_input_parts(&[text.as_bytes()], cx);
    }

    fn send_input_parts(&mut self, parts: &[&[u8]], cx: &mut Context<Self>) {
        if parts.is_empty() {
            return;
        }

        if let Some(input) = self.input.as_ref() {
            for bytes in parts {
                input.send(bytes);
            }
            return;
        }

        for bytes in parts {
            let _ = self.session.feed(bytes);
        }
        self.apply_side_effects(cx);
        self.schedule_viewport_refresh(cx);
    }

    fn feed_output_bytes_to_session(&mut self, bytes: &[u8]) {
        if let Some(input) = self.input.as_ref() {
            let _ = self
                .session
                .feed_with_pty_responses(bytes, |resp| input.send(resp));
        } else {
            let _ = self.session.feed(bytes);
        }
    }

    fn sync_viewport_scroll_tracking(&mut self) {
        let _ = self.session.take_viewport_scroll_delta();
    }

    fn apply_viewport_scroll_delta(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }

        let rows = self.session.rows() as usize;
        if rows == 0 {
            return;
        }

        if self.viewport_lines.len() != rows || self.viewport_style_runs.len() != rows {
            self.refresh_viewport();
            return;
        }

        let delta_abs: usize = delta.unsigned_abs() as usize;
        if delta_abs == 0 {
            return;
        }
        if delta_abs >= rows {
            self.refresh_viewport();
            return;
        }

        let has_layouts = self.line_layouts.len() == rows;

        if delta > 0 {
            self.viewport_lines.rotate_left(delta_abs);
            self.viewport_style_runs.rotate_left(delta_abs);
            if has_layouts {
                self.line_layouts.rotate_left(delta_abs);
            }

            for idx in rows - delta_abs..rows {
                self.viewport_lines[idx].clear();
                self.viewport_style_runs[idx].clear();
                if has_layouts {
                    self.line_layouts[idx] = None;
                }
            }

            let dirty_rows: Vec<u16> = (rows - delta_abs..rows).map(|row| row as u16).collect();
            let _ = self.apply_dirty_viewport_rows(&dirty_rows);
            return;
        }

        self.viewport_lines.rotate_right(delta_abs);
        self.viewport_style_runs.rotate_right(delta_abs);
        if has_layouts {
            self.line_layouts.rotate_right(delta_abs);
        }

        for idx in 0..delta_abs {
            self.viewport_lines[idx].clear();
            self.viewport_style_runs[idx].clear();
            if has_layouts {
                self.line_layouts[idx] = None;
            }
        }

        let dirty_rows: Vec<u16> = (0..delta_abs).map(|row| row as u16).collect();
        let _ = self.apply_dirty_viewport_rows(&dirty_rows);
    }

    fn reconcile_dirty_viewport_after_output(&mut self) {
        let delta = self.session.take_viewport_scroll_delta();
        self.apply_viewport_scroll_delta(delta);

        let dirty = self.session.take_dirty_viewport_rows();
        if !dirty.is_empty() && !self.apply_dirty_viewport_rows(&dirty) {
            self.pending_refresh = true;
        }
    }

    fn with_refreshed_viewport(mut self) -> Self {
        self.refresh_viewport();
        self
    }

    fn refresh_viewport(&mut self) {
        let viewport = self.session.dump_viewport().unwrap_or_default();
        self.viewport_lines = split_viewport_lines(&viewport);
        self.viewport_line_offsets = Self::compute_viewport_line_offsets(&self.viewport_lines);
        self.viewport_total_len = Self::compute_viewport_total_len(&self.viewport_lines);
        self.viewport_style_runs = (0..self.session.rows())
            .map(|row| {
                self.session
                    .dump_viewport_row_style_runs(row)
                    .unwrap_or_default()
            })
            .collect();
        self.line_layouts.clear();
        self.line_layout_key = None;
        self.selection = None;
    }

    fn compute_viewport_line_offsets(lines: &[String]) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(lines.len());
        let mut offset = 0usize;
        for line in lines {
            offsets.push(offset);
            offset = offset.saturating_add(line.len() + 1);
        }
        offsets
    }

    fn compute_viewport_total_len(lines: &[String]) -> usize {
        lines
            .iter()
            .fold(0usize, |acc, line| acc.saturating_add(line.len() + 1))
    }

    fn viewport_slice(&self, range: Range<usize>) -> String {
        if range.is_empty() || self.viewport_lines.is_empty() {
            return String::new();
        }

        let start = range.start.min(self.viewport_total_len);
        let end = range.end.min(self.viewport_total_len);
        if start >= end {
            return String::new();
        }

        let mut out = String::new();
        let mut i = 0usize;
        while i < self.viewport_lines.len() {
            let line_start = *self.viewport_line_offsets.get(i).unwrap_or(&0);
            let line = &self.viewport_lines[i];
            let line_end = line_start.saturating_add(line.len());
            let newline_pos = line_end;

            let seg_start = start.max(line_start);
            let seg_end = end.min(newline_pos.saturating_add(1));
            if seg_start < seg_end {
                let local_start = seg_start.saturating_sub(line_start);
                let local_end = seg_end.saturating_sub(line_start);
                let local_end = local_end.min(line.len().saturating_add(1));

                if local_start < line.len() {
                    let text_end = local_end.min(line.len());
                    if let Some(seg) = line.get(local_start..text_end) {
                        out.push_str(seg);
                    }
                }
                if local_end > line.len() {
                    out.push('\n');
                }
            }

            i += 1;
        }

        out
    }

    fn url_at_viewport_index(&self, index: usize) -> Option<String> {
        if self.viewport_lines.is_empty() {
            return None;
        }

        let idx = index.min(self.viewport_total_len.saturating_sub(1));
        let row = self
            .viewport_line_offsets
            .iter()
            .enumerate()
            .rfind(|(_, offset)| **offset <= idx)
            .map(|(i, _)| i)?;

        let line = self.viewport_lines.get(row)?.as_str();
        let line_start = *self.viewport_line_offsets.get(row).unwrap_or(&0);
        let local = idx
            .saturating_sub(line_start)
            .min(line.len().saturating_sub(1));
        url_at_byte_index(line, local)
    }

    fn apply_dirty_viewport_rows(&mut self, dirty_rows: &[u16]) -> bool {
        if dirty_rows.is_empty() {
            return false;
        }

        let expected_rows = self.session.rows() as usize;
        if self.viewport_lines.len() != expected_rows {
            self.refresh_viewport();
            return true;
        }
        if self.viewport_style_runs.len() != expected_rows {
            self.refresh_viewport();
            return true;
        }

        for &row in dirty_rows {
            let row = row as usize;
            if row >= self.viewport_lines.len() {
                continue;
            }

            let line = match self.session.dump_viewport_row(row as u16) {
                Ok(s) => s,
                Err(_) => {
                    self.refresh_viewport();
                    return true;
                }
            };

            let line = line.strip_suffix('\n').unwrap_or(line.as_str());
            self.viewport_lines[row].clear();
            self.viewport_lines[row].push_str(line);
            self.viewport_style_runs[row] = self
                .session
                .dump_viewport_row_style_runs(row as u16)
                .unwrap_or_default();
            if row < self.line_layouts.len() {
                self.line_layouts[row] = None;
            }
        }

        self.viewport_line_offsets = Self::compute_viewport_line_offsets(&self.viewport_lines);
        self.viewport_total_len = Self::compute_viewport_total_len(&self.viewport_lines);
        self.selection = None;
        true
    }

    fn schedule_viewport_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_refresh = true;
        cx.notify();
    }

    fn apply_side_effects(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.session.take_clipboard_write() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn feed_output_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.feed_output_bytes_to_session(bytes);
        self.refresh_viewport();
        self.apply_side_effects(cx);
        cx.notify();
    }

    pub fn queue_output_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        const MAX_PENDING_OUTPUT_BYTES: usize = 256 * 1024;

        if self.pending_output.len().saturating_add(bytes.len()) <= MAX_PENDING_OUTPUT_BYTES {
            self.pending_output.extend_from_slice(bytes);
            cx.notify();
            return;
        }

        if !self.pending_output.is_empty() {
            let pending = std::mem::take(&mut self.pending_output);
            self.feed_output_bytes_to_session(&pending);
            self.apply_side_effects(cx);
            self.reconcile_dirty_viewport_after_output();
        }

        if bytes.len() > MAX_PENDING_OUTPUT_BYTES {
            let mut offset = 0usize;
            while offset < bytes.len() {
                let end = (offset + MAX_PENDING_OUTPUT_BYTES).min(bytes.len());
                self.feed_output_bytes_to_session(&bytes[offset..end]);
                offset = end;
            }
            self.apply_side_effects(cx);
            self.reconcile_dirty_viewport_after_output();
            cx.notify();
            return;
        }

        self.pending_output.extend_from_slice(bytes);
        cx.notify();
    }

    pub fn resize_terminal(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let _ = self.session.resize(cols, rows);
        self.sync_viewport_scroll_tracking();
        self.pending_refresh = true;
        cx.notify();
    }

    fn on_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };

        //  Los corchetes y el salto de línea los pone la misma pieza que en la
        //  isla, que pegar es de las cosas que no pueden diferir entre las dos.
        let payload = self.session.paste_payload(&text);
        self.send_input_parts(&[payload.as_slice()], cx);
    }

    fn on_copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let selection = self
            .selection
            .map(|s| s.range())
            .filter(|range| !range.is_empty())
            .map(|range| self.viewport_slice(range))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.viewport_slice(0..self.viewport_total_len));

        let item = ClipboardItem::new_string(selection.to_string());
        cx.write_to_clipboard(item.clone());
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        cx.write_to_primary(item);
    }

    fn on_select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = Some(ByteSelection {
            anchor: 0,
            active: self.viewport_total_len,
        });
        self.on_copy(&Copy, window, cx);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);

        if event.first_mouse {
            return;
        }

        //  Ctrl+clic abre el enlace, que es lo que espera todo el mundo. Antes
        //  era Súper+clic y copiaba: dos decisiones raras juntas, y en
        //  Hyprland la tecla Súper la tiene el compositor.
        if event.button == MouseButton::Left && event.modifiers.control {
            if let Some((col, row)) = self.mouse_position_to_cell(event.position, window) {
                if let Some(link) = self.session.hyperlink_at(col, row) {
                    abrir_enlace(&link);
                    return;
                }

                if let Some(line) = self.viewport_lines.get(row.saturating_sub(1) as usize)
                    && let Some(url) = url_at_column_in_line(line, col)
                {
                    abrir_enlace(&url);
                    return;
                }
            }

            if let Some(index) = self.mouse_position_to_viewport_index(event.position, window)
                && let Some(url) = self.url_at_viewport_index(index)
            {
                abrir_enlace(&url);
                return;
            }
        }

        if event.modifiers.shift
            || self.input.is_none()
            || !self.session.mouse_reporting_enabled()
            || !self.session.mouse_sgr_enabled()
        {
            if event.button == MouseButton::Left
                && let Some(index) = self.mouse_position_to_viewport_index(event.position, window)
            {
                self.selection = Some(ByteSelection {
                    anchor: index,
                    active: index,
                });
                cx.notify();
            }
            return;
        }

        let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
            return;
        };

        if let Some(input) = self.input.as_ref() {
            let base_button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };

            let button_value = sgr_mouse_button_value(
                base_button,
                false,
                false,
                event.modifiers.alt,
                event.modifiers.control,
            );
            let seq = sgr_mouse_sequence(button_value, col, row, true);
            input.send(seq.as_bytes());
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.shift
            || self.input.is_none()
            || !self.session.mouse_reporting_enabled()
            || !self.session.mouse_sgr_enabled()
        {
            if let Some(selection) = self.selection {
                if selection.range().is_empty() {
                    self.selection = None;
                }
                cx.notify();
            }
            return;
        }

        let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
            return;
        };

        if let Some(input) = self.input.as_ref() {
            let base_button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };

            let button_value = sgr_mouse_button_value(
                base_button,
                false,
                false,
                event.modifiers.alt,
                event.modifiers.control,
            );
            let seq = sgr_mouse_sequence(button_value, col, row, false);
            input.send(seq.as_bytes());
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.shift
            && self.input.is_some()
            && self.session.mouse_reporting_enabled()
            && self.session.mouse_sgr_enabled()
        {
            let send_motion = if self.session.mouse_any_event_enabled() {
                true
            } else if self.session.mouse_button_event_enabled() {
                event.pressed_button.is_some()
            } else {
                false
            };

            if send_motion {
                let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
                    return;
                };

                let base_button = match event.pressed_button {
                    Some(MouseButton::Left) => 0,
                    Some(MouseButton::Middle) => 1,
                    Some(MouseButton::Right) => 2,
                    Some(_) => 3,
                    None => 3,
                };

                let button_value = sgr_mouse_button_value(
                    base_button,
                    true,
                    false,
                    event.modifiers.alt,
                    event.modifiers.control,
                );
                if let Some(input) = self.input.as_ref() {
                    let seq = sgr_mouse_sequence(button_value, col, row, true);
                    input.send(seq.as_bytes());
                }
                return;
            }
        }

        if !event.dragging() {
            return;
        }

        if self.selection.is_none() {
            return;
        }

        let Some(index) = self.mouse_position_to_viewport_index(event.position, window) else {
            return;
        };

        if let Some(selection) = self.selection.as_mut()
            && selection.active != index
        {
            selection.active = index;
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let raw_keystroke = event.keystroke.clone();
        if should_skip_key_down_for_ime(self.input.is_some(), &raw_keystroke) {
            return;
        }
        let keystroke = raw_keystroke.with_simulated_ime();

        if std::env::var("K4TERM_TRAZA_TECLAS").is_ok() {
            eprintln!(
                "tecla={:?} ctrl={} shift={} alt={} char={:?}",
                keystroke.key,
                keystroke.modifiers.control,
                keystroke.modifiers.shift,
                keystroke.modifiers.alt,
                keystroke.key_char
            );
        }

        //  Con la búsqueda abierta, el teclado es suyo: lo que se escriba va
        //  al patrón y no a la shell, que si no se buscaría a ciegas mientras
        //  se le teclea a un programa por detrás.
        if self.servidores.is_some() {
            self.tecla_de_servidores(&keystroke, cx);
            return;
        }
        if self.busqueda.is_some() {
            self.tecla_de_busqueda(&keystroke, cx);
            cx.stop_propagation();
            return;
        }

        if keystroke.modifiers.platform || keystroke.modifiers.function {
            return;
        }

        let scroll_step = (self.session.rows() as i32 / 2).max(1);

        if let Some(input) = self.input.as_ref() {
            if keystroke.modifiers.shift {
                match keystroke.key.as_str() {
                    "home" => {
                        let _ = self.session.scroll_viewport_top();
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "end" => {
                        let _ = self.session.scroll_viewport_bottom();
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "pageup" | "page_up" | "page-up" => {
                        let _ = self.session.scroll_viewport(-scroll_step);
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "pagedown" | "page_down" | "page-down" => {
                        let _ = self.session.scroll_viewport(scroll_step);
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    _ => {}
                }
            }

            if keystroke.modifiers.control
                && let Some(b) = ctrl_byte_for_keystroke(&keystroke)
            {
                input.send(&[b]);
                return;
            }

            if keystroke.modifiers.alt
                && let Some(text) = keystroke.key_char.as_deref()
            {
                input.send(&[0x1b]);
                input.send(text.as_bytes());
                return;
            }

            let modifiers = KeyModifiers {
                shift: keystroke.modifiers.shift,
                control: keystroke.modifiers.control,
                alt: keystroke.modifiers.alt,
                super_key: false,
            };
            if let Some(encoded) = encode_key_named(&keystroke.key, modifiers) {
                input.send(&encoded);
                return;
            }
            return;
        }

        match keystroke.key.as_str() {
            "home" => {
                let _ = self.session.scroll_viewport_top();
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "end" => {
                let _ = self.session.scroll_viewport_bottom();
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "pageup" | "page_up" | "page-up" => {
                let _ = self.session.scroll_viewport(-scroll_step);
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "pagedown" | "page_down" | "page-down" => {
                let _ = self.session.scroll_viewport(scroll_step);
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            _ => {}
        }

        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            super_key: false,
        };
        if let Some(encoded) = encode_key_named(&keystroke.key, modifiers) {
            let _ = self.session.feed(&encoded);
            self.apply_side_effects(cx);
            self.schedule_viewport_refresh(cx);
            return;
        }

        if keystroke.key == "backspace" {
            if let Some(input) = self.input.as_ref() {
                input.send(&[0x7f]);
                return;
            }
            let _ = self.session.feed(&[0x08]);
            self.apply_side_effects(cx);
            self.schedule_viewport_refresh(cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dy_lines: f32 = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / 16.0,
        };

        let delta_lines = (-dy_lines).round() as i32;
        if delta_lines == 0 {
            return;
        }

        if let Some(input) = self.input.as_ref()
            && !event.modifiers.shift
            && self.session.mouse_reporting_enabled()
            && self.session.mouse_sgr_enabled()
        {
            let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
                return;
            };

            let button = if delta_lines < 0 { 64 } else { 65 };
            let button_value = sgr_mouse_button_value(
                button,
                false,
                false,
                event.modifiers.alt,
                event.modifiers.control,
            );
            let steps = delta_lines.unsigned_abs().min(10);
            for _ in 0..steps {
                let seq = sgr_mouse_sequence(button_value, col, row, true);
                input.send(seq.as_bytes());
            }
            return;
        }

        let _ = self.session.scroll_viewport(delta_lines);
        self.sync_viewport_scroll_tracking();
        self.apply_side_effects(cx);
        self.schedule_viewport_refresh(cx);
    }

    fn mouse_position_to_viewport_index(
        &self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        let rows = self.session.rows() as usize;
        if rows == 0 {
            return None;
        }

        let (_, cell_height) = cell_metrics(window, &self.font, self.font_size)?;
        let y = f32::from(position.y);
        let mut row_index = (y / cell_height).floor() as i32;
        if row_index < 0 {
            row_index = 0;
        }
        if row_index >= rows as i32 {
            row_index = rows as i32 - 1;
        }
        let row_index = row_index as usize;

        if let Some(Some(line)) = self.line_layouts.get(row_index) {
            let byte_index = line
                .closest_index_for_x(px(f32::from(position.x)))
                .min(line.text.len());
            let offset = *self.viewport_line_offsets.get(row_index).unwrap_or(&0);
            return Some(offset.saturating_add(byte_index));
        }

        let (col, row) = self.mouse_position_to_cell(position, window)?;
        let row_index = row.saturating_sub(1) as usize;
        let line = self.viewport_lines.get(row_index)?.as_str();
        let byte_index = byte_index_for_column_in_line(line, col).min(line.len());
        let offset = *self.viewport_line_offsets.get(row_index).unwrap_or(&0);
        Some(offset.saturating_add(byte_index))
    }

    fn mouse_position_to_cell(
        &self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
    ) -> Option<(u16, u16)> {
        let cols = self.session.cols();
        let rows = self.session.rows();

        let position = self.mouse_position_to_local(position);
        let (cell_width, cell_height) = cell_metrics(window, &self.font, self.font_size)?;
        let x = f32::from(position.x);
        let y = f32::from(position.y);

        let mut col = (x / cell_width).floor() as i32 + 1;
        let mut row = (y / cell_height).floor() as i32 + 1;

        if col < 1 {
            col = 1;
        }
        if row < 1 {
            row = 1;
        }
        if col > cols as i32 {
            col = cols as i32;
        }
        if row > rows as i32 {
            row = rows as i32;
        }

        Some((col as u16, row as u16))
    }

    fn mouse_position_to_local(
        &self,
        position: gpui::Point<gpui::Pixels>,
    ) -> gpui::Point<gpui::Pixels> {
        window_position_to_local(self.last_bounds, position)
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.marked_text.as_ref()?.as_str();
        let total_utf16 = Self::utf16_len(text);
        let start = range_utf16.start.min(total_utf16);
        let end = range_utf16.end.min(total_utf16);
        let range_utf16 = start..end;
        *adjusted_range = Some(range_utf16.clone());

        let range_utf8 = Self::utf16_range_to_utf8(text, range_utf16)?;
        Some(text.get(range_utf8)?.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked_selected_range_utf16.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let text = self.marked_text.as_ref()?.as_str();
        let len = Self::utf16_len(text);
        (len > 0).then_some(0..len)
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.clear_marked_text(cx);
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        //  Con un selector abierto, lo que se teclea es SUYO. El texto normal
        //  no llega por `on_key_down` sino por aquí —es la vía del método de
        //  entrada, la que hace que funcionen las teclas muertas y el CJK—,
        //  así que sin esta guarda el filtro se escribía además en la sesión:
        //  `trab` + Intro dejaba un precioso `trabssh trabajo` en el prompt.
        if self.servidores.is_some() || self.busqueda.is_some() {
            return;
        }
        self.clear_marked_text(cx);
        self.commit_text(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_marked_text(new_text.to_string(), new_selected_range, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (col, row) = self.session.cursor_position()?;
        let (cell_width, cell_height) = cell_metrics(window, &self.font, self.font_size)?;

        let base_x = element_bounds.left() + px(cell_width * (col.saturating_sub(1)) as f32);
        let base_y = element_bounds.top() + px(cell_height * (row.saturating_sub(1)) as f32);

        let offset_cells = self
            .marked_text
            .as_ref()
            .map(|text| Self::cell_offset_for_utf16(text.as_str(), range_utf16.start))
            .unwrap_or(range_utf16.start);
        let x = base_x + px(cell_width * offset_cells as f32);
        Some(Bounds::new(
            point(x, base_y),
            size(px(cell_width), px(cell_height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

struct TerminalPrepaintState {
    line_height: Pixels,
    shaped_lines: Vec<gpui::ShapedLine>,
    background_quads: Vec<PaintQuad>,
    selection_quads: Vec<PaintQuad>,
    box_drawing_quads: Vec<PaintQuad>,
    marked_text: Option<(gpui::ShapedLine, gpui::Point<Pixels>)>,
    marked_text_background: Option<PaintQuad>,
    estela: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    //  Lo de la casa: el filete de cada mandato, el velo del modo tranquilo
    //  y la barra de desplazamiento.
    adornos: Vec<PaintQuad>,
}

//  Los filetes, el velo y la barra. Se calculan aquí porque necesitan el alto
//  de línea, que no se sabe hasta que la fuente está medida.
fn adornos_de_la_casa(
    vista: &TerminalView,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    filas: usize,
    ancho_celda: f32,
) -> Vec<PaintQuad> {
    let mut salida = Vec::new();
    let Some((arriba, total)) = vista.session.viewport_position() else {
        return salida;
    };

    let y_de = |indice: usize| bounds.top() + line_height * indice as f32;

    // ── todas las coincidencias de la búsqueda ────────────────────
    //
    //  Saltar a una está bien; ver dónde están todas es lo que hace que la
    //  búsqueda sirva. La activa va en sólido, las demás insinuadas.
    if let Some(b) = vista.busqueda.as_ref() {
        let patron = b.patron.to_lowercase();
        if !patron.is_empty() {
            let activa = b.resultados.get(b.indice).copied();
            for (indice, linea) in vista.viewport_lines.iter().enumerate() {
                if indice >= filas {
                    break;
                }
                let bajo = linea.to_lowercase();
                let esta_activa = activa == Some(arriba + indice as u32);
                let mut desde = 0usize;
                while let Some(pos) = bajo[desde..].find(&patron) {
                    let ini = desde + pos;
                    //  Columnas, no bytes: una tilde ocupa dos bytes y una
                    //  columna, y por bytes el resalte se desplaza.
                    let col = bajo[..ini].chars().count();
                    let ancho = patron.chars().count();
                    salida.push(fill(
                        Bounds::new(
                            point(bounds.left() + px(ancho_celda * col as f32), y_de(indice)),
                            size(px(ancho_celda * ancho as f32), line_height),
                        ),
                        if esta_activa {
                            hsla(0.13, 1.0, 0.52, 0.55)
                        } else {
                            hsla(0.13, 1.0, 0.52, 0.22)
                        },
                    ));
                    desde = ini + patron.len();
                }
            }
        }
    }

    // ── el filete de cada mandato ─────────────────────────────────
    //
    //  Dos píxeles en el margen izquierdo: verde si salió bien, rojo si no,
    //  apagado mientras corre. Nada hasta que significa algo.
    for bloque in &vista.bloques {
        let fin = bloque.fin.unwrap_or(arriba + filas as u32);
        if fin < arriba || bloque.inicio >= arriba + filas as u32 {
            continue;
        }
        let desde = bloque.inicio.max(arriba) - arriba;
        let hasta = fin.min(arriba + filas as u32 - 1) - arriba;
        if hasta < desde {
            continue;
        }

        let color = match bloque.salida {
            None => hsla(0., 0., 0.56, 0.45),        // corriendo
            Some(0) => hsla(0.38, 0.72, 0.51, 0.85), // verde de la casa
            Some(_) => hsla(0.01, 1.0, 0.61, 0.85),  // rojo de la casa
        };

        salida.push(fill(
            Bounds::new(
                point(bounds.left() - px(8.), y_de(desde as usize)),
                size(px(2.), line_height * (hasta - desde + 1) as f32),
            ),
            color,
        ));
    }

    // ── el velo del modo tranquilo ────────────────────────────────
    if vista.tranquilo
        && let Some(ultimo) = vista.bloques.last()
        && ultimo.inicio > arriba
    {
        let hasta = (ultimo.inicio - arriba).min(filas as u32);
        salida.push(fill(
            Bounds::new(
                point(bounds.left() - px(12.), bounds.top()),
                size(bounds.size.width + px(24.), line_height * hasta as f32),
            ),
            hsla(0., 0., 0., 0.55),
        ));
    }

    // ── la barra de desplazamiento ────────────────────────────────
    //
    //  Fina, sin surco y solo cuando hay algo que decir: si todo cabe en la
    //  pantalla, no aparece. Es la IslandScrollBar de la barra, aquí.
    if total > filas as u32 {
        let alto = bounds.size.height;
        let proporcion = (filas as f32 / total as f32).clamp(0.04, 1.0);
        let recorrido = (total - filas as u32) as f32;
        let avance = if recorrido > 0.0 {
            (arriba as f32 / recorrido).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let alto_pulgar = alto * proporcion;
        let y = bounds.top() + (alto - alto_pulgar) * avance;

        salida.push(fill(
            Bounds::new(point(bounds.right() + px(4.), y), size(px(3.), alto_pulgar)),
            hsla(0., 0., 1.0, 0.22),
        ));
    }

    salida
}

const CELL_STYLE_FLAG_BOLD: u8 = 0x02;
const CELL_STYLE_FLAG_ITALIC: u8 = 0x04;
const CELL_STYLE_FLAG_UNDERLINE: u8 = 0x08;
const CELL_STYLE_FLAG_FAINT: u8 = 0x10;
const CELL_STYLE_FLAG_STRIKETHROUGH: u8 = 0x40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextRunKey {
    fg: Rgb,
    flags: u8,
}

fn hsla_from_rgb(rgb: Rgb) -> gpui::Hsla {
    let rgba = gpui::Rgba {
        r: rgb.r as f32 / 255.0,
        g: rgb.g as f32 / 255.0,
        b: rgb.b as f32 / 255.0,
        a: 1.0,
    };
    rgba.into()
}

fn cursor_color_for_background(background: Rgb) -> gpui::Hsla {
    let bg = hsla_from_rgb(background);
    let mut cursor = if bg.l > 0.6 {
        gpui::black()
    } else {
        gpui::white()
    };
    cursor.a = 0.72;
    cursor
}

fn font_for_flags(base: &gpui::Font, flags: u8) -> gpui::Font {
    let mut font = base.clone();
    if flags & CELL_STYLE_FLAG_BOLD != 0 {
        font = font.bold();
    }
    if flags & CELL_STYLE_FLAG_ITALIC != 0 {
        font = font.italic();
    }
    font
}

fn color_for_key(key: TextRunKey) -> gpui::Hsla {
    let mut color = hsla_from_rgb(key.fg);
    if key.flags & CELL_STYLE_FLAG_FAINT != 0 {
        color = color.alpha(0.65);
    }
    color
}

pub(crate) const BOX_DIR_LEFT: u8 = 0x01;
pub(crate) const BOX_DIR_RIGHT: u8 = 0x02;
pub(crate) const BOX_DIR_UP: u8 = 0x04;
pub(crate) const BOX_DIR_DOWN: u8 = 0x08;

pub(crate) fn box_drawing_mask(ch: char) -> Option<(u8, f32)> {
    let light = 1.0;
    let heavy = 1.35;
    let double = 1.15;

    let mask = match ch {
        '─' | '━' | '═' => BOX_DIR_LEFT | BOX_DIR_RIGHT,
        '│' | '┃' | '║' => BOX_DIR_UP | BOX_DIR_DOWN,
        '┌' | '┏' | '╔' | '╭' => BOX_DIR_RIGHT | BOX_DIR_DOWN,
        '┐' | '┓' | '╗' | '╮' => BOX_DIR_LEFT | BOX_DIR_DOWN,
        '└' | '┗' | '╚' | '╰' => BOX_DIR_RIGHT | BOX_DIR_UP,
        '┘' | '┛' | '╝' | '╯' => BOX_DIR_LEFT | BOX_DIR_UP,
        '├' | '┣' | '╠' => BOX_DIR_RIGHT | BOX_DIR_UP | BOX_DIR_DOWN,
        '┤' | '┫' | '╣' => BOX_DIR_LEFT | BOX_DIR_UP | BOX_DIR_DOWN,
        '┬' | '┳' | '╦' => BOX_DIR_LEFT | BOX_DIR_RIGHT | BOX_DIR_DOWN,
        '┴' | '┻' | '╩' => BOX_DIR_LEFT | BOX_DIR_RIGHT | BOX_DIR_UP,
        '┼' | '╋' | '╬' => BOX_DIR_LEFT | BOX_DIR_RIGHT | BOX_DIR_UP | BOX_DIR_DOWN,
        _ => return None,
    };

    let scale = match ch {
        '━' | '┃' | '┏' | '┓' | '┗' | '┛' | '┣' | '┫' | '┳' | '┻' | '╋' => {
            heavy
        }
        '═' | '║' | '╔' | '╗' | '╚' | '╝' | '╠' | '╣' | '╦' | '╩' | '╬' => {
            double
        }
        _ => light,
    };

    Some((mask, scale))
}

fn box_drawing_quads_for_char(
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    cell_width: f32,
    color: gpui::Hsla,
    ch: char,
) -> Vec<PaintQuad> {
    let Some((mask, scale)) = box_drawing_mask(ch) else {
        return Vec::new();
    };

    let x0 = bounds.left();
    let x1 = x0 + px(cell_width);
    let y0 = bounds.top();
    let y1 = y0 + line_height;

    let mid_x = x0 + px(cell_width * 0.5);
    let mid_y = y0 + line_height * 0.5;

    let thickness = px(((f32::from(line_height) / 12.0).max(1.0) * scale).max(1.0));
    let half_t = thickness * 0.5;

    let has_left = mask & BOX_DIR_LEFT != 0;
    let has_right = mask & BOX_DIR_RIGHT != 0;
    let has_up = mask & BOX_DIR_UP != 0;
    let has_down = mask & BOX_DIR_DOWN != 0;

    let mut quads = Vec::new();

    if has_left || has_right {
        let (start_x, end_x) = if has_left && has_right {
            (x0, x1)
        } else if has_left {
            (x0, mid_x)
        } else {
            (mid_x, x1)
        };
        quads.push(fill(
            Bounds::from_corners(point(start_x, mid_y - half_t), point(end_x, mid_y + half_t)),
            color,
        ));
    }

    if has_up || has_down {
        let (start_y, end_y) = if has_up && has_down {
            (y0, y1)
        } else if has_up {
            (y0, mid_y)
        } else {
            (mid_y, y1)
        };

        quads.push(fill(
            Bounds::from_corners(point(mid_x - half_t, start_y), point(mid_x + half_t, end_y)),
            color,
        ));
    }

    quads
}

fn text_run_for_key(base_font: &gpui::Font, key: TextRunKey, len: usize) -> TextRun {
    let font = font_for_flags(base_font, key.flags);
    let color = color_for_key(key);

    let underline = (key.flags & CELL_STYLE_FLAG_UNDERLINE != 0).then_some(UnderlineStyle {
        color: Some(color),
        thickness: px(1.0),
        wavy: false,
    });

    let strikethrough =
        (key.flags & CELL_STYLE_FLAG_STRIKETHROUGH != 0).then_some(gpui::StrikethroughStyle {
            color: Some(color),
            thickness: px(1.0),
        });

    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline,
        strikethrough,
    }
}

pub(crate) fn byte_index_for_column_in_line(line: &str, col: u16) -> usize {
    use unicode_width::UnicodeWidthChar as _;

    let col = col.max(1) as usize;
    if col == 1 {
        return 0;
    }

    let mut current_col = 1usize;
    for (byte_index, ch) in line.char_indices() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            continue;
        }

        if current_col == col {
            return byte_index;
        }

        let next_col = current_col.saturating_add(width);
        if col < next_col {
            return byte_index;
        }

        current_col = next_col;
    }

    line.len()
}

struct TerminalTextElement {
    view: gpui::Entity<TerminalView>,
}

impl IntoElement for TerminalTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalTextElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let mut style = window.text_style();
        let font = { self.view.read(cx).font.clone() };
        let font_size_override = { self.view.read(cx).font_size };
        style.font_family = font.family.clone();
        style.font_features = crate::default_terminal_font_features();
        style.font_fallbacks = font.fallbacks.clone();
        if let Some(s) = font_size_override {
            style.font_size = s.into();
        }
        let default_fg = { self.view.read(cx).session.default_foreground() };
        style.color = hsla_from_rgb(default_fg);
        let rem_size = window.rem_size();
        let font_size = style.font_size.to_pixels(rem_size);
        let line_height = style.line_height.to_pixels(style.font_size, rem_size);

        let run_font = style.font();
        let run_color = style.color;

        let cell_width = cell_metrics(window, &font, font_size_override).map(|(w, _)| px(w));

        self.view.update(cx, |view, _cx| {
            if view.viewport_lines.is_empty() {
                view.line_layouts.clear();
                view.line_layout_key = None;
                return;
            }

            if view.line_layout_key != Some((font_size, line_height))
                || view.line_layouts.len() != view.viewport_lines.len()
            {
                view.line_layout_key = Some((font_size, line_height));
                view.line_layouts = vec![None; view.viewport_lines.len()];
            }

            for (idx, line) in view.viewport_lines.iter().enumerate() {
                let Some(slot) = view.line_layouts.get_mut(idx) else {
                    continue;
                };

                if let Some(existing) = slot.as_ref()
                    && existing.text.as_str() == line.as_str()
                {
                    continue;
                }

                let text = SharedString::from(line.clone());
                let mut runs: Vec<TextRun> = Vec::new();

                if let Some(style_runs) = view.viewport_style_runs.get(idx)
                    && !style_runs.is_empty()
                {
                    let mut byte_pos = 0usize;
                    for style in style_runs.iter() {
                        let key = TextRunKey {
                            fg: style.fg,
                            flags: style.flags
                                & (CELL_STYLE_FLAG_BOLD
                                    | CELL_STYLE_FLAG_ITALIC
                                    | CELL_STYLE_FLAG_UNDERLINE
                                    | CELL_STYLE_FLAG_FAINT
                                    | CELL_STYLE_FLAG_STRIKETHROUGH),
                        };

                        let start = byte_index_for_column_in_line(text.as_str(), style.start_col)
                            .min(text.len());
                        let end = byte_index_for_column_in_line(
                            text.as_str(),
                            style.end_col.saturating_add(1),
                        )
                        .min(text.len());

                        if start > byte_pos {
                            runs.push(TextRun {
                                len: start.saturating_sub(byte_pos),
                                font: run_font.clone(),
                                color: run_color,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            });
                            byte_pos = start;
                        }

                        if end > start {
                            runs.push(text_run_for_key(&run_font, key, end.saturating_sub(start)));
                            byte_pos = end;
                        }
                    }

                    if byte_pos < text.len() {
                        runs.push(TextRun {
                            len: text.len().saturating_sub(byte_pos),
                            font: run_font.clone(),
                            color: run_color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        });
                    }
                }

                if runs.is_empty() {
                    runs.push(TextRun {
                        len: text.len(),
                        font: run_font.clone(),
                        color: run_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    });
                }

                let force_width = cell_width.and_then(|cell_width| {
                    use unicode_width::UnicodeWidthChar as _;
                    let has_wide = text.as_str().chars().any(|ch| ch.width().unwrap_or(0) > 1);
                    (!has_wide).then_some(cell_width)
                });
                let shaped = window
                    .text_system()
                    .shape_line(text, font_size, &runs, force_width);
                *slot = Some(shaped);
            }
        });

        let default_bg = { self.view.read(cx).session.default_background() };
        let background_quads = cell_metrics(window, &font, font_size_override)
            .map(|(cell_width, _)| {
                let origin = bounds.origin;
                let mut quads: Vec<PaintQuad> = Vec::new();

                let view = self.view.read(cx);
                for (row, runs) in view.viewport_style_runs.iter().enumerate() {
                    if runs.is_empty() {
                        continue;
                    }

                    let y = origin.y + line_height * row as f32;
                    for run in runs.iter() {
                        if run.bg == default_bg {
                            continue;
                        }

                        let x =
                            origin.x + px(cell_width * (run.start_col.saturating_sub(1)) as f32);
                        let w = px(cell_width
                            * (run.end_col.saturating_sub(run.start_col).saturating_add(1)) as f32);
                        let color = rgba(
                            (u32::from(run.bg.r) << 24)
                                | (u32::from(run.bg.g) << 16)
                                | (u32::from(run.bg.b) << 8)
                                | 0xFF,
                        );
                        quads.push(fill(Bounds::new(point(x, y), size(w, line_height)), color));
                    }
                }

                quads
            })
            .unwrap_or_default();

        let (shaped_lines, selection, line_offsets) = {
            let view = self.view.read(cx);
            (
                view.line_layouts
                    .iter()
                    .map(|line| line.clone().unwrap_or_default())
                    .collect::<Vec<_>>(),
                view.selection,
                view.viewport_line_offsets.clone(),
            )
        };

        let (marked_text, cursor_position, font) = {
            let view = self.view.read(cx);
            (
                view.marked_text.clone(),
                view.session.cursor_position(),
                view.font.clone(),
            )
        };

        let (marked_text, marked_text_background) = marked_text
            .and_then(|text| {
                if text.is_empty() {
                    return None;
                }
                let (col, row) = cursor_position?;
                let (cell_width, _) = cell_metrics(window, &font, font_size_override)?;

                let origin_x = bounds.left() + px(cell_width * (col.saturating_sub(1)) as f32);
                let origin_y = bounds.top() + line_height * (row.saturating_sub(1)) as f32;
                let origin = point(origin_x, origin_y);

                let run = TextRun {
                    len: text.len(),
                    font: run_font.clone(),
                    color: run_color,
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        color: Some(run_color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                let force_width = {
                    use unicode_width::UnicodeWidthChar as _;
                    let has_wide = text.as_str().chars().any(|ch| ch.width().unwrap_or(0) > 1);
                    (!has_wide).then_some(px(cell_width))
                };
                let shaped =
                    window
                        .text_system()
                        .shape_line(text.clone(), font_size, &[run], force_width);

                let bg = {
                    let view = self.view.read(cx);
                    let row_index = row.saturating_sub(1) as usize;
                    view.viewport_style_runs
                        .get(row_index)
                        .and_then(|runs| {
                            runs.iter().find_map(|run| {
                                (col >= run.start_col && col <= run.end_col).then_some(run.bg)
                            })
                        })
                        .unwrap_or(default_bg)
                };

                let cell_len = {
                    use unicode_width::UnicodeWidthChar as _;
                    let mut cells = 0usize;
                    for ch in text.as_str().chars() {
                        let w = ch.width().unwrap_or(0);
                        if w > 0 {
                            cells = cells.saturating_add(w);
                        }
                    }
                    cells.max(1)
                };

                let marked_text_background = fill(
                    Bounds::new(origin, size(px(cell_width * cell_len as f32), line_height)),
                    rgba(
                        (u32::from(bg.r) << 24)
                            | (u32::from(bg.g) << 16)
                            | (u32::from(bg.b) << 8)
                            | 0xFF,
                    ),
                );

                Some(((shaped, origin), marked_text_background))
            })
            .map(|(text, bg)| (Some(text), Some(bg)))
            .unwrap_or((None, None));

        let selection_quads = selection
            .map(|sel| sel.range())
            .filter(|range| !range.is_empty())
            .map(|range| {
                let highlight = hsla(0.58, 0.9, 0.55, 0.35);
                let mut quads = Vec::new();

                for (row, line) in shaped_lines.iter().enumerate() {
                    let Some(&line_offset) = line_offsets.get(row) else {
                        continue;
                    };

                    let line_start = line_offset;
                    let line_end = line_offset.saturating_add(line.text.len());

                    let seg_start = range.start.max(line_start).min(line_end);
                    let seg_end = range.end.max(line_start).min(line_end);
                    if seg_start >= seg_end {
                        continue;
                    }

                    let local_start = seg_start.saturating_sub(line_start);
                    let local_end = seg_end.saturating_sub(line_start);

                    let x1 = line.x_for_index(local_start);
                    let x2 = line.x_for_index(local_end);

                    let y1 = bounds.top() + line_height * row as f32;
                    let y2 = y1 + line_height;

                    quads.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + x1, y1),
                            point(bounds.left() + x2, y2),
                        ),
                        highlight,
                    ));
                }

                quads
            })
            .unwrap_or_default();

        let box_drawing_quads = cell_metrics(window, &font, font_size_override)
            .map(|(cell_width, _)| {
                use unicode_width::UnicodeWidthChar as _;
                let default_fg = run_color;
                let mut quads = Vec::new();

                let view = self.view.read(cx);
                for (row, line) in view.viewport_lines.iter().enumerate() {
                    let y = bounds.top() + line_height * row as f32;
                    let runs = view.viewport_style_runs.get(row).map(|v| v.as_slice());
                    let mut run_idx: usize = 0;

                    let mut col = 1usize;
                    for ch in line.chars() {
                        let width = ch.width().unwrap_or(0);
                        if width == 0 {
                            continue;
                        }

                        if let Some((_, _)) = box_drawing_mask(ch) {
                            let fg = runs
                                .and_then(|runs| {
                                    while let Some(run) = runs.get(run_idx) {
                                        if (col as u16) <= run.end_col {
                                            break;
                                        }
                                        run_idx = run_idx.saturating_add(1);
                                    }
                                    runs.get(run_idx).and_then(|run| {
                                        (col as u16 >= run.start_col && (col as u16) <= run.end_col)
                                            .then_some(run)
                                    })
                                })
                                .map(|run| {
                                    let key = TextRunKey {
                                        fg: run.fg,
                                        flags: run.flags
                                            & (CELL_STYLE_FLAG_FAINT
                                                | CELL_STYLE_FLAG_BOLD
                                                | CELL_STYLE_FLAG_ITALIC
                                                | CELL_STYLE_FLAG_UNDERLINE
                                                | CELL_STYLE_FLAG_STRIKETHROUGH),
                                    };
                                    color_for_key(key)
                                })
                                .unwrap_or(default_fg);

                            let x = bounds.left() + px(cell_width * (col.saturating_sub(1)) as f32);
                            let cell_bounds =
                                Bounds::new(point(x, y), size(px(cell_width), line_height));
                            quads.extend(box_drawing_quads_for_char(
                                cell_bounds,
                                line_height,
                                cell_width,
                                fg,
                                ch,
                            ));
                        }

                        col = col.saturating_add(width);
                    }
                }

                quads
            })
            .unwrap_or_default();

        let cursor = {
            let view = self.view.read(cx);
            view.focus_handle
                .is_focused(window)
                .then(|| view.session.cursor_position())
                .flatten()
        }
        .and_then(|(col, row)| {
            let background = { self.view.read(cx).session.default_background() };
            let cursor_color = cursor_color_for_background(background);
            let y = bounds.top() + line_height * (row.saturating_sub(1)) as f32;
            let row_index = row.saturating_sub(1) as usize;
            let line = shaped_lines.get(row_index)?;
            // The VT cursor is a grid coordinate, not a byte offset into the
            // visible text. Interactive CLIs commonly keep trailing spaces
            // after the last word, while viewport dumps trim those spaces;
            // using `line.x_for_index` in that case pins the caret to the last
            // glyph. Use the cell grid whenever metrics are available so a
            // cursor after a space is rendered in the correct column.
            let x = cell_width
                .map(|width| bounds.left() + width * col.saturating_sub(1) as f32)
                .unwrap_or_else(|| {
                    let byte_index = byte_index_for_column_in_line(line.text.as_str(), col);
                    bounds.left() + line.x_for_index(byte_index.min(line.text.len()))
                });

            //  Kitty's trail is an after-image: the real cursor follows the
            //  terminal immediately, while only large jumps leave a fading
            //  ghost. This avoids a visible input lag when typing quickly.
            let destino = point(x, y);
            let pintado = self.view.update(cx, |vista, _| {
                let anterior = vista.cursor_pintado.unwrap_or(destino);
                let ancho = cell_width.map(f32::from).unwrap_or(8.0).max(1.0);
                let alto = f32::from(line_height).max(1.0);
                let dx = f32::from(destino.x - anterior.x).abs() / ancho;
                let dy = f32::from(destino.y - anterior.y).abs() / alto;
                let distancia = dx.hypot(dy);

                //  Kitty's default threshold is two cells: ordinary typing
                //  does not create a cloudy trail, but jumps and page moves
                //  remain easy to follow.
                if vista.estela_largo > 0 && distancia >= 2.0 {
                    let intensidad = (distancia / 24.0).clamp(0.0, 1.0);
                    let lifetime = Duration::from_secs_f32(0.1 + intensidad * 0.3);
                    let ahora = Instant::now();
                    // Sample the movement into a short ribbon. Kitty's
                    // shader renders this as a continuous trail; a handful
                    // of fading after-images gives the same visual result in
                    // GPUI without making the actual cursor lag.
                    let pasos = distancia.ceil().clamp(1.0, vista.estela_largo as f32) as usize;
                    for paso in 0..pasos {
                        let t = if pasos == 1 {
                            0.0
                        } else {
                            paso as f32 / (pasos - 1) as f32
                        };
                        let posicion = point(
                            anterior.x + (destino.x - anterior.x) * t,
                            anterior.y + (destino.y - anterior.y) * t,
                        );
                        // The oldest part of the ribbon has already started
                        // fading when the newest part is created.
                        let edad = (1.0 - t) * 0.35;
                        let started = ahora
                            .checked_sub(Duration::from_secs_f32(lifetime.as_secs_f32() * edad))
                            .unwrap_or(ahora);
                        vista.cursor_estela.push(CursorTrailGhost {
                            position: posicion,
                            started,
                            lifetime,
                        });
                    }
                    let sobran = vista
                        .cursor_estela
                        .len()
                        .saturating_sub(vista.estela_largo as usize);
                    if sobran > 0 {
                        vista.cursor_estela.drain(..sobran);
                    }
                }

                //  Never interpolate the actual caret: doing so makes the
                //  cursor visibly lag behind the bytes just typed.
                vista.cursor_pintado = Some(destino);
                vista.cursor_moviendose = !vista.cursor_estela.is_empty();
                destino
            });

            //  La figura que pida el programa (DECSCUSR): barra al escribir,
            //  bloque en el modo normal de vim, subrayado si lo pide. El
            //  bloque va translúcido a propósito — tapa la letra de debajo y
            //  así se sigue leyendo, que es lo que consigue una terminal al
            //  invertir la celda.
            //
            //  El parpadeo no se atiende, y es una decisión: esta ventana solo
            //  pide fotogramas mientras hay estela que apagar, y hacer que
            //  parpadee sería pedirlos para siempre.
            let ancho_celda = cell_width.unwrap_or(px(8.0));
            let quad = match self.view.read(cx).session.cursor_style().figura() {
                ghostty_vt::Figura::Bloque => fill(
                    Bounds::new(pintado, size(ancho_celda, line_height)),
                    cursor_color.opacity(0.45),
                ),
                ghostty_vt::Figura::Subrayado => fill(
                    Bounds::new(
                        point(pintado.x, pintado.y + line_height - px(2.0)),
                        size(ancho_celda, px(2.0)),
                    ),
                    cursor_color,
                ),
                ghostty_vt::Figura::Barra => fill(
                    Bounds::new(pintado, size(px(2.0), line_height)),
                    cursor_color,
                ),
            };
            Some(quad)
        });

        //  Remove expired ghosts before painting. The host keeps requesting
        //  frames while this list is non-empty, so the fade is smooth.
        let ahora = Instant::now();
        self.view.update(cx, |vista, _| {
            vista
                .cursor_estela
                .retain(|fantasma| ahora.duration_since(fantasma.started) < fantasma.lifetime);
            vista.cursor_moviendose = !vista.cursor_estela.is_empty();
        });

        //  Fading after-images, from oldest to newest, painted below the
        //  actual cursor.
        let estela: Vec<PaintQuad> = {
            let vista = self.view.read(cx);
            let color = cursor_color_for_background(vista.session.default_background());
            vista
                .cursor_estela
                .iter()
                .filter_map(|fantasma| {
                    let progreso = ahora.duration_since(fantasma.started).as_secs_f32()
                        / fantasma.lifetime.as_secs_f32();
                    let fuerza = (1.0 - progreso.clamp(0.0, 1.0)).powi(2) * 0.35;
                    (fuerza > 0.0).then(|| {
                        fill(
                            Bounds::new(fantasma.position, size(px(2.0), line_height)),
                            color.opacity(fuerza),
                        )
                    })
                })
                .collect()
        };

        let adornos = {
            let ancho = cell_width.map(f32::from).unwrap_or(8.0);
            let vista = self.view.read(cx);
            adornos_de_la_casa(vista, bounds, line_height, shaped_lines.len(), ancho)
        };

        TerminalPrepaintState {
            line_height,
            adornos,
            shaped_lines,
            background_quads,
            selection_quads,
            box_drawing_quads,
            marked_text,
            marked_text_background,
            estela,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let font = self.view.read(cx).font.clone();
        let font_size_override = self.view.read(cx).font_size;
        let metrics = cell_metrics(window, &font, font_size_override);
        self.view.update(cx, |view, cx| {
            view.last_bounds = Some(bounds);

            // La rejilla persigue al elemento: si el tamaño pintado ya no casa
            // con la sesión, se ajusta aquí mismo — el primer pintado incluido,
            // que es el hueco que dejaba observar solo los cambios de ventana.
            if let Some((cell_width, cell_height)) = metrics {
                let cols = (f32::from(bounds.size.width) / cell_width).floor().max(2.0) as u16;
                let rows = (f32::from(bounds.size.height) / cell_height)
                    .floor()
                    .max(2.0) as u16;
                if cols != view.session.cols() || rows != view.session.rows() {
                    view.resize_terminal(cols, rows, cx);
                    if let Some(cb) = view.on_resize.clone() {
                        cb(cols, rows);
                    }
                }
            }
        });

        let focus_handle = { self.view.read(cx).focus_handle.clone() };
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        window.paint_layer(bounds, |window| {
            //  El fondo NO se pinta aquí. Lo pone la lámina de arriba, que es
            //  la que sabe de opacidad y de esquinas; tapar el hueco con un
            //  rectángulo opaco y cuadrado —que es lo que se hacía— se comía
            //  el cristal y el redondeo y dejaba el color solo en el borde.
            for quad in prepaint.background_quads.drain(..) {
                window.paint_quad(quad);
            }

            for quad in prepaint.selection_quads.drain(..) {
                window.paint_quad(quad);
            }

            let origin = bounds.origin;
            for (row, line) in prepaint.shaped_lines.iter().enumerate() {
                let y = origin.y + prepaint.line_height * row as f32;
                let _ = line.paint(
                    point(origin.x, y),
                    prepaint.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }

            for quad in prepaint.box_drawing_quads.drain(..) {
                window.paint_quad(quad);
            }

            //  Los adornos van por encima del texto: el velo tiene que
            //  atenuarlo, y el filete y la barra viven en los márgenes.
            for quad in prepaint.adornos.drain(..) {
                window.paint_quad(quad);
            }

            if let Some(bg) = prepaint.marked_text_background.take() {
                window.paint_quad(bg);
            }

            if let Some((line, origin)) = prepaint.marked_text.as_ref() {
                let _ = line.paint(
                    *origin,
                    prepaint.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }

            for quad in prepaint.estela.drain(..) {
                window.paint_quad(quad);
            }

            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        });
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ensure_key_bindings(cx);

        if !self.pending_output.is_empty() {
            let bytes = std::mem::take(&mut self.pending_output);
            self.feed_output_bytes_to_session(&bytes);
            self.apply_side_effects(cx);
            self.reconcile_dirty_viewport_after_output();
        }

        if self.pending_refresh {
            self.refresh_viewport();
            self.pending_refresh = false;
        }

        //  El título solo se toca cuando la sesión anuncia uno (OSC 0/2): el
        //  inicial lo pone el anfitrión en sus WindowOptions y no hay por qué
        //  machacárselo con un genérico.
        if self.session.window_title_updates_enabled()
            && let Some(title) = self.session.title()
            && self.last_window_title.as_deref() != Some(title)
        {
            window.set_window_title(title);
            self.last_window_title = Some(title.to_string());
        }

        div()
            .size_full()
            .flex()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_tab_prev))
            .on_action(cx.listener(Self::on_increase_font_size))
            .on_action(cx.listener(Self::on_decrease_font_size))
            .on_action(cx.listener(Self::on_reset_font_size))
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(Self::on_previous_block))
            .on_action(cx.listener(Self::on_next_block))
            .on_action(cx.listener(Self::on_copy_last_output))
            .on_action(cx.listener(Self::on_toggle_quiet))
            .on_action(cx.listener(Self::on_send_block_to_note))
            .on_action(cx.listener(Self::on_send_session_to_note))
            .on_action(cx.listener(Self::on_to_island))
            .on_action(cx.listener(Self::on_servers))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            //  El fondo lleva la opacidad de los ajustes: con la ventana en
            //  modo cristal, es lo que deja ver —borroso— lo que hay detrás.
            .bg(hsla_from_rgb(self.session.default_background()).opacity(self.opacidad))
            .text_color(gpui::white())
            .font(self.font.clone())
            //  Más aire arriba que abajo, que es como respiran las terminales
            //  de macOS, y sitio a la izquierda para el filete de los bloques.
            .pt(self.padding + px(6.))
            .pb(self.padding)
            .pl(self.padding + px(8.))
            .pr(self.padding + px(6.))
            .whitespace_nowrap()
            .relative()
            //  Esquinas y filo propios SOLO si se piden. En un compositor que
            //  ya redondea y ya pone borde —Hyprland, sin ir más lejos— poner
            //  los nuestros encima deja dos curvas que no casan; ahí lo que
            //  se quiere es que la lámina llegue hasta el filo y que recorte
            //  el de fuera.
            .when(self.radio > 0., |d| {
                d.rounded(px(self.radio))
                    .border_1()
                    .border_color(hsla(0., 0., 1.0, 0.08))
            })
            .child(TerminalTextElement { view: cx.entity() })
            //  El botón de ajustes: una rueda apagada en la esquina que solo
            //  se ve al acercar el ratón. Una terminal no debe tener cromo
            //  permanente, pero tampoco obligarte a saberte un fichero.
            .children(crate::ajustes_disponibles().then(|| {
                div()
                    .absolute()
                    .bottom_1()
                    .right_2()
                    .px_2()
                    .py_0p5()
                    .rounded(px(8.))
                    .text_color(hsla(0., 0., 0.35, 1.0))
                    .hover(|d| {
                        d.text_color(hsla(0., 0., 0.85, 1.0))
                            .bg(hsla(0., 0., 1.0, 0.06))
                    })
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, _| crate::abrir_ajustes())
                    //  La rueda dentada de la Nerd Font, la misma de la barra.
                    .child("\u{f0493}")
            }))
            //  Un punto ámbar arriba a la derecha cuando el depósito de la
            //  barra está seco. Nada más: quien lo sepa, lo sabe.
            .children(self.seco.then(|| {
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .size(px(6.))
                    .rounded_full()
                    .bg(hsla(0.13, 1.0, 0.52, 0.8))
            }))
            .children(
                self.campana_hasta
                    .filter(|hasta| std::time::Instant::now() < *hasta)
                    .map(|_| div().absolute().inset_0().bg(gpui::rgba(0xffffff20))),
            )
            //  La barra de búsqueda va por encima y no dentro del flujo: así
            //  no le quita filas a la rejilla ni la obliga a rehacerse cada
            //  vez que se abre o se cierra.
            //  El selector de servidores, en medio y arriba: es una decisión
            //  —tapa parte de lo que hay debajo— pero elegir a dónde ir es lo
            //  único que estás haciendo mientras está abierto.
            //  El selector de servidores, en medio y arriba: tapa parte de lo
            //  que hay debajo, y es a propósito — elegir a dónde ir es lo
            //  único que estás haciendo mientras está abierto.
            .children(self.servidores.as_ref().map(|s| {
                let radio = px(self.radio.max(10.));
                let apagado = hsla(0., 0., 0.45, 1.0);

                let mut caja = div()
                    .absolute()
                    .top_8()
                    .left_1_2()
                    .w(px(520.))
                    .ml(px(-260.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(radio)
                    .bg(hsla(0., 0., 0.11, 0.97))
                    .border_1()
                    .border_color(hsla(0., 0., 1.0, 0.08));

                if let Some(f) = s.editando.as_ref() {
                    //  Configurando: arriba lo que entiende ssh, abajo lo
                    //  nuestro, separados por una línea para que se vea de un
                    //  vistazo qué va a dónde.
                    caja = caja.child(
                        div()
                            .flex()
                            .gap_2()
                            .px_1()
                            .pb_1()
                            .child(div().text_color(apagado).child(if f.original.is_empty() {
                                "servidor nuevo".to_string()
                            } else {
                                format!("editar {}", f.original)
                            }))
                            .child(
                                div()
                                    .text_color(if f.favorito {
                                        hsla(0.13, 0.9, 0.6, 1.0)
                                    } else {
                                        apagado
                                    })
                                    .child(if f.favorito {
                                        "★ favorito"
                                    } else {
                                        "☆ ctrl+F"
                                    }),
                            ),
                    );

                    for (i, (nombre, ayuda)) in CAMPOS.iter().enumerate() {
                        let activo = i == f.indice;
                        let valor = f.valores[i].clone();
                        //  La raya justo antes de lo nuestro.
                        if i == 6 {
                            caja = caja.child(div().h(px(1.)).my_1().bg(hsla(0., 0., 1.0, 0.08)));
                        }
                        caja = caja.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_0p5()
                                .rounded(px(8.))
                                .when(activo, |d| d.bg(hsla(0., 0., 0.20, 1.0)))
                                .child(
                                    div()
                                        .w(px(90.))
                                        .text_color(if activo { gpui::white() } else { apagado })
                                        .child(*nombre),
                                )
                                .child(if valor.is_empty() {
                                    div().text_color(hsla(0., 0., 0.35, 1.0)).child(*ayuda)
                                } else {
                                    div().text_color(gpui::white()).child(valor)
                                }),
                        );
                    }

                    caja = caja.child(
                        div()
                            .px_2()
                            .pt_1()
                            .text_color(hsla(0., 0., 0.35, 1.0))
                            .child("intro guarda · esc cancela · ↑↓ cambia de campo"),
                    );

                    return caja;
                }

                let filtrados = s.filtrados();
                let elegido = s.indice.min(filtrados.len().saturating_sub(1));

                caja = caja.child(
                    div()
                        .flex()
                        .gap_2()
                        .px_1()
                        .pb_1()
                        .child(div().text_color(apagado).child("servidor"))
                        .child(div().text_color(gpui::white()).child(s.patron.clone())),
                );

                if filtrados.is_empty() {
                    //  Sin nada que enseñar, decir por qué: la lista vacía la
                    //  primera vez no es un fallo, es que no has guardado
                    //  ninguno todavía.
                    caja = caja.child(div().px_2().py_1().text_color(apagado).child(
                        if s.todos.is_empty() {
                            "no hay servidores en ~/.ssh/config"
                        } else {
                            "ninguno con ese nombre"
                        },
                    ));
                }

                //  Ocho como mucho: una lista más larga que la pantalla no se
                //  lee, se recorre, y para eso está el filtro.
                for (i, servidor) in filtrados.iter().take(8).enumerate() {
                    let esta = i == elegido;
                    caja = caja.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(px(8.))
                            .when(esta, |d| d.bg(hsla(0., 0., 0.20, 1.0)))
                            .child(
                                div()
                                    .text_color(if servidor.favorito {
                                        hsla(0.13, 0.9, 0.6, 1.0)
                                    } else {
                                        apagado
                                    })
                                    .child(if servidor.favorito {
                                        "★"
                                    } else if servidor.rapido {
                                        "+"
                                    } else {
                                        "·"
                                    }),
                            )
                            .child(div().text_color(gpui::white()).child(if servidor.rapido {
                                format!("conectar a {}", servidor.host)
                            } else {
                                servidor.alias.clone()
                            }))
                            .child(div().text_color(apagado).child(servidor.detalle())),
                    );
                }

                //  Las teclas, en el pie y no en la fila: metidas al lado del
                //  nombre se salían de la caja en cuanto el host era largo.
                caja.child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_color(hsla(0., 0., 0.35, 1.0))
                        .child("intro conecta · ctrl+S guarda o edita · supr borra"),
                )
            }))
            .children(self.busqueda.as_ref().map(|b| {
                let total = b.resultados.len();
                let cuenta = if b.calculado != b.patron {
                    "…".to_string()
                } else if total == 0 {
                    "0".to_string()
                } else {
                    format!("{}/{}", b.indice + 1, total)
                };

                //  Vestida de isla: superficie de la casa, esquinas suaves,
                //  la palabra en apagado y el contador en su pastilla azul.
                div()
                    .absolute()
                    .bottom_3()
                    .right_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded(px(self.radio.max(10.)))
                    .bg(hsla(0., 0., 0.11, 0.96))
                    .border_1()
                    .border_color(hsla(0., 0., 1.0, 0.08))
                    .child(div().text_color(hsla(0., 0., 0.56, 1.0)).child("buscar"))
                    .child(div().text_color(gpui::white()).child(b.patron.clone()))
                    .child(
                        div()
                            .px_2()
                            .rounded(px(8.))
                            .bg(hsla(0.58, 1.0, 0.52, 0.22))
                            .text_color(hsla(0.58, 1.0, 0.66, 1.0))
                            .child(cuenta),
                    )
            }))
    }
}

pub(crate) fn cell_metrics(
    window: &mut gpui::Window,
    font: &gpui::Font,
    size: Option<Pixels>,
) -> Option<(f32, f32)> {
    let mut style = window.text_style();
    style.font_family = font.family.clone();
    style.font_features = crate::default_terminal_font_features();
    style.font_fallbacks = font.fallbacks.clone();
    if let Some(s) = size {
        style.font_size = s.into();
    }

    let rem_size = window.rem_size();
    let font_size = style.font_size.to_pixels(rem_size);
    let line_height = style.line_height.to_pixels(style.font_size, rem_size);

    let run = style.to_run(1);
    let lines = window
        .text_system()
        .shape_text(
            gpui::SharedString::from("M"),
            font_size,
            &[run],
            None,
            Some(1),
        )
        .ok()?;
    let line = lines.first()?;

    let cell_width = f32::from(line.width()).max(1.0);
    let cell_height = f32::from(line_height).max(1.0);
    Some((cell_width, cell_height))
}

#[cfg(test)]
mod tests {
    use ghostty_vt::Rgb;

    use super::{url_at_byte_index, url_at_column_in_line, window_position_to_local};

    #[test]
    fn url_detection_finds_https_links() {
        let text = "Visit https://google.com for search";
        let idx = text.find("google").unwrap();
        assert_eq!(
            url_at_byte_index(text, idx).as_deref(),
            Some("https://google.com")
        );
    }

    #[test]
    fn url_detection_finds_https_links_by_cell_column() {
        let line = "https://google.com";
        assert_eq!(
            url_at_column_in_line(line, 1).as_deref(),
            Some("https://google.com")
        );
        assert_eq!(
            url_at_column_in_line(line, 10).as_deref(),
            Some("https://google.com")
        );
    }

    #[test]
    fn mouse_position_to_local_accounts_for_bounds_origin() {
        let bounds = Some(gpui::Bounds::new(
            gpui::point(gpui::px(100.0), gpui::px(20.0)),
            gpui::size(gpui::px(200.0), gpui::px(80.0)),
        ));

        let local = window_position_to_local(bounds, gpui::point(gpui::px(110.0), gpui::px(30.0)));
        assert_eq!(local, gpui::point(gpui::px(10.0), gpui::px(10.0)));
    }

    #[test]
    fn cursor_color_contrasts_with_background() {
        let cursor = super::cursor_color_for_background(Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0xFF,
        });
        assert!(cursor.l < 0.2);
        assert!((cursor.a - 0.72).abs() < f32::EPSILON);

        let cursor = super::cursor_color_for_background(Rgb {
            r: 0x00,
            g: 0x00,
            b: 0x00,
        });
        assert!(cursor.l > 0.8);
        assert!((cursor.a - 0.72).abs() < f32::EPSILON);
    }
}
