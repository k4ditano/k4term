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
//      k4term --heredar SOCKET   adopta una sesión viva que viene de la isla
//
//  K4TERM_MANDATO="btop" teclea un mandato al arrancar: es el gancho de las
//  pruebas automatizadas con capturas, al estilo de la barra.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gpui::{App, AppContext, Application, KeyBinding, TitlebarOptions, WindowOptions};
use gpui_ghostty_terminal::view::{
    Appearance, Copy, CopyLastOutput, DecreaseFontSize, Find, IncreaseFontSize, NextBlock, Paste,
    PreviousBlock, ResetFontSize, SelectAll, SendBlockToNote, SendSessionToNote, TerminalInput,
    TerminalView, ToIsland, ToggleQuiet,
};
use gpui_ghostty_terminal::{Rgb, TerminalConfig, TerminalSession};
use k4term_puente::{
    Ajustes, Aviso, Suceso, barra, edinot, osc::Escaner, tema, trabajos, traspaso,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

//  La que digan los ajustes, con los respaldos de siempre detrás: si la
//  elegida no tiene un glifo —o no está instalada— que haya de dónde tirar.
fn fuente(nombre: String) -> gpui::Font {
    let fallbacks = gpui::FontFallbacks::from_fonts(vec![
        "MesloLGS Nerd Font Mono".to_string(),
        "MesloLGS Nerd Font".to_string(),
        "Symbols Nerd Font Mono".to_string(),
        "DejaVu Sans Mono".to_string(),
        "Noto Color Emoji".to_string(),
    ]);
    let mut font = gpui::font(nombre);
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

//  Los límites de cada mandato, camino de la vista: allí se apuntan en
//  coordenadas del historial para poder pintar su filete.
enum Marca {
    Empieza,
    Acaba { salida: i32 },
}

struct Argumentos {
    ejecutar: Option<Vec<String>>,
    directorio: Option<PathBuf>,
    //  Una sesión que ya existe y viene de otro sitio —de la isla— esperando
    //  en ese socket. Con esto no se abre ninguna shell: se adopta la que hay.
    heredar: Option<PathBuf>,
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

    let heredar = argv[..hasta]
        .iter()
        .position(|a| a == "--heredar")
        .and_then(|i| argv.get(i + 1))
        .map(PathBuf::from);

    Argumentos {
        ejecutar,
        directorio,
        heredar,
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
        let ajustes = Ajustes::leer();

        cx.bind_keys([
            KeyBinding::new("ctrl-shift-a", SelectAll, None),
            KeyBinding::new("ctrl-shift-c", Copy, None),
            KeyBinding::new("ctrl-shift-v", Paste, None),
            //  Todas las formas de decir «más» y «menos». En Linux gpui
            //  nombra las teclas por su keysym —`minus`, `equal`, `plus`— y
            //  no por el símbolo, y encima cada distribución pone el `+` en
            //  un sitio: registrarlas todas sale más barato que adivinar.
            KeyBinding::new("ctrl-plus", IncreaseFontSize, None),
            KeyBinding::new("ctrl-equal", IncreaseFontSize, None),
            KeyBinding::new("ctrl-shift-equal", IncreaseFontSize, None),
            KeyBinding::new("ctrl-+", IncreaseFontSize, None),
            KeyBinding::new("ctrl-=", IncreaseFontSize, None),
            KeyBinding::new("ctrl-minus", DecreaseFontSize, None),
            KeyBinding::new("ctrl--", DecreaseFontSize, None),
            KeyBinding::new("ctrl-0", ResetFontSize, None),
            KeyBinding::new("ctrl-shift-f", Find, None),
            //  Los bloques: saltar de prompt en prompt, llevarse la salida
            //  del último y apagar lo viejo.
            KeyBinding::new("ctrl-shift-up", PreviousBlock, None),
            KeyBinding::new("ctrl-shift-down", NextBlock, None),
            KeyBinding::new("ctrl-shift-e", CopyLastOutput, None),
            KeyBinding::new("ctrl-shift-q", ToggleQuiet, None),
            KeyBinding::new("ctrl-shift-n", SendBlockToNote, None),
            KeyBinding::new("ctrl-shift-m", SendSessionToNote, None),
            //  Devolverla a la isla, que es de donde vino o donde cabe mejor
            //  para vigilarla de reojo.
            KeyBinding::new("ctrl-shift-i", ToIsland, None),
        ]);

        //  La puerta a Edinot solo se abre si Edinot está. Quien no lo tenga
        //  no se entera de que existe: las teclas no hacen nada y nadie le da
        //  la lata con un error por algo que nunca pidió.
        if edinot::disponible() {
            gpui_ghostty_terminal::registrar_anotador(|titulo, texto| {
                thread::spawn(move || match edinot::anotar(&titulo, &texto) {
                    Ok(nota) => barra::decir("Guardado en Edinot", &nota),
                    Err(fallo) => barra::decir("No se pudo guardar en Edinot", &fallo),
                });
            });
        }

        //  El botón de ajustes solo existe si hay barra que los enseñe.
        if barra::hay_barra() {
            gpui_ghostty_terminal::registrar_ajustes(barra::abrir_ajustes);
        }

        let opciones = WindowOptions {
            // Con nombre y apellidos: sin app_id la ventana sale con clase
            // vacía y ni Hyprland ni nadie puede dirigirse a ella.
            app_id: Some("k4term".to_string()),
            titlebar: Some(TitlebarOptions {
                title: Some("k4term".into()),
                ..Default::default()
            }),
            //  Cristal esmerilado: lo de detrás se ve borroso, que es lo más
            //  macOS que hay. Si se pide opacidad total no se molesta al
            //  compositor con transparencias que no hacen falta.
            window_background: if ajustes.opacidad < 1.0 {
                gpui::WindowBackgroundAppearance::Blurred
            } else {
                gpui::WindowBackgroundAppearance::Opaque
            },
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

            //  Dos maneras de tener sesión: abrir una shell nuestra, o adoptar
            //  la que viene de la isla. Adoptar no es abrir otra igual — es
            //  que el shell de dentro, y el agente que esté pensando, y el
            //  `make` a medias, sigan sin enterarse de nada: lo que cambia de
            //  manos es el maestro del PTY, y a ellos lo que los ata es el
            //  esclavo, que no se toca.
            let heredada = args
                .heredar
                .as_deref()
                .and_then(|ruta| match traspaso::recoger(ruta) {
                    Ok(par) => Some(par),
                    Err(fallo) => {
                        eprintln!("k4term: no se pudo heredar la sesión: {fallo}");
                        None
                    }
                });

            let mut pty_reader: Box<dyn Read + Send>;
            let mut pty_writer: Box<dyn Write + Send>;
            //  Redimensionar es lo único que hace falta del PTY además de leer
            //  y escribir. Va en un `Rc` y no en un `Arc` porque solo lo llama
            //  la vista, que vive en el hilo de la interfaz — y porque el
            //  maestro de la biblioteca no se puede compartir entre hilos.
            let medir: std::rc::Rc<dyn Fn(u16, u16)>;
            //  El descriptor del maestro: lo que se pasa a la isla si un día
            //  esta sesión se muda.
            let fd_maestro: std::os::fd::RawFd;
            //  Con qué dejar la pantalla como estaba. Solo hay algo cuando la
            //  sesión viene de fuera: el VT del que la suelta no viaja.
            let mut pintura = String::new();

            match heredada {
                Some((maestro, equipaje)) => {
                    let lector = std::fs::File::from(
                        maestro.try_clone().expect("copia del maestro para leer"),
                    );
                    let escritor = std::fs::File::from(
                        maestro
                            .try_clone()
                            .expect("copia del maestro para escribir"),
                    );
                    pty_reader = Box::new(lector);
                    pty_writer = Box::new(escritor);
                    pintura = equipaje.pintura;
                    fd_maestro = maestro.as_raw_fd();

                    //  El descriptor se queda vivo dentro de este cierre: si
                    //  se soltara, el shell de dentro recibiría un cuelgue.
                    medir = std::rc::Rc::new(move |cols, rows| {
                        traspaso::medir(maestro.as_raw_fd(), cols, rows);
                    });
                }
                None => {
                    let pty_system = native_pty_system();
                    let pty_pair = pty_system
                        .openpty(PtySize {
                            rows: config.rows,
                            cols: config.cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .expect("no se pudo abrir el pty");

                    let master: Arc<dyn portable_pty::MasterPty + Send> =
                        Arc::from(pty_pair.master);

                    let mut cmd = match &args.ejecutar {
                        Some(resto) => {
                            let mut c = CommandBuilder::new(&resto[0]);
                            c.args(&resto[1..]);
                            c
                        }
                        None => {
                            let shell = ajustes.shell.clone().unwrap_or_else(|| {
                                std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
                            });
                            let es_fish = PathBuf::from(&shell)
                                .file_name()
                                .is_some_and(|nombre| nombre == "fish");
                            let mut c = CommandBuilder::new(shell);
                            // Fish 4.8 probes the terminal with XTGETTCAP, kitty
                            // keyboard and cursor-style queries before showing its
                            // greeting. k4term already provides the useful replies
                            // (DSR and OSC 10/11), but not every optional probe; fish
                            // otherwise waits several seconds for a timeout. Keep
                            // the normal xterm-256color capabilities while disabling
                            // only this startup probe for fish sessions.
                            if es_fish {
                                c.args(["--features", "no-query-term"]);
                            }
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

                    pty_reader = Box::new(master.try_clone_reader().expect("lector del pty"));
                    pty_writer = Box::new(master.take_writer().expect("escritor del pty"));
                    fd_maestro = master.as_raw_fd().unwrap_or(-1);

                    //  El maestro se queda vivo dentro del cierre: soltarlo
                    //  cerraría el descriptor y la shell recibiría un cuelgue.
                    let master_para_medir = master.clone();
                    medir = std::rc::Rc::new(move |cols, rows| {
                        let _ = master_para_medir.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    });
                }
            }

            //  La puerta de vuelta a la isla, que solo tiene sentido si hay
            //  isla: se le ofrece la sesión por un socket y se le dice a la
            //  barra dónde recogerla. La ventana se va en cuanto se la lleven
            //  —no antes—, que hasta ese momento la sesión sigue siendo suya.
            if barra::hay_barra() {
                gpui_ghostty_terminal::registrar_mudanza(move |pintura, titulo| {
                    let equipaje = k4term_puente::Equipaje {
                        titulo,
                        pintura,
                        ..Default::default()
                    };
                    match traspaso::ofrecer(fd_maestro, &equipaje) {
                        Ok((ruta, aviso)) => {
                            barra::adoptar(&ruta.to_string_lossy());
                            thread::spawn(move || {
                                if aviso.recv().unwrap_or(false) {
                                    std::process::exit(0);
                                }
                            });
                        }
                        Err(fallo) => barra::decir("No se pudo mover la sesión", &fallo),
                    }
                });
            }

            let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
            let (stdout_tx, stdout_rx) = mpsc::channel::<Vec<u8>>();

            //  Lo primero que ve el VT nuevo es la pantalla que traía la
            //  sesión: va por el mismo canal que el PTY porque para el
            //  terminal es lo mismo, bytes que pintar.
            if !pintura.is_empty() {
                let _ = stdout_tx.send(std::mem::take(&mut pintura).into_bytes());
            }

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
            let (campana_tx, campana_rx) = mpsc::channel::<()>();
            let (marcas_tx, marcas_rx) = mpsc::channel::<Marca>();
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
                                let _ = marcas_tx.send(Marca::Empieza);
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
                                let _ = marcas_tx.send(Marca::Acaba { salida });
                                let _ = avisos.send(Aviso::Acaba { salida });
                                mandato.clear();
                            }
                            //  La campana la ve el escáner y la enseña la
                            //  ventana: un destello corto, que un pitido a
                            //  las tres de la mañana no lo quiere nadie.
                            Suceso::Campana => {
                                let _ = campana_tx.send(());
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

                //  Se acabó el chorro: o la shell ha terminado, o el otro
                //  extremo se ha cerrado. Con una sesión heredada no hay hijo
                //  a quien esperar —lo adoptó el sistema al soltarla—, así que
                //  este es el único aviso de que aquí ya no queda nada.
                barra::limpiar(std::process::id());
                std::process::exit(0);
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
                vista.set_font(fuente(ajustes.fuente.clone()));
                vista.set_font_size(gpui::px(ajustes.tamano));
                vista.set_padding(gpui::px(ajustes.margen));
                vista.set_cristal(ajustes.opacidad, ajustes.radio);
                vista.set_estela(ajustes.estela);
                if ajustes.tranquilo {
                    vista.set_tranquilo(true, cx);
                }

                let medir_al_cambiar = medir.clone();
                vista.set_on_resize(move |cols, rows| medir_al_cambiar(cols, rows));

                vista
            });

            let temas = tema::vigilar(ruta_tema);
            let cambios = k4term_puente::ajustes::vigilar();
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
                        let campana = std::iter::from_fn(|| campana_rx.try_recv().ok()).count() > 0;
                        let marcas: Vec<Marca> =
                            std::iter::from_fn(|| marcas_rx.try_recv().ok()).collect();
                        let ajuste_nuevo = std::iter::from_fn(|| cambios.try_recv().ok()).last();

                        //  `apagando` mantiene vivo el bucle mientras dure el
                        //  destello: sin salida nueva nadie volvería a pedir
                        //  un pintado y el fogonazo se quedaría encendido.
                        let mut apagando = false;
                        cx.update(|_, cx| {
                            view_for_task.update(cx, |this, cx| {
                                apagando = this.campana_encendida(cx);
                                //  El cursor deslizándose también pide seguir
                                //  pintando aunque no llegue nada nuevo.
                                if this.animando() {
                                    apagando = true;
                                    cx.notify();
                                }
                            });
                        })
                        .ok();

                        if batch.is_empty()
                            && ultimo_tema.is_none()
                            && !campana
                            && !apagando
                            && marcas.is_empty()
                            && ajuste_nuevo.is_none()
                        {
                            continue;
                        }

                        let mut avisar = false;
                        let mut titulo = String::new();
                        cx.update(|ventana, cx| {
                            view_for_task.update(cx, |this, cx| {
                                if !batch.is_empty() {
                                    this.queue_output_bytes(&batch, cx);
                                }
                                if let Some(t) = ultimo_tema {
                                    this.set_default_colors(rgb(t.tinta), rgb(t.fondo), cx);
                                    this.set_seco(t.seco, cx);
                                }
                                //  Las marcas van DESPUÉS de tragar la salida:
                                //  el sitio donde empieza un mandato es el que
                                //  ocupa el cursor una vez pintado lo suyo.
                                for marca in &marcas {
                                    match marca {
                                        Marca::Empieza => this.empieza_bloque(cx),
                                        Marca::Acaba { salida } => this.acaba_bloque(*salida, cx),
                                    }
                                }
                                //  Ajustes cambiados en caliente: lo que se
                                //  toque en la barra se ve aquí sin reabrir.
                                if let Some(a) = &ajuste_nuevo {
                                    this.set_apariencia(
                                        Appearance {
                                            font: fuente(a.fuente.clone()),
                                            size: gpui::px(a.tamano),
                                            padding: gpui::px(a.margen),
                                            opacity: a.opacidad,
                                            radius: a.radio,
                                            trail: a.estela,
                                        },
                                        cx,
                                    );
                                }
                                if campana {
                                    this.tocar_campana(cx);
                                    //  Con la ventana delante ya lo estás
                                    //  viendo; el aviso es para cuando no.
                                    if !ventana.is_window_active() {
                                        //  El título lo pone el programa que
                                        //  corre: es lo más parecido a «quién
                                        //  te llama» que hay sin adivinar.
                                        avisar = true;
                                        titulo = this.titulo_actual();
                                    }
                                }
                            });
                        })
                        .ok();

                        if avisar {
                            barra::avisar_campana(std::process::id(), &titulo);
                        }
                    }
                })
                .detach();

            view
        })
        .unwrap();
    });
}
