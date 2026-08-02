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
//       "cursor":[col,fila],"cols":90,"filas_n":16}

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
}

#[derive(Serialize)]
struct Donde {
    que: &'static str,
    ruta: String,
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

    Marco {
        que: "marco",
        filas: salida,
        cursor: [col, fila],
        cols,
        filas_n: filas,
        usadas: ultima.max(fila),
        cwd,
    }
}

//  El dueño del VT. Nadie más lo toca: le llegan recados, decide cuándo la
//  pantalla está lo bastante quieta y manda la foto.
//  El directorio se mira al pintar y no en cada byte: es una lectura de
//  /proc, barata, pero no hay por qué hacerla cincuenta veces por `ls`.
fn motor(rx: std::sync::mpsc::Receiver<Recado>, pid_shell: u32) {
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

    let (tx, rx) = channel::<Recado>();
    thread::spawn(move || motor(rx, pid_shell));

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
    //  Este hilo es el único dueño del master y del escritor, que tampoco se
    //  pueden compartir. Si la barra se va, nos vamos con ella: quedarse sería
    //  dejar una shell huérfana por ahí.
    thread::spawn(move || {
        let entrada = std::io::stdin();
        for linea in entrada.lock().lines() {
            let Ok(linea) = linea else { break };
            let Ok(orden) = serde_json::from_str::<Orden>(&linea) else {
                continue;
            };

            match orden {
                Orden::Texto { valor } => {
                    let _ = escritor.write_all(valor.as_bytes());
                    let _ = escritor.flush();
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
                        let _ = escritor.write_all(&bytes);
                        let _ = escritor.flush();
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
