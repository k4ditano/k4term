use ghostty_vt::{Error, ModeTracker, Rgb, Terminal};

use crate::TerminalConfig;

pub struct TerminalSession {
    config: TerminalConfig,
    terminal: Terminal,
    //  Los modos, el título y el portapapeles se leen del chorro con la misma
    //  pieza que usa la terminal de la isla: si esto fueran dos copias, el
    //  mismo programa se portaría distinto en cada una.
    modes: ModeTracker,
    dsr_state: DsrScanState,
    osc_query_state: OscQueryScanState,
}

impl TerminalSession {
    pub fn new(config: TerminalConfig) -> Result<Self, Error> {
        let mut terminal = Terminal::new(config.cols, config.rows)?;
        terminal.set_default_colors(config.default_fg, config.default_bg);
        Ok(Self {
            config,
            terminal,
            modes: ModeTracker::new(),
            dsr_state: DsrScanState::default(),
            osc_query_state: OscQueryScanState::default(),
        })
    }

    pub fn cols(&self) -> u16 {
        self.config.cols
    }

    pub fn rows(&self) -> u16 {
        self.config.rows
    }

    //  Los colores de fondo y tinta se pueden cambiar en marcha: es lo que
    //  permite que la terminal siga el ambiente de la barra sin reiniciarse.
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        self.config.default_fg = fg;
        self.config.default_bg = bg;
        self.terminal.set_default_colors(fg, bg);
    }

    pub fn default_foreground(&self) -> Rgb {
        self.config.default_fg
    }

    pub fn default_background(&self) -> Rgb {
        self.config.default_bg
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.modes.bracketed_paste_enabled()
    }

    //  Lo que hay que escribirle al PTY para pegar un texto, con los corchetes
    //  puestos si la aplicación los pidió.
    pub fn paste_payload(&self, text: &str) -> Vec<u8> {
        self.modes.paste_payload(text)
    }

    pub fn mouse_reporting_enabled(&self) -> bool {
        self.modes.mouse_reporting_enabled()
    }

    pub fn mouse_sgr_enabled(&self) -> bool {
        self.modes.mouse_sgr_enabled()
    }

    pub fn mouse_button_event_enabled(&self) -> bool {
        self.modes.mouse_button_event_enabled()
    }

    pub fn mouse_any_event_enabled(&self) -> bool {
        self.modes.mouse_any_event_enabled()
    }

    pub fn title(&self) -> Option<&str> {
        self.modes.title()
    }

    //  La forma de cursor que pide el programa (DECSCUSR). La pregunta va al
    //  VT y no al lector del chorro: es estado del terminal, no un modo que
    //  se pueda leer de paso.
    pub fn cursor_style(&self) -> ghostty_vt::CursorStyle {
        self.terminal.cursor_style()
    }

    pub(crate) fn window_title_updates_enabled(&self) -> bool {
        self.config.update_window_title
    }

    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<String> {
        self.terminal.hyperlink_at(col, row)
    }

    pub fn take_clipboard_write(&mut self) -> Option<String> {
        self.modes.take_clipboard_write()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.modes.feed(bytes);
        self.terminal.feed(bytes)
    }

    pub fn feed_with_pty_responses(
        &mut self,
        bytes: &[u8],
        mut send: impl FnMut(&[u8]),
    ) -> Result<(), Error> {
        self.modes.feed(bytes);

        let mut seg_start = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            let dsr = self.dsr_state.advance(b);
            let osc = self.osc_query_state.advance(b);
            if dsr.is_none() && osc.is_none() {
                continue;
            }

            self.terminal.feed(&bytes[seg_start..=i])?;
            seg_start = i + 1;

            if let Some(query) = dsr {
                match query {
                    TerminalQuery::DeviceStatus => send(b"\x1b[0n"),
                    TerminalQuery::CursorPosition => {
                        let (col, row) = self.cursor_position().unwrap_or((1, 1));
                        let resp = format!("\x1b[{};{}R", row, col);
                        send(resp.as_bytes());
                    }
                }
            }

            if let Some(query) = osc {
                let rgb = match query {
                    OscQuery::ForegroundColor => {
                        let fg = self.config.default_fg;
                        (fg.r, fg.g, fg.b)
                    }
                    OscQuery::BackgroundColor => {
                        let bg = self.config.default_bg;
                        (bg.r, bg.g, bg.b)
                    }
                };
                let resp = osc_color_query_response(query, rgb);
                send(resp.as_bytes());
            }
        }

        if seg_start < bytes.len() {
            self.terminal.feed(&bytes[seg_start..])?;
        }

        Ok(())
    }

    pub fn dump_viewport(&self) -> Result<String, Error> {
        self.terminal.dump_viewport()
    }

    //  (fila donde empieza lo que se ve, filas totales) en coordenadas del
    //  historial. Es lo que permite pintar la barra de desplazamiento y
    //  colocar marcas apuntadas en absoluto.
    pub fn viewport_position(&self) -> Option<(u32, u32)> {
        self.terminal.viewport_position()
    }

    //  Una fila del historial entero, para buscar hacia atrás. `None` cuando
    //  ya no hay más: así se sabe dónde acaba sin preguntar el tamaño.
    pub fn dump_screen_row(&self, row: u32) -> Option<String> {
        self.terminal.dump_screen_row(row)
    }

    pub fn dump_viewport_row(&self, row: u16) -> Result<String, Error> {
        self.terminal.dump_viewport_row(row)
    }

    pub fn dump_viewport_row_cell_styles(
        &self,
        row: u16,
    ) -> Result<Vec<ghostty_vt::CellStyle>, Error> {
        self.terminal.dump_viewport_row_cell_styles(row)
    }

    pub fn dump_viewport_row_style_runs(
        &self,
        row: u16,
    ) -> Result<Vec<ghostty_vt::StyleRun>, Error> {
        self.terminal.dump_viewport_row_style_runs(row)
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        self.terminal.cursor_position()
    }

    pub fn scroll_viewport(&mut self, delta_lines: i32) -> Result<(), Error> {
        self.terminal.scroll_viewport(delta_lines)
    }

    pub fn scroll_viewport_top(&mut self) -> Result<(), Error> {
        self.terminal.scroll_viewport_top()
    }

    pub fn scroll_viewport_bottom(&mut self) -> Result<(), Error> {
        self.terminal.scroll_viewport_bottom()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        self.config.cols = cols;
        self.config.rows = rows;
        self.terminal.resize(cols, rows)
    }

    pub(crate) fn take_dirty_viewport_rows(&mut self) -> Vec<u16> {
        self.terminal
            .take_dirty_viewport_rows(self.config.rows)
            .unwrap_or_default()
    }

    pub(crate) fn take_viewport_scroll_delta(&mut self) -> i32 {
        self.terminal.take_viewport_scroll_delta()
    }
}

