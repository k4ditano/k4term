//  k4term-isla — la sesión que vive dentro de la barra.
//
//  Es la misma terminal de siempre menos la ventana: PTY y VT de ghostty,
//  pero en vez de pintar, sirve la rejilla por la salida estándar en JSON por
//  líneas y recibe teclas por la entrada. Ese es el idioma que la barra ya
//  habla con sus vigías, así que no hace falta socket ni protocolo nuevo: el
//  plugin lo arranca con K4.Process y listo.
//
//  Quien manda aquí es el VT: la barra no interpreta nada, solo pinta lo que
//  se le da y devuelve teclas. Y como el proceso vive mientras viva la barra,
//  la sesión persiste — cierras la vista y tu shell sigue ahí, con su
//  directorio y su historial.
//
//  El reparto es de actores y no de cerrojos, y no por gusto: el terminal de
//  ghostty no se puede mandar de un hilo a otro, así que lo tiene UN hilo y
//  los demás le hablan por su buzón.
//
//      lector  ──bytes──▶  motor (dueño del VT)  ──marcos──▶  salida
//      entrada ──medidas─▶     ▲
//              ──teclas──────▶ PTY
//
//  Ordenes que entiende (una por línea):
//
//      {"que":"texto","valor":"ls -la"}          escribe eso tal cual
//      {"que":"tecla","nombre":"enter"}          tecla con nombre (+ mods)
//      {"que":"medida","cols":90,"filas":16}     redimensiona
//      {"que":"pinta"}                           manda un marco entero ya
//
//  Y lo que responde:
//
//      {"que":"marco","filas":[[{"t":"…","f":"#rrggbb","b":"#rrggbb","n":0}]],
//       "cursor":[col,fila],"cols":90,"filas_n":16,"scroll":[arriba,total]}

use std::io::{BufRead, Read, Write};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use ghostty_vt::{KeyModifiers, Rgb, Terminal, encode_key_named};
use k4term_puente::tema;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};

const COLS: u16 = 90;
const FILAS: u16 = 16;

//  Lo que se espera a que la pantalla se calme antes de mandar la foto: la
//  salida de un `ls` llega en cincuenta trozos y mandar cincuenta marcos
//  sería tirar el trabajo cincuenta veces.
const REPOSO: Duration = Duration::from_millis(30);
//  Y lo que se aguanta sin mandar nada cuando algo escupe sin parar: con esto
//  un `yes` se ve correr en vez de quedarse en blanco.
const MAXIMO: Duration = Duration::from_millis(120);

#[derive(Deserialize)]
#[serde(tag = "que")]
enum Orden {
    #[serde(rename = "texto")]
    Texto { valor: String },
    #[serde(rename = "tecla")]
    Tecla {
        nombre: String,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        control: bool,
        #[serde(default)]
        alt: bool,
    },
    #[serde(rename = "medida")]
    Medida { cols: u16, filas: u16 },
    #[serde(rename = "pinta")]
    Pinta,
    #[serde(rename = "donde")]
    Donde,
    #[serde(rename = "rueda")]
    Rueda { lineas: i32 },
}

enum Recado {
    Bytes(Vec<u8>),
    Medida { cols: u16, filas: u16 },
    Pinta,
    Donde,
    Rueda { lineas: i32 },
    Ajustes(Box<k4term_puente::Ajustes>),
}

#[derive(Serialize)]
struct Donde {
    que: &'static str,
    ruta: String,
}

//  Lo que la vista necesita saber de los ajustes de k4term. Se le manda desde
//  aquí en vez de dejar que la barra lea el fichero por su cuenta: los
//  ajustes son de la terminal, y con dos lectores acabaría habiendo dos
//  formatos y una ventana y una isla que no se parecen.
#[derive(Serialize)]
struct Config {
    que: &'static str,
    estela: u8,
    //  Y con qué letra se pinta. No es un detalle de gusto: la barra dibuja
    //  una REJILLA, y el ancho de celda lo saca de medir la fuente. Si mide
    //  con una y pinta con otra —o si mide con la variante de iconos, que los
    //  lleva a doble ancho— la celda no vale lo que ocupa un carácter y el
    //  cursor se va separando del texto hacia la derecha. Que venga de aquí es
    //  lo que garantiza que la isla y la ventana usan la misma.
    fuente: String,
    tamano: f32,
}

