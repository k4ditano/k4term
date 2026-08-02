//  k4term — la terminal de la casa k4.
//
//  Fase 1: la app mínima usable. El VT lo pone ghostty (vendorizado), el
//  render GPUI, y esta app pone lo que falta: el PTY con la shell del
//  usuario, la fuente de la casa (MesloLGS, la misma de la barra) y la
//  rejilla siempre a juego con la ventana — la vista ajusta el VT al
//  pintarse y nos avisa por set_on_resize para ajustar el PTY.
//
//  K4TERM_MANDATO="btop" teclea un mandato al arrancar: es el gancho de las
//  pruebas automatizadas con capturas, al estilo de la barra.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gpui::{App, AppContext, Application, KeyBinding, TitlebarOptions, WindowOptions};
use gpui_ghostty_terminal::view::{Copy, Paste, SelectAll, TerminalInput, TerminalView};
use gpui_ghostty_terminal::{TerminalConfig, TerminalSession};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn fuente_de_la_casa() -> gpui::Font {
    let fallbacks = gpui::FontFallbacks::from_fonts(vec![
        "MesloLGS Nerd Font".to_string(),
        "Symbols Nerd Font Mono".to_string(),
        "DejaVu Sans Mono".to_string(),
        "Noto Color Emoji".to_string(),
    ]);
    let mut font = gpui::font("MesloLGS Nerd Font Mono");
    font.fallbacks = Some(fallbacks);
    font
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-a", SelectAll, None),
            KeyBinding::new("ctrl-shift-c", Copy, None),
            KeyBinding::new("ctrl-shift-v", Paste, None),
        ]);

        let opciones = WindowOptions {
            // Con nombre y apellidos: sin app_id la ventana sale con clase
            // vacía y ni Hyprland ni nadie puede dirigirse a ella.
            app_id: Some("k4term".to_string()),
            titlebar: Some(TitlebarOptions {
                title: Some("k4term".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        cx.open_window(opciones, |window, cx| {
            let config = TerminalConfig::default();

            let pty_system = native_pty_system();
            let pty_pair = pty_system
                .openpty(PtySize {
                    rows: config.rows,
                    cols: config.cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("no se pudo abrir el pty");

            let master: Arc<dyn portable_pty::MasterPty + Send> = Arc::from(pty_pair.master);

            //  `k4term -e prog args…` ejecuta eso en vez de la shell — el
            //  contrato de kitty que la barra ya habla (yay -Syu, instalador).
            let argv: Vec<String> = std::env::args().collect();
            let ejecutar: Option<Vec<String>> = argv
                .iter()
                .position(|a| a == "-e")
                .map(|i| argv[i + 1..].to_vec())
                .filter(|resto| !resto.is_empty());

            let mut cmd = match &ejecutar {
                Some(resto) => {
                    let mut c = CommandBuilder::new(&resto[0]);
                    c.args(&resto[1..]);
                    c
                }
                None => {
                    let shell =
                        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
                    let mut c = CommandBuilder::new(shell);
                    c.arg("-l");
                    c
                }
            };
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("TERM_PROGRAM", "k4term");
            if let Ok(home) = std::env::var("HOME") {
                cmd.cwd(home);
            }

            let mut child = pty_pair
                .slave
                .spawn_command(cmd)
                .expect("no arrancó la shell");

            thread::spawn(move || {
                let _ = child.wait();
                // Muere la shell (o el mandato de -e), muere la ventana: el
                // contrato de toda terminal.
                std::process::exit(0);
            });

            let mut pty_reader = master.try_clone_reader().expect("lector del pty");
            let mut pty_writer = master.take_writer().expect("escritor del pty");

            let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
            let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>();

            if let Ok(mandato) = std::env::var("K4TERM_MANDATO") {
                let stdin_tx = stdin_tx.clone();
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(600));
                    let mut mandato = mandato;
                    if !mandato.ends_with('\n') {
                        mandato.push('\n');
                    }
                    let _ = stdin_tx.send(mandato.into_bytes());
                });
            }

            thread::spawn(move || {
                while let Ok(bytes) = stdin_rx.recv() {
                    if pty_writer.write_all(&bytes).is_err() {
                        break;
                    }
                    let _ = pty_writer.flush();
                }
            });

            thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    let n = match pty_reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    let _ = stdout_tx.send(buf[..n].to_vec());
                }
            });

            let view = cx.new(|cx| {
                let focus_handle = cx.focus_handle();
                focus_handle.focus(window, cx);

                let session = TerminalSession::new(config).expect("vt init");
                let stdin_tx = stdin_tx.clone();
                let input = TerminalInput::new(move |bytes| {
                    let _ = stdin_tx.send(bytes.to_vec());
                });

                let mut vista = TerminalView::new_with_input(session, focus_handle, input);
                vista.set_font(fuente_de_la_casa());
                vista.set_padding(gpui::px(12.));

                let master_para_resize = master.clone();
                vista.set_on_resize(move |cols, rows| {
                    let _ = master_para_resize.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                });

                vista
            });

            let view_for_task = view.clone();
            window
                .spawn(cx, async move |cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                        let mut batch = Vec::new();
                        while let Ok(chunk) = stdout_rx.try_recv() {
                            batch.extend_from_slice(&chunk);
                        }
                        if batch.is_empty() {
                            continue;
                        }

                        cx.update(|_, cx| {
                            view_for_task.update(cx, |this, cx| {
                                this.queue_output_bytes(&batch, cx);
                            });
                        })
                        .ok();
                    }
                })
                .detach();

            view
        })
        .unwrap();
    });
}