#[derive(Clone, Copy, Debug)]
enum TerminalQuery {
    DeviceStatus,
    CursorPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscQuery {
    ForegroundColor,
    BackgroundColor,
}

fn osc_color_query_response(query: OscQuery, (r, g, b): (u8, u8, u8)) -> String {
    let ps = match query {
        OscQuery::ForegroundColor => 10,
        OscQuery::BackgroundColor => 11,
    };

    let r16 = u16::from(r) * 0x0101;
    let g16 = u16::from(g) * 0x0101;
    let b16 = u16::from(b) * 0x0101;

    format!("\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\", ps, r16, g16, b16)
}

#[derive(Clone, Copy, Debug, Default)]
enum DsrScanState {
    #[default]
    Idle,
    Esc,
    Csi,
    CsiQ,
    Csi5,
    CsiQ5,
    Csi6,
    CsiQ6,
}

impl DsrScanState {
    fn advance(&mut self, b: u8) -> Option<TerminalQuery> {
        use DsrScanState::*;

        let matched = match (*self, b) {
            (Csi5, b'n') | (CsiQ5, b'n') => Some(TerminalQuery::DeviceStatus),
            (Csi6, b'n') | (CsiQ6, b'n') => Some(TerminalQuery::CursorPosition),
            _ => None,
        };

        *self = match (*self, b) {
            (_, 0x1b) => Esc,
            (Esc, b'[') => Csi,
            (Csi, b'?') => CsiQ,
            (Csi, b'5') => Csi5,
            (CsiQ, b'5') => CsiQ5,
            (Csi, b'6') => Csi6,
            (CsiQ, b'6') => CsiQ6,
            (Csi5, b'n') => Idle,
            (CsiQ5, b'n') => Idle,
            (Csi6, b'n') => Idle,
            (CsiQ6, b'n') => Idle,
            _ => Idle,
        };

        matched
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum OscQueryScanState {
    #[default]
    Idle,
    Esc,
    Osc,
    Ps {
        value: u32,
    },
    AfterSemicolon {
        ps: u32,
    },
    Query {
        ps: u32,
    },
    StEscape {
        ps: u32,
    },
}

impl OscQueryScanState {
    fn advance(&mut self, b: u8) -> Option<OscQuery> {
        use OscQueryScanState::*;

        let matched = match (*self, b) {
            (Query { ps }, 0x07) => match ps {
                10 => Some(OscQuery::ForegroundColor),
                11 => Some(OscQuery::BackgroundColor),
                _ => None,
            },
            (StEscape { ps }, b'\\') => match ps {
                10 => Some(OscQuery::ForegroundColor),
                11 => Some(OscQuery::BackgroundColor),
                _ => None,
            },
            _ => None,
        };

        *self = match (*self, b) {
            (Query { ps }, 0x1b) => StEscape { ps },
            (_, 0x1b) => Esc,
            (Esc, b']') => Osc,
            (Esc, _) => Idle,
            (Osc, d) if d.is_ascii_digit() => Ps {
                value: (d - b'0') as u32,
            },
            (Ps { value }, d) if d.is_ascii_digit() => Ps {
                value: value.saturating_mul(10).saturating_add((d - b'0') as u32),
            },
            (Ps { value }, b';') => value_to_after_semicolon_state(value),
            (Osc, _) | (Ps { .. }, _) => Idle,
            (AfterSemicolon { ps }, b'?') => Query { ps },
            (AfterSemicolon { .. }, _) => Idle,
            (Query { .. }, 0x07) => Idle,
            (Query { .. }, _) => Idle,
            (StEscape { .. }, b'\\') => Idle,
            (StEscape { .. }, _) => Idle,
            _ => Idle,
        };

        matched
    }
}

fn value_to_after_semicolon_state(ps: u32) -> OscQueryScanState {
    match ps {
        10 | 11 => OscQueryScanState::AfterSemicolon { ps },
        _ => OscQueryScanState::Idle,
    }
}
