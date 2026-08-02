//  k4term — la terminal de la casa k4.
//
//  El VT lo pone ghostty (vendorizado), el render GPUI, y esta app pone lo
//  que falta: el PTY con la shell del usuario, la fuente de la casa y la
//  rejilla siempre a juego con la ventana.
//
//  De la barra sabe lo justo, y todo por k4term_puente: lee el tema que ella
//  publica —y lo sigue en caliente, para que el ambiente de la isla llegue
//  también aquí— y le cuenta los mandatos largos cuando terminan.
//
//      k4term                    abre la shell
//      k4term -d /ruta           abre ahí
//      k4term -e prog args…      ejecuta eso y se cierra al acabar
//
//  K4TERM_MANDATO="btop" teclea un mandato al arrancar: es el gancho de las
//  pruebas automatizadas con capturas, al estilo de la barra.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gpui::{App, AppContext, Application, KeyBinding, TitlebarOptions, WindowOptions};
use gpui_ghostty_terminal::view::{Copy, Paste, SelectAll, TerminalInput, TerminalView};
use gpui_ghostty_terminal::{Rgb, TerminalConfig, TerminalSession};
use k4term_puente::{Aviso, Suceso, barra, osc::Escaner, tema, trabajos};
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

fn rgb(c: tema::Color) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

struct Argumentos {
    ejecutar: Option<Vec<String>>,
    directorio: Option<PathBuf>,
}

fn argumentos() -> Argumentos {
    let argv: Vec<String> = std::env::args().collect();

    //  Todo lo que va detrás de -e es del mandato, incluida cualquier cosa que
    //  parezca una opción nuestra.
    let ejecutar = argv
        .iter()
        .position(|a| a == "-e")
        .map(|i| argv[i + 1..].to_vec())
        .filter(|resto| !resto.is_empty());

    let hasta = argv.iter().position(|a| a == "-e").unwrap_or(argv.len());
    let directorio = argv[..hasta]
        .iter()
        .position(|a| a == "-d" || a == "--cwd")
        .and_then(|i| argv.get(i + 1))
        .map(PathBuf::from)
        .filter(|d| d.is_dir());

    Argumentos {
        ejecutar,
        directorio,
    }
}

//  Dónde abrir: lo que digan los argumentos, si no de donde te lanzaron, y si
//  eso no vale (uwsm lanza desde la raíz) tu casa.
fn directorio_inicial(pedido: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(d) = pedido {
        return Some(d);
    }
    if let Ok(actual) = std::env::current_dir() {
        if actual != PathBuf::from("/") {
            return Some(actual);
        }
    }
    std::env::var("HOME").ok().map(PathBuf::from)
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
            let args = argumentos();

            //  El tema de la barra manda desde el primer fotograma: leerlo
            //  aquí evita el fogonazo de abrir en negro y teñirse después.
            let ruta_tema = tema::ruta_por_defecto();
            let mut config = TerminalConfig::default();
            if let Some(t) = tema::leer(&ruta_tema) {
                config.default_fg = rgb(t.tinta);
                config.default_bg = rgb(t.fondo);
            }

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

            let mut cmd = match &args.ejecutar {
                Some(resto) => {
                    let mut c = CommandBuilder::new(&resto[0]);
                    c.args(&resto[1..]);
                    c
                }
                None => {
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
                    let mut c = CommandBuilder::new(shell);
                    c.arg("-l");
                    c
                }
            };
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("TERM_PROGRAM", "k4term");
            if let Some(d) = directorio_inicial(args.directorio) {
                cmd.cwd(d);
            }

            let mut child = pty_pair
                .slave
                .spawn_command(cmd)
                .expect("no arrancó la shell");

            thread::spawn(move || {
                let _ = child.wait();
                // Lo que estuviera corriendo se va con la ventana: que la
                // barra no se quede con el indicador de un muerto.
                barra::limpiar(std::process::id());
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

            //  El lector es también quien vigila los marcadores de la shell:
            //  los bytes pasan por aquí antes de llegar al terminal, así que
            //  se miran al vuelo y siguen intactos.
            let avisos = trabajos::notificador(std::process::id());
            thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let mut escaner = Escaner::new();
                let mut mandato = String::new();

                loop {
                    let n = match pty_reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };

                    for suceso in escaner.tragar(&buf[..n]) {
                        match suceso {
                            Suceso::Mandato(m) => mandato = m,
                            Suceso::Comienza => {
                                //  Sin integración de shell no hay nombre, y
                                //  un indicador que no dice qué corre no vale
                                //  para nada: mejor callarse.
                                if !mandato.trim().is_empty() {
                                    let _ = avisos.send(Aviso::Empieza {
                                        mandato: mandato.trim().to_string(),
                                    });
                                }
                            }
                            Suceso::Termina { salida } => {
                                let _ = avisos.send(Aviso::Acaba { salida });
                                mandato.clear();
                            }
                            // El directorio se sigue ya, pero todavía no lo
                            // usa nadie: será el «abre otra aquí mismo».
                            Suceso::Directorio(_) => {}
                        }
                    }

                    if stdout_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
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

            let temas = tema::vigilar(ruta_tema);
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

                        // Del ambiente solo interesa el último: entre dos
                        // fotogramas la barra puede haber escrito tres pasos
                        // de una animación de tinte.
                        let ultimo_tema = std::iter::from_fn(|| temas.try_recv().ok()).last();

                        if batch.is_empty() && ultimo_tema.is_none() {
                            continue;
                        }

                        cx.update(|_, cx| {
                            view_for_task.update(cx, |this, cx| {
                                if !batch.is_empty() {
                                    this.queue_output_bytes(&batch, cx);
                                }
                                if let Some(t) = ultimo_tema {
                                    this.set_default_colors(rgb(t.tinta), rgb(t.fondo), cx);
                                }
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