//  Dónde está la sesión ahora mismo. Se le pregunta al kernel por el
//  directorio de la shell en vez de pedirle integración a nadie: el proceso
//  es hijo nuestro y su cwd está ahí para quien quiera mirarlo.
fn directorio(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|r| r.to_string_lossy().into_owned())
}

#[derive(Serialize)]
struct Tramo {
    t: String,
    f: String,
    b: String,
    //  Negrita y compañía, tal como los da el VT: la barra decide qué hace
    //  con ellos.
    n: u8,
    //  La COLUMNA por la que empieza, contando desde 1.
    //
    //  Sin esto la barra encadenaba los tramos uno detrás de otro con el ancho
    //  natural de su texto, y un terminal no es texto encadenado: es una
    //  rejilla de celdas iguales. En cuanto aparecía un glifo que no mide lo
    //  mismo que los demás —los marcos de las cajas, un icono de la Nerd Font,
    //  un espacio duro— la línea se iba desplazando a la derecha respecto del
    //  cursor, que sí se coloca por columna. Con la columna a cuestas, cada
    //  tramo se ancla donde le toca y el error no se acumula.
    c: u16,
}

#[derive(Serialize)]
struct Marco {
    que: &'static str,
    filas: Vec<Vec<Tramo>>,
    cursor: [u16; 2],
    cols: u16,
    filas_n: u16,
    //  Hasta dónde llega lo escrito: con esto la isla puede crecer con el
    //  contenido en vez de reservar siempre el mismo cajón.
    usadas: u16,
    //  Dónde está la sesión, para que el pie diga algo útil.
    cwd: String,
    //  [fila por la que empieza lo que se ve, filas que hay en total], en
    //  coordenadas del historial entero. La rejilla no es un Flickable —el
    //  historial vive aquí, no en la barra—, así que sin estos dos números la
    //  vista no tiene con qué dibujar la barrita de la casa.
    scroll: [u32; 2],
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

//  Una fila en tramos de color. El texto se corta por COLUMNAS y no por
//  bytes: una tilde ocupa dos bytes y una columna, y confundirlos la parte.
//
//  Del final se quita lo que no dice nada —espacios con el fondo de siempre—,
//  que en una pantalla de terminal es casi todo. Los espacios CON color se
//  quedan: son la barra de estado de vim o una selección, y sin ellos la
//  pantalla mentiría.
fn fila_en_tramos(term: &Terminal, fila: u16, fondo: &str) -> Vec<Tramo> {
    let texto = term.dump_viewport_row(fila).unwrap_or_default();
    let columnas: Vec<char> = texto.chars().collect();
    let runs = term.dump_viewport_row_style_runs(fila).unwrap_or_default();

    let mut tramos = Vec::new();
    for run in runs {
        //  El VT cuenta columnas desde 1 y con los dos extremos dentro; aquí
        //  se indexa desde 0 y con el final fuera. Confundirlos se come la
        //  primera letra de cada tramo — «cho» en vez de «echo».
        let ini = run.start_col.saturating_sub(1) as usize;
        let fin = (run.end_col as usize).min(columnas.len());
        if ini >= fin {
            continue;
        }
        tramos.push(Tramo {
            t: columnas[ini..fin].iter().collect(),
            f: hex(run.fg),
            b: hex(run.bg),
            n: run.flags,
            c: run.start_col.max(1),
        });
    }

    while tramos
        .last()
        .is_some_and(|x| x.t.trim().is_empty() && x.b == fondo)
    {
        tramos.pop();
    }
    tramos
}

//  Ojo a las dos numeraciones, que no son la misma y muerden: las filas del
//  volcado empiezan en 0, y el cursor viene en 1. Volcar desde 1 se comía la
//  primera fila y dejaba el cursor pintado una línea por debajo de donde de
//  verdad escribes.
fn marco(term: &Terminal, cols: u16, filas: u16, fondo: &str, cwd: String) -> Marco {
    let mut salida = Vec::with_capacity(filas as usize);
    for f in 0..filas {
        salida.push(fila_en_tramos(term, f, fondo));
    }
    let (col, fila) = term.cursor_position().unwrap_or((1, 1));

    //  La última fila con algo escrito, o donde esté el cursor si va por
    //  debajo: es lo que de verdad ocupa la sesión ahora mismo.
    let ultima = salida
        .iter()
        .rposition(|f| !f.is_empty())
        .map(|i| i as u16 + 1)
        .unwrap_or(0);

    //  Si el VT no sabe decirlo, se finge que no hay historial: total igual a
    //  lo que se ve deja la barrita entera, que es como no tenerla.
    let (arriba, total) = term.viewport_position().unwrap_or((0, u32::from(filas)));

    Marco {
        que: "marco",
        filas: salida,
        cursor: [col, fila],
        cols,
        filas_n: filas,
        usadas: ultima.max(fila),
        cwd,
        scroll: [arriba, total.max(u32::from(filas))],
    }
}

fn decir_config(salida: &std::io::Stdout, ajustes: &k4term_puente::Ajustes) {
    if let Ok(json) = serde_json::to_string(&Config {
        que: "config",
        estela: ajustes.estela,
        fuente: ajustes.fuente.clone(),
        tamano: ajustes.tamano,
    }) {
        let mut s = salida.lock();
        let _ = writeln!(s, "{}", json);
        let _ = s.flush();
    }
}

//  El dueño del VT. Nadie más lo toca: le llegan recados, decide cuándo la
//  pantalla está lo bastante quieta y manda la foto.
//  El directorio se mira al pintar y no en cada byte: es una lectura de
//  /proc, barata, pero no hay por qué hacerla cincuenta veces por `ls`.
fn motor(rx: std::sync::mpsc::Receiver<Recado>, pid_shell: u32, al_pty: Sender<Vec<u8>>) {
    let mut ultimo_cwd = String::new();
    let mut term = match Terminal::new(COLS, FILAS) {
        Ok(t) => t,
        Err(_) => return,
    };

    let mut fondo = Rgb { r: 0, g: 0, b: 0 };
    if let Some(t) = tema::leer(&tema::ruta_por_defecto()) {
        fondo = Rgb {
            r: t.fondo.r,
            g: t.fondo.g,
            b: t.fondo.b,
        };
        term.set_default_colors(
            Rgb {
                r: t.tinta.r,
                g: t.tinta.g,
                b: t.tinta.b,
            },
            fondo,
        );
    }
    let fondo = hex(fondo);

    let mut cols = COLS;
    let mut filas = FILAS;
    let mut sucio = false;
    let mut desde: Option<Instant> = None;
    let salida = std::io::stdout();

    //  Los ajustes van por delante del primer marco: si llegaran después, la
    //  vista pintaría el primer cursor con la estela de fábrica y la
    //  corregiría a la vista del usuario.
    decir_config(&salida, &k4term_puente::Ajustes::leer());

    loop {
        let recado = if sucio {
            rx.recv_timeout(REPOSO)
        } else {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };

        //  «Quieto» es que ha vencido el reposo sin llegar nada; se anota
        //  antes de consumir el recado, que después ya no se puede mirar.
        let quieto = matches!(recado, Err(RecvTimeoutError::Timeout));

        match recado {
            Ok(Recado::Bytes(b)) => {
                let _ = term.feed(&b);
                //  Y lo que haya que contestar, de vuelta por el PTY. Va
                //  pegado al feed porque quien pregunta está esperando: un
                //  TUI que consulta el terminal y no recibe respuesta se
                //  coloca a ciegas.
                let respuestas = term.take_responses();
                if !respuestas.is_empty() {
                    let _ = al_pty.send(respuestas);
                }
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Medida { cols: c, filas: f }) => {
                if (c, f) != (cols, filas) {
                    let _ = term.resize(c, f);
                    cols = c;
                    filas = f;
                }
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Pinta) => {
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Rueda { lineas }) => {
                let _ = term.scroll_viewport(lineas);
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Ajustes(ajustes)) => {
                decir_config(&salida, &ajustes);
            }
            Ok(Recado::Donde) => {
                let ruta = directorio(pid_shell).unwrap_or_default();
                if let Ok(json) = serde_json::to_string(&Donde { que: "donde", ruta }) {
                    let mut s = salida.lock();
                    let _ = writeln!(s, "{}", json);
                    let _ = s.flush();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let vencido = desde.is_some_and(|d| d.elapsed() >= MAXIMO);
        if sucio && (quieto || vencido) {
            if let Some(d) = directorio(pid_shell) {
                ultimo_cwd = d;
            }
            if let Ok(json) =
                serde_json::to_string(&marco(&term, cols, filas, &fondo, ultimo_cwd.clone()))
            {
                let mut s = salida.lock();
                if writeln!(s, "{}", json).is_err() || s.flush().is_err() {
                    break;
                }
            }
            sucio = false;
            desde = None;
        }
    }
}

fn main() {
    let pty = native_pty_system();
    let par = pty
        .openpty(PtySize {
            rows: FILAS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("no se pudo abrir el pty");

    let master = par.master;
    let mut lector = master.try_clone_reader().expect("lector del pty");
    let mut escritor = master.take_writer().expect("escritor del pty");

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(shell);
    cmd.arg("-l");
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "k4term");
    if let Ok(home) = std::env::var("HOME") {
        cmd.cwd(home);
    }

    let mut hijo = par.slave.spawn_command(cmd).expect("no arrancó la shell");
    let pid_shell = hijo.process_id().unwrap_or(0);

    // ── el que escribe en el PTY ──────────────────────────────────
    //
    //  Un hilo con el escritor y nada más, porque ahora le escriben DOS: las
    //  teclas que llegan de la barra y las respuestas que el terminal debe dar
    //  a quien le pregunta. El escritor no se puede compartir, así que en vez
    //  de repartirlo se le pone buzón.
    let (al_pty, del_buzon) = channel::<Vec<u8>>();
    thread::spawn(move || {
        while let Ok(bytes) = del_buzon.recv() {
            if escritor.write_all(&bytes).is_err() || escritor.flush().is_err() {
                break;
            }
        }
    });

    let (tx, rx) = channel::<Recado>();
    {
        let al_pty = al_pty.clone();
        thread::spawn(move || motor(rx, pid_shell, al_pty));
    }

    // ── los ajustes, en caliente ──────────────────────────────────
    //
    //  El mismo fichero que la ventana y vigilado igual, para que tocar la
    //  estela una vez se vea en las dos. Sin esto la isla se quedaría con lo
    //  que hubiera al arrancar la barra, que puede ser de hace días.
    {
        let tx: Sender<Recado> = tx.clone();
        thread::spawn(move || {
            let rx_ajustes = k4term_puente::ajustes::vigilar();
            while let Ok(ajustes) = rx_ajustes.recv() {
                if tx.send(Recado::Ajustes(Box::new(ajustes))).is_err()
                {
                    break;
                }
            }
        });
    }

    // ── el PTY habla ──────────────────────────────────────────────
    {
        let tx: Sender<Recado> = tx.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let n = match lector.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if tx.send(Recado::Bytes(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }

    // ── la barra manda ────────────────────────────────────────────
    //
    //  Este hilo es el único dueño del master, que no se puede compartir. Lo
    //  que se escribe va por el buzón del PTY, que es de otro. Si la barra se
    //  va, nos vamos con ella: quedarse sería dejar una shell huérfana.
    thread::spawn(move || {
        let entrada = std::io::stdin();
        for linea in entrada.lock().lines() {
            let Ok(linea) = linea else { break };
            let Ok(orden) = serde_json::from_str::<Orden>(&linea) else {
                continue;
            };

            match orden {
                Orden::Texto { valor } => {
                    let _ = al_pty.send(valor.into_bytes());
                }
                Orden::Tecla {
                    nombre,
                    shift,
                    control,
                    alt,
                } => {
                    let mods = KeyModifiers {
                        shift,
                        control,
                        alt,
                        super_key: false,
                    };
                    if let Some(bytes) = encode_key_named(&nombre, mods) {
                        let _ = al_pty.send(bytes);
                    }
                }
                Orden::Medida { cols, filas } => {
                    let cols = cols.max(20);
                    let filas = filas.max(4);
                    let _ = master.resize(PtySize {
                        rows: filas,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                    let _ = tx.send(Recado::Medida { cols, filas });
                }
                Orden::Pinta => {
                    let _ = tx.send(Recado::Pinta);
                }
                Orden::Donde => {
                    let _ = tx.send(Recado::Donde);
                }
                Orden::Rueda { lineas } => {
                    let _ = tx.send(Recado::Rueda { lineas });
                }
            }
        }
        std::process::exit(0);
    });

    let _ = hijo.wait();
}
