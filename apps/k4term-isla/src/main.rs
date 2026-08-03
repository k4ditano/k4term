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
//      {"que":"pegar","valor":"…"}               pega (con corchetes si toca)
//      {"que":"raton","tipo":"pulsar","boton":"izquierdo","col":1,"fila":1}
//      {"que":"rueda","lineas":-3,"col":1,"fila":1}
//      {"que":"texto_de","desde":10,"hasta":0}   historial en texto plano
//      {"que":"saltar","hacia":-1}               al prompt anterior
//      {"que":"buscar","texto":"error","hacia":-1}
//      {"que":"nota","entera":false}             a la nota del día, si hay
//
//  Y lo que responde:
//
//      {"que":"marco","filas":[[{"t":"…","f":"#rrggbb","b":"#rrggbb","n":0}]],
//       "cursor":[col,fila],"cols":90,"filas_n":16,"scroll":[arriba,total],
//       "raton":false,"bloques":[{"fila":120,"estado":"bien","fin":126}],
//       "ultimo":{"fila":120,"estado":"bien","fin":126}}
//      {"que":"portapapeles","texto":"…"}        lo copió la aplicación
//      {"que":"texto","texto":"…","motivo":"…"}  lo pedido con `texto_de`
//      {"que":"buscado","hay":true,"fila":97}    dónde cayó la búsqueda
//      {"que":"aviso","texto":"…"}               un recado para el usuario

use std::io::{BufRead, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use ghostty_vt::{
    Boton, Figura, KeyModifiers, ModeTracker, Rgb, SucesoRaton, Terminal, encode_key_named,
    encode_mouse,
};
use k4term_puente::{Aviso, Suceso, edinot, osc::Escaner, tema, trabajos, traspaso};
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
    Rueda {
        lineas: i32,
        //  Dónde estaba el puntero. Solo hace falta cuando la aplicación
        //  quiere el ratón: entonces la rueda es suya y no del historial.
        #[serde(default)]
        col: u16,
        #[serde(default)]
        fila: u16,
        //  «Esto es MÍO»: mover el historial aunque la aplicación tenga el
        //  ratón puesto. Es lo que pasa con shift, la salida de emergencia de
        //  toda la vida para mirar hacia atrás dentro de un programa.
        #[serde(default)]
        historial: bool,
    },
    //  Al principio o al final del historial de una vez.
    #[serde(rename = "tope")]
    Tope { arriba: bool },
    //  Pegar es distinto de escribir: puede llevar corchetes alrededor si la
    //  aplicación los pidió, y eso solo lo sabe quien mira el chorro.
    #[serde(rename = "pegar")]
    Pegar { valor: String },
    #[serde(rename = "raton")]
    Raton {
        tipo: String,
        #[serde(default)]
        boton: String,
        col: u16,
        fila: u16,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        control: bool,
        #[serde(default)]
        alt: bool,
    },
    //  Qué hay escrito de tal fila a tal fila del historial, para copiar.
    //
    //  Las columnas recortan la primera y la última línea —que es lo que
    //  distingue una selección de arrastre de «estas filas enteras»—, y el
    //  motivo vuelve con la respuesta: la barra pide texto para copiar, para
    //  una nota o para el portapapeles, y la contestación tiene que decir
    //  cuál de las tres era.
    #[serde(rename = "texto_de")]
    TextoDe {
        desde: u32,
        hasta: u32,
        #[serde(default)]
        col_desde: u16,
        #[serde(default)]
        col_hasta: u16,
        #[serde(default)]
        motivo: String,
    },
    //  Al prompt anterior (-1) o al siguiente (1).
    #[serde(rename = "saltar")]
    Saltar { hacia: i32 },
    //  El color del sitio donde estás. Vacío lo quita.
    #[serde(rename = "tinte")]
    Tinte { color: String },
    //  Plegar o desplegar la salida de un mandato, por la fila del historial
    //  donde empieza.
    #[serde(rename = "plegar")]
    Plegar { fila: u32 },
    //  Soltar la sesión para que se la lleve una ventana. Lo que corre dentro
    //  no se entera: lo que cambia de manos es el maestro del PTY.
    #[serde(rename = "emigrar")]
    Emigrar,
    //  Buscar en el historial desde donde se está mirando. Hacia atrás (-1) o
    //  hacia delante (1).
    #[serde(rename = "buscar")]
    Buscar { texto: String, hacia: i32 },
    //  A la nota del día: el último mandato con su salida, o la sesión entera.
    #[serde(rename = "nota")]
    Nota { entera: bool },
}

enum Recado {
    Bytes(Vec<u8>),
    Medida {
        cols: u16,
        filas: u16,
    },
    Pinta,
    Donde,
    Rueda {
        lineas: i32,
        col: u16,
        fila: u16,
        historial: bool,
    },
    Tope {
        arriba: bool,
    },
    Pegar(String),
    Raton(Clic),
    TextoDe {
        desde: u32,
        hasta: u32,
        col_desde: u16,
        col_hasta: u16,
        motivo: String,
    },
    //  Al prompt anterior o al siguiente. Lo resuelve el motor porque es el
    //  único que tiene las marcas y el historial en las mismas coordenadas.
    Saltar {
        hacia: i32,
    },
    Tinte {
        color: String,
    },
    Plegar {
        fila: u32,
    },
    Emigrar,
    Buscar {
        texto: String,
        hacia: i32,
    },
    Nota {
        entera: bool,
    },
    //  Un mandato empieza o termina, visto en el chorro. Llega por el mismo
    //  canal que los bytes y DESPUÉS que ellos, así que el cursor ya está
    //  donde toca cuando se apunta la marca.
    Marca {
        empieza: bool,
        salida: i32,
        mandato: String,
    },
    Ajustes(Box<k4term_puente::Ajustes>),
}

//  Un clic ya traducido: la barra habla de «izquierdo» y «pulsar», el
//  terminal de números y letras.
struct Clic {
    suceso: SucesoRaton,
    boton: Boton,
    col: u16,
    fila: u16,
    mods: KeyModifiers,
}

//  Lo que se está cociendo aquí dentro, y quién te llama.
//
//  Va por la salida estándar y no por el IPC de la barra —que es lo que hace
//  la ventana— porque esta sesión YA tiene un canal abierto con ella. Y de
//  paso resuelve lo que el IPC no podía: la barra sabe de QUÉ sesión viene,
//  así que pulsar el indicador puede traerte a esta terminal y no a una
//  ventana que no existe.
#[derive(Serialize)]
struct Trabajo {
    que: &'static str,
    estado: &'static str,
    mandato: String,
    salida: i32,
    segundos: u64,
}

//  Alguien ha tocado la campana. Si eso merece aviso lo decide la barra, que
//  es la única que sabe si estás mirando esta terminal ahora mismo.
#[derive(Serialize)]
struct Campana {
    que: &'static str,
    titulo: String,
}

#[derive(Serialize)]
struct Donde {
    que: &'static str,
    ruta: String,
}

//  Lo que la aplicación de dentro ha pedido copiar (OSC 52). Aquí no hay
//  portapapeles —esto no es una ventana—, así que se le pasa a la barra, que
//  sí lo tiene y además lleva el historial de copias de la casa.
#[derive(Serialize)]
struct Portapapeles {
    que: &'static str,
    texto: String,
}

//  «Ahí tienes la sesión»: por ese socket se la puede llevar quien quiera.
#[derive(Serialize)]
struct Emigrando {
    que: &'static str,
    socket: String,
}

//  Un recado suelto para el usuario: la terminal de la isla no tiene dónde
//  decir «guardado» sin taparse a sí misma, así que lo dice la barra.
#[derive(Serialize)]
struct AvisoDicho {
    que: &'static str,
    texto: String,
}

//  Un trozo del historial en texto plano, que es lo que se copia y lo que se
//  manda a una nota. Se pide por filas del historial y no del hueco visible:
//  lo que se ve cambia de sitio en cuanto sigue saliendo salida.
#[derive(Serialize)]
struct Texto {
    que: &'static str,
    texto: String,
    motivo: String,
}

//  El resultado de una búsqueda: por qué fila del historial andaba y si
//  había algo. Lo que se pinta amarillo lo resuelve la vista con lo que ve;
//  aquí solo se contesta a dónde ir, que es lo que hace falta el historial.
#[derive(Serialize)]
struct Buscado {
    que: &'static str,
    hay: bool,
    fila: u32,
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
    //  El enlace de OSC 8, si esa celda lo lleva. Es el de verdad —el que la
    //  aplicación escondió detrás del texto—, no el que se adivina mirando si
    //  algo parece una dirección.
    #[serde(skip_serializing_if = "Option::is_none")]
    u: Option<String>,
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
    //  A qué fila del historial corresponde cada fila pintada. Con las salidas
    //  recogidas la rejilla ya no es un calco del hueco visible, así que esta
    //  correspondencia es lo único que permite seleccionar, copiar y saber qué
    //  bloque hay debajo del ratón.
    filas_abs: Vec<u32>,
    //  Y cuáles de esas filas son la línea de una salida recogida, para que la
    //  barra las pinte como lo que son y sepa que al pulsarlas se despliegan.
    resumidas: Vec<u16>,
    cursor: [u16; 2],
    //  La forma que pide el programa de dentro (DECSCUSR): bloque, subrayado
    //  o barra, y si parpadea. Vim la usa para decirte en qué modo estás sin
    //  escribirlo en ningún sitio.
    cursor_figura: &'static str,
    cursor_parpadea: bool,
    cols: u16,
    filas_n: u16,
    //  Hasta dónde llega lo escrito: con esto la isla puede crecer con el
    //  contenido en vez de reservar siempre el mismo cajón.
    usadas: u16,
    //  Dónde está la sesión, para que el pie diga algo útil.
    cwd: String,
    //  Y cómo se llama lo que corre dentro, si lo ha dicho. Con varias
    //  sesiones abiertas es lo que deja distinguir «claude» de «codex» en el
    //  selector, en vez de «terminal 1» y «terminal 2».
    titulo: String,
    //  [fila por la que empieza lo que se ve, filas que hay en total], en
    //  coordenadas del historial entero. La rejilla no es un Flickable —el
    //  historial vive aquí, no en la barra—, así que sin estos dos números la
    //  vista no tiene con qué dibujar la barrita de la casa.
    scroll: [u32; 2],
    //  Si la aplicación de dentro quiere el ratón. Con esto puesto, arrastrar
    //  es SUYO —htop cambiando de columna, vim seleccionando— y la vista no
    //  puede quedarse el arrastre para hacer su propia selección; sin ello, al
    //  revés. Que lo diga el marco evita que la barra tenga que adivinarlo.
    raton: bool,
    //  Los bloques que se ven ahora mismo: por qué fila del historial empieza
    //  cada mandato y cómo acabó. Es lo que pinta el filete del margen y lo
    //  que permite saltar de un prompt al siguiente.
    bloques: Vec<Bloque>,
    //  Y el último, se vea o no. Copiar la salida del mandato anterior tiene
    //  que funcionar también cuando el prompt ya se ha ido por arriba, que es
    //  justo cuando hace falta.
    ultimo: Option<Bloque>,
}

//  Un mandato, en coordenadas del historial. El estado es el del filete:
//  «corre» mientras no ha terminado, «bien» o «mal» según el código de salida.
#[derive(Serialize, Clone)]
struct Bloque {
    fila: u32,
    estado: &'static str,
    //  Dónde terminó. Mientras corre no se sabe, y vale la fila de arranque.
    fin: u32,
    //  Qué se escribió. Es lo que se lee en la línea de un bloque plegado:
    //  «300 líneas» a secas no dice de qué.
    mandato: String,
    //  Si su salida está recogida ahora mismo.
    plegado: bool,
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
fn fila_en_tramos(term: &Terminal, fila: u16, fondo: &str, enlaces: bool) -> Vec<Tramo> {
    let texto = term.dump_viewport_row(fila).unwrap_or_default();
    let columnas: Vec<char> = texto.chars().collect();
    let runs = term.dump_viewport_row_style_runs(fila).unwrap_or_default();

    //  Los enlaces se piden por fila entera y solo si esta sesión ha visto
    //  alguno: un OSC 8 no tiene por qué traer estilo propio —se puede colgar
    //  de un texto que se pinta igual que el de al lado—, así que no hay
    //  ningún cambio de color del que tirar para encontrarlo.
    let enlaces_fila = if enlaces {
        term.row_hyperlinks(fila)
    } else {
        Vec::new()
    };

    let mut tramos = Vec::new();
    for run in runs {
        //  El VT cuenta columnas desde 1 y con los dos extremos dentro; aquí
        //  se indexa desde 0 y con el final fuera. Confundirlos se come la
        //  primera letra de cada tramo — «cho» en vez de «echo».
        let ini = run.start_col.max(1);
        let fin = u16::try_from(columnas.len())
            .unwrap_or(u16::MAX)
            .min(run.end_col);
        if ini > fin {
            continue;
        }

        //  Y dentro del tramo de color, se corta otra vez por donde empieza y
        //  acaba cada enlace: el color dice cómo se pinta y el enlace a dónde
        //  lleva, y no tienen por qué coincidir.
        let mut col = ini;
        while col <= fin {
            let enlace = enlaces_fila
                .iter()
                .find(|e| col >= e.start_col && col <= e.end_col);

            let hasta = match enlace {
                Some(e) => e.end_col.min(fin),
                None => enlaces_fila
                    .iter()
                    .filter(|e| e.start_col > col)
                    .map(|e| e.start_col - 1)
                    .min()
                    .unwrap_or(fin)
                    .min(fin),
            };

            let desde_idx = usize::from(col - 1);
            let hasta_idx = usize::from(hasta).min(columnas.len());
            if desde_idx < hasta_idx {
                tramos.push(Tramo {
                    t: columnas[desde_idx..hasta_idx].iter().collect(),
                    f: hex(run.fg),
                    b: hex(run.bg),
                    n: run.flags,
                    u: enlace.map(|e| e.uri.clone()),
                    c: col,
                });
            }

            col = hasta + 1;
        }
    }

    while tramos
        .last()
        .is_some_and(|x| x.t.trim().is_empty() && x.b == fondo && x.u.is_none())
    {
        tramos.pop();
    }
    tramos
}

//  Ojo a las dos numeraciones, que no son la misma y muerden: las filas del
//  volcado empiezan en 0, y el cursor viene en 1. Volcar desde 1 se comía la
//  primera fila y dejaba el cursor pintado una línea por debajo de donde de
//  verdad escribes.
//  Por qué fila del historial va el cursor. Es la coordenada en la que se
//  apuntan las marcas de los mandatos: la única que sobrevive a que la salida
//  siga subiendo y lo visible se desplace debajo.
fn fila_absoluta(term: &Terminal) -> u32 {
    let (arriba, _) = term.viewport_position().unwrap_or((0, 0));
    let (_, fila) = term.cursor_position().unwrap_or((1, 1));
    arriba + u32::from(fila.saturating_sub(1))
}

//  ¿Está eso en esta fila? En minúsculas las dos partes: quien busca en una
//  terminal no quiere pelearse con las mayúsculas.
fn fila_contiene(term: &Terminal, fila: u32, aguja: &str) -> bool {
    term.dump_screen_row(fila)
        .is_some_and(|l| l.to_lowercase().contains(aguja))
}

//  El historial en texto plano, de una fila a otra, ambas dentro. `hasta` en
//  cero significa «hasta donde llegue»: quien copia una sesión entera no tiene
//  por qué saber cuántas filas hay.
fn texto_de(term: &Terminal, desde: u32, hasta: u32, col_desde: u16, col_hasta: u16) -> String {
    let (_, total) = term.viewport_position().unwrap_or((0, 0));
    let fin = if hasta == 0 { total } else { hasta.min(total) };

    let mut lineas: Vec<String> = Vec::new();
    let mut fila = desde;
    while fila <= fin {
        match term.dump_screen_row(fila) {
            Some(l) => lineas.push(l.trim_end().to_string()),
            None => break,
        }
        fila += 1;
    }

    //  Las líneas en blanco del final son el hueco que quedaba por debajo, no
    //  parte de lo que se copia.
    while lineas.last().is_some_and(|l| l.is_empty()) {
        lineas.pop();
    }

    //  El recorte por columnas es lo que hace que una selección de arrastre
    //  sea lo que se ve y no las filas enteras. Se corta por CARACTERES y no
    //  por bytes: una tilde ocupa dos bytes y una sola columna.
    //
    //  Y con una sola línea hay que cortar por los dos lados A LA VEZ: hacerlo
    //  como en el caso de varias —quitar por delante en la primera, recortar
    //  por detrás en la última— se lleva por delante el trozo bueno, porque
    //  aquí la primera y la última son la misma.
    let ini = usize::from(col_desde.saturating_sub(1));
    let fin = if col_hasta == 0 {
        usize::MAX
    } else {
        usize::from(col_hasta)
    };

    if lineas.len() == 1 {
        if let Some(unica) = lineas.first_mut() {
            *unica = unica.chars().take(fin).skip(ini).collect::<String>();
        }
    } else {
        if let Some(primera) = lineas.first_mut()
            && ini > 0
        {
            *primera = primera.chars().skip(ini).collect::<String>();
        }
        if let Some(ultima) = lineas.last_mut()
            && col_hasta > 0
        {
            *ultima = ultima.chars().take(fin).collect::<String>();
        }
    }

    lineas.join("\n")
}

//  El bloque plegado al que pertenece esa fila del historial, si hay alguno.
//  Los que corren no se pliegan: recoger algo que todavía está saliendo sería
//  esconder justo lo que estás mirando.
fn plegado_en(bloques: &[Bloque], fila: u32) -> Option<&Bloque> {
    bloques
        .iter()
        .find(|b| b.plegado && b.fin > b.fila && fila >= b.fila && fila < b.fin)
}

//  La línea que sustituye a una salida recogida. Se compone a mano —no sale
//  del VT, que no sabe nada de esto— y se pinta como el resto: un tramo con
//  su color y su columna.
fn resumen(bloque: &Bloque, fondo: &str, apagado: &str) -> Vec<Tramo> {
    let cuantas = bloque.fin.saturating_sub(bloque.fila);
    let mandato = bloque.mandato.trim();
    let recortado: String = if mandato.chars().count() > 46 {
        format!("{}…", mandato.chars().take(45).collect::<String>())
    } else {
        mandato.to_string()
    };

    let texto = if recortado.is_empty() {
        format!("▸ {cuantas} líneas")
    } else {
        format!("▸ {recortado} · {cuantas} líneas")
    };

    vec![Tramo {
        t: texto,
        f: apagado.to_string(),
        b: fondo.to_string(),
        n: 0,
        u: None,
        c: 1,
    }]
}

#[allow(clippy::too_many_arguments)]
fn marco(
    term: &Terminal,
    cols: u16,
    filas: u16,
    fondo: &str,
    apagado: &str,
    cwd: String,
    raton: bool,
    enlaces: bool,
    bloques: &[Bloque],
) -> Marco {
    let titulo = term.title().unwrap_or_default();

    //  Si el VT no sabe decirlo, se finge que no hay historial: total igual a
    //  lo que se ve deja la barrita entera, que es como no tenerla.
    let (arriba, total) = term.viewport_position().unwrap_or((0, u32::from(filas)));

    //  La rejilla se compone fila a fila y NO es un calco del hueco visible:
    //  donde hay una salida recogida se pone una línea y se salta el resto.
    //  Por eso cada fila pintada viaja con la del historial a la que
    //  corresponde — sin eso, la barra no podría ni seleccionar ni saber a qué
    //  bloque pertenece lo que ve.
    let mut salida: Vec<Vec<Tramo>> = Vec::with_capacity(filas as usize);
    let mut filas_abs: Vec<u32> = Vec::with_capacity(filas as usize);
    let mut resumidas: Vec<u16> = Vec::new();

    let mut f: u16 = 0;
    while f < filas {
        let abs = arriba + u32::from(f);
        if let Some(bloque) = plegado_en(bloques, abs) {
            salida.push(resumen(bloque, fondo, apagado));
            filas_abs.push(bloque.fila);
            resumidas.push(salida.len() as u16 - 1);

            //  Al otro lado del bloque. Si se sale de lo visible, aquí se
            //  acaba la rejilla.
            let siguiente = bloque.fin.saturating_sub(arriba);
            if siguiente <= u32::from(f) {
                break;
            }
            f = u16::try_from(siguiente).unwrap_or(filas);
            continue;
        }

        salida.push(fila_en_tramos(term, f, fondo, enlaces));
        filas_abs.push(abs);
        f += 1;
    }

    let (col, fila) = term.cursor_position().unwrap_or((1, 1));

    //  El cursor va por la fila del historial en la que está de verdad, y esa
    //  puede haberse movido de sitio al recoger algo por encima. Se busca; si
    //  no aparece —está dentro de lo recogido— se queda al final, que es lo
    //  menos mentiroso.
    let cursor_abs = arriba + u32::from(fila.saturating_sub(1));
    let cursor_fila = filas_abs
        .iter()
        .position(|x| *x == cursor_abs)
        .map(|i| i as u16 + 1)
        .unwrap_or_else(|| salida.len().max(1) as u16);

    //  La última fila con algo escrito, o donde esté el cursor si va por
    //  debajo: es lo que de verdad ocupa la sesión ahora mismo.
    let ultima = salida
        .iter()
        .rposition(|f| !f.is_empty())
        .map(|i| i as u16 + 1)
        .unwrap_or(0);

    //  Solo los bloques que caen en lo que se ve: el filete se pinta por fila
    //  de la rejilla, y mandar los quinientos de la sesión entera para pintar
    //  tres sería tirar trabajo en cada marco.
    let abajo = arriba + u32::from(filas);
    let vistos: Vec<Bloque> = bloques
        .iter()
        .filter(|b| b.fila >= arriba && b.fila < abajo)
        .cloned()
        .collect();

    let estilo = term.cursor_style();

    Marco {
        que: "marco",
        filas: salida,
        filas_abs,
        resumidas,
        cursor: [col, cursor_fila],
        cursor_figura: match estilo.figura() {
            Figura::Bloque => "bloque",
            Figura::Subrayado => "subrayado",
            Figura::Barra => "barra",
        },
        cursor_parpadea: estilo.parpadea(),
        cols,
        filas_n: filas,
        usadas: ultima.max(cursor_fila),
        cwd,
        titulo,
        scroll: [arriba, total.max(u32::from(filas))],
        raton,
        bloques: vistos,
        ultimo: bloques.last().cloned(),
    }
}

//  Una línea suelta por la salida. El motor también escribe ahí, pero cada
//  uno toma el cerrojo para su línea entera, así que no se entrelazan.
fn decir<T: Serialize>(cosa: &T) {
    if let Ok(json) = serde_json::to_string(cosa) {
        let salida = std::io::stdout();
        let mut s = salida.lock();
        let _ = writeln!(s, "{}", json);
        let _ = s.flush();
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
fn motor(
    rx: std::sync::mpsc::Receiver<Recado>,
    pid_shell: u32,
    al_pty: Sender<Vec<u8>>,
    maestro: RawFd,
) {
    let mut ultimo_cwd = String::new();
    let mut term = match Terminal::new(COLS, FILAS) {
        Ok(t) => t,
        Err(_) => return,
    };

    //  Los colores de casa, guardados aparte: al teñir por un servidor hay que
    //  poder volver a ellos, y recalcularlos no valdría —el ambiente de la
    //  barra puede haber cambiado el tema mientras estabas dentro.
    let mut fondo_base = Rgb { r: 0, g: 0, b: 0 };
    let mut tinta_base = Rgb {
        r: 0xff,
        g: 0xff,
        b: 0xff,
    };
    //  El gris de la casa (`muted`) para lo que no reclama atención: la línea
    //  de una salida recogida es eso — está ahí, no molesta, y se despliega si
    //  la quieres. No viene en el tema porque el tema solo publica fondo y
    //  tinta, así que va fijo; es el mismo de Theme.qml.
    let apagado = Rgb {
        r: 0x8e,
        g: 0x8e,
        b: 0x93,
    };
    if let Some(t) = tema::leer(&tema::ruta_por_defecto()) {
        fondo_base = Rgb {
            r: t.fondo.r,
            g: t.fondo.g,
            b: t.fondo.b,
        };
        tinta_base = Rgb {
            r: t.tinta.r,
            g: t.tinta.g,
            b: t.tinta.b,
        };
        term.set_default_colors(tinta_base, fondo_base);
    }
    let mut fondo = hex(fondo_base);
    let apagado = hex(apagado);

    let mut cols = COLS;
    let mut filas = FILAS;
    let mut sucio = false;
    let mut desde: Option<Instant> = None;
    let salida = std::io::stdout();

    //  Lo que la aplicación pide sin decirlo: corchetes al pegar, ratón,
    //  portapapeles. La misma pieza que usa la ventana.
    let mut modos = ModeTracker::new();

    //  Los mandatos vistos, en coordenadas del historial. Se podan por arriba
    //  porque una sesión de un día entero acumularía miles y solo se usan los
    //  de cerca: los que se ven y el último.
    let mut bloques: Vec<Bloque> = Vec::new();
    const TOPE_BLOQUES: usize = 400;

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
                //  Los modos se miran ANTES de que el VT se coma los bytes:
                //  los dos leen lo mismo, cada uno a lo suyo.
                modos.feed(&b);
                if let Some(texto) = modos.take_clipboard_write() {
                    decir(&Portapapeles {
                        que: "portapapeles",
                        texto,
                    });
                }
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
            Ok(Recado::Tope { arriba }) => {
                let _ = if arriba {
                    term.scroll_viewport_top()
                } else {
                    term.scroll_viewport_bottom()
                };
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Rueda {
                lineas,
                col,
                fila,
                historial,
            }) => {
                //  Con el ratón puesto, la rueda es de la aplicación: en htop
                //  o en less mover el historial del VT no haría nada visible y
                //  el programa se quedaría sin enterarse de nada.
                if modos.mouse_reporting_enabled() && !historial {
                    let boton = if lineas < 0 {
                        Boton::RuedaArriba
                    } else {
                        Boton::RuedaAbajo
                    };
                    for _ in 0..lineas.unsigned_abs().min(10) {
                        if let Some(bytes) = encode_mouse(
                            SucesoRaton::Pulsar,
                            boton,
                            col.max(1),
                            fila.max(1),
                            KeyModifiers::default(),
                            modos.mouse_sgr_enabled(),
                        ) {
                            let _ = al_pty.send(bytes);
                        }
                    }
                } else {
                    let _ = term.scroll_viewport(lineas);
                }
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Pegar(texto)) => {
                let _ = al_pty.send(modos.paste_payload(&texto));
            }
            Ok(Recado::Raton(clic)) => {
                //  Sin modo de ratón no se manda nada: el arrastre es de la
                //  vista, que lo usa para seleccionar. Y moverse sin arrastrar
                //  solo lo quiere quien pidió 1003; con 1002 se cuentan los
                //  arrastres, no el paseo.
                let paseo = clic.suceso == SucesoRaton::Mover;
                let interesa =
                    modos.mouse_reporting_enabled() && (!paseo || modos.mouse_any_event_enabled());
                if interesa
                    && let Some(bytes) = encode_mouse(
                        clic.suceso,
                        clic.boton,
                        clic.col,
                        clic.fila,
                        clic.mods,
                        modos.mouse_sgr_enabled(),
                    )
                {
                    let _ = al_pty.send(bytes);
                }
            }
            Ok(Recado::TextoDe {
                desde: d,
                hasta,
                col_desde,
                col_hasta,
                motivo,
            }) => {
                decir(&Texto {
                    que: "texto",
                    texto: texto_de(&term, d, hasta, col_desde, col_hasta),
                    motivo,
                });
            }
            Ok(Recado::Emigrar) => {
                //  El equipaje lo compone quien tiene el VT delante: dónde
                //  está, cómo se llama, de qué tamaño iba y con qué repintar.
                let equipaje = traspaso::Equipaje {
                    cwd: directorio(pid_shell).unwrap_or_default(),
                    titulo: term.title().unwrap_or_default(),
                    cols,
                    filas,
                    pintura: ghostty_vt::repintar(&term, filas, &term.title().unwrap_or_default()),
                };

                match traspaso::ofrecer(maestro, &equipaje) {
                    Ok((ruta, aviso)) => {
                        decir(&Emigrando {
                            que: "emigrando",
                            socket: ruta.to_string_lossy().into_owned(),
                        });

                        //  A partir de aquí esta sesión ya no es nuestra: en
                        //  cuanto se la lleven, este proceso sobra. El shell
                        //  no se toca —lo adopta el sistema— y sigue vivo
                        //  porque el otro lado tiene su copia del maestro.
                        thread::spawn(move || {
                            if aviso.recv().unwrap_or(false) {
                                std::process::exit(0);
                            }
                        });
                    }
                    Err(fallo) => decir(&AvisoDicho {
                        que: "aviso",
                        texto: fallo,
                    }),
                }
            }
            Ok(Recado::Tinte { color }) => {
                //  Se tiñe el FONDO POR DEFECTO del terminal y no se pinta un
                //  marco: así queda teñido todo lo que no lleva color propio,
                //  que es casi todo, y se ve de reojo sin leer nada. Y se
                //  guarda el de antes, que el ambiente de la barra puede
                //  cambiar el tema mientras estás dentro.
                let nuevo = match k4term_puente::servidores::color_del_tinte(&color) {
                    Some((r, g, b)) => {
                        let base = fondo_base;
                        let (r, g, b) = k4term_puente::servidores::fondo_tenido(
                            (base.r, base.g, base.b),
                            (r, g, b),
                        );
                        Rgb { r, g, b }
                    }
                    None => fondo_base,
                };
                fondo = hex(nuevo);
                term.set_default_colors(tinta_base, nuevo);
                sucio = true;
                desde.get_or_insert_with(Instant::now);
            }
            Ok(Recado::Plegar { fila }) => {
                //  Se pliega por la fila donde empieza el bloque, que es lo
                //  único que la barra puede nombrar sin ambigüedad: los
                //  índices de la rejilla cambian en cuanto sale una línea más.
                if let Some(bloque) = bloques.iter_mut().find(|b| b.fila == fila) {
                    bloque.plegado = !bloque.plegado;
                    sucio = true;
                    desde.get_or_insert_with(Instant::now);
                }
            }
            Ok(Recado::Saltar { hacia }) => {
                let (arriba, _) = term.viewport_position().unwrap_or((0, 0));
                let destino = if hacia < 0 {
                    bloques
                        .iter()
                        .rev()
                        .find(|b| b.fila < arriba)
                        .map(|b| b.fila)
                } else {
                    bloques.iter().find(|b| b.fila > arriba).map(|b| b.fila)
                };
                if let Some(fila) = destino {
                    let salto = i64::from(fila) - i64::from(arriba);
                    let _ =
                        term.scroll_viewport(salto.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
                    sucio = true;
                    desde.get_or_insert_with(Instant::now);
                }
            }
            Ok(Recado::Buscar { texto, hacia }) => {
                let (arriba, total) = term.viewport_position().unwrap_or((0, 0));
                let aguja = texto.trim().to_lowercase();

                let mut hallada: Option<u32> = None;
                if !aguja.is_empty() {
                    //  Se empieza en la fila de al lado y no en la de ahora:
                    //  si no, buscar «lo siguiente» te deja clavado en lo que
                    //  ya tienes delante.
                    if hacia < 0 {
                        let mut f = arriba;
                        while f > 0 {
                            f -= 1;
                            if fila_contiene(&term, f, &aguja) {
                                hallada = Some(f);
                                break;
                            }
                        }
                    } else {
                        let mut f = arriba + 1;
                        while f < total {
                            if fila_contiene(&term, f, &aguja) {
                                hallada = Some(f);
                                break;
                            }
                            f += 1;
                        }
                    }
                }

                if let Some(f) = hallada {
                    let salto = i64::from(f) - i64::from(arriba);
                    let _ = term.scroll_viewport(salto.clamp(-1_000_000, 1_000_000) as i32);
                    sucio = true;
                    desde.get_or_insert_with(Instant::now);
                }

                decir(&Buscado {
                    que: "buscado",
                    hay: hallada.is_some(),
                    fila: hallada.unwrap_or(arriba),
                });
            }
            Ok(Recado::Nota { entera }) => {
                //  La puerta a Edinot solo se abre si Edinot está, igual que
                //  en la ventana: quien no lo tenga no se entera de que
                //  existe y la tecla no hace nada.
                let ultimo = bloques.last().cloned();
                let (titulo, cuerpo) = if entera {
                    (
                        "Sesión de terminal".to_string(),
                        texto_de(&term, 0, 0, 0, 0),
                    )
                } else {
                    match ultimo {
                        Some(b) => (
                            "Salida de un mandato".to_string(),
                            texto_de(&term, b.fila, b.fin.saturating_sub(1), 0, 0),
                        ),
                        None => (String::new(), String::new()),
                    }
                };

                if cuerpo.trim().is_empty() {
                    decir(&AvisoDicho {
                        que: "aviso",
                        texto: "No hay nada que anotar".to_string(),
                    });
                } else if !edinot::disponible() {
                    decir(&AvisoDicho {
                        que: "aviso",
                        texto: "No hay Edinot abierto".to_string(),
                    });
                } else {
                    //  En su propio hilo: la primera llamada se engancha a la
                    //  aplicación viva y puede tardar lo suyo, y el motor no
                    //  puede quedarse parado mientras tanto — es quien pinta.
                    thread::spawn(move || match edinot::anotar(&titulo, &cuerpo) {
                        Ok(nota) => decir(&AvisoDicho {
                            que: "aviso",
                            texto: format!("Guardado en {nota}"),
                        }),
                        Err(fallo) => decir(&AvisoDicho {
                            que: "aviso",
                            texto: fallo,
                        }),
                    });
                }
            }
            Ok(Recado::Marca {
                empieza,
                salida,
                mandato,
            }) => {
                if empieza {
                    bloques.push(Bloque {
                        fila: fila_absoluta(&term),
                        estado: "corre",
                        fin: 0,
                        mandato,
                        plegado: false,
                    });
                    if bloques.len() > TOPE_BLOQUES {
                        bloques.remove(0);
                    }
                } else if let Some(ultimo) = bloques.last_mut() {
                    ultimo.estado = if salida == 0 { "bien" } else { "mal" };
                    ultimo.fin = fila_absoluta(&term);
                }
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
            if let Ok(json) = serde_json::to_string(&marco(
                &term,
                cols,
                filas,
                &fondo,
                &apagado,
                ultimo_cwd.clone(),
                modos.mouse_reporting_enabled(),
                modos.hyperlinks_seen(),
                &bloques,
            )) {
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

//  El maestro del PTY, venga de donde venga: abierto por nosotros o heredado
//  de otro frontal. Lo único que se le pide es leer, escribir y cambiar de
//  tamaño, y eso se puede hacer igual con los dos.
enum Maestro {
    Propio(Box<dyn portable_pty::MasterPty + Send>),
    Heredado(OwnedFd),
}

impl Maestro {
    fn fd(&self) -> RawFd {
        match self {
            Maestro::Propio(m) => m.as_raw_fd().unwrap_or(-1),
            Maestro::Heredado(fd) => fd.as_raw_fd(),
        }
    }

    fn lector(&self) -> Box<dyn Read + Send> {
        match self {
            Maestro::Propio(m) => m.try_clone_reader().expect("lector del pty"),
            Maestro::Heredado(fd) => Box::new(std::fs::File::from(
                fd.try_clone().expect("copia para leer"),
            )),
        }
    }

    fn escritor(&self) -> Box<dyn Write + Send> {
        match self {
            Maestro::Propio(m) => m.take_writer().expect("escritor del pty"),
            Maestro::Heredado(fd) => Box::new(std::fs::File::from(
                fd.try_clone().expect("copia para escribir"),
            )),
        }
    }

    fn medir(&self, cols: u16, filas: u16) {
        match self {
            Maestro::Propio(m) => {
                let _ = m.resize(PtySize {
                    rows: filas,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            Maestro::Heredado(fd) => traspaso::medir(fd.as_raw_fd(), cols, filas),
        }
    }
}

fn main() {
    //  Una sesión que ya existe esperando en un socket: viene de una ventana
    //  que la devuelve a la isla. Con esto no se abre ninguna shell.
    let argv: Vec<String> = std::env::args().collect();
    let heredar = argv
        .iter()
        .position(|a| a == "--heredar")
        .and_then(|i| argv.get(i + 1))
        .map(PathBuf::from);

    let (maestro, mut hijo, pid_shell, pintura) = match heredar {
        Some(ruta) => match traspaso::recoger(&ruta) {
            Ok((fd, equipaje)) => (Maestro::Heredado(fd), None, 0, equipaje.pintura),
            Err(fallo) => {
                eprintln!("k4term-isla: no se pudo heredar la sesión: {fallo}");
                std::process::exit(1);
            }
        },
        None => {
            let pty = native_pty_system();
            let par = pty
                .openpty(PtySize {
                    rows: FILAS,
                    cols: COLS,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("no se pudo abrir el pty");

            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let es_fish = PathBuf::from(&shell)
                .file_name()
                .is_some_and(|nombre| nombre == "fish");
            let mut cmd = CommandBuilder::new(shell);
            // See k4term: fish's optional terminal probes wait for replies that an
            // embedded session cannot provide, delaying the first prompt by seconds.
            if es_fish {
                cmd.args(["--features", "no-query-term"]);
            }
            cmd.arg("-l");
            cmd.env("TERM", "xterm-256color");
            cmd.env("COLORTERM", "truecolor");
            cmd.env("TERM_PROGRAM", "k4term");
            if let Ok(home) = std::env::var("HOME") {
                cmd.cwd(home);
            }

            let hijo = par.slave.spawn_command(cmd).expect("no arrancó la shell");
            let pid = hijo.process_id().unwrap_or(0);
            (Maestro::Propio(par.master), Some(hijo), pid, String::new())
        }
    };

    let mut lector = maestro.lector();
    let mut escritor = maestro.escritor();
    let fd_maestro = maestro.fd();

    //  Con una sesión heredada no hay hijo al que esperar, así que el final lo
    //  marca el lector cuando se acaba el chorro.
    let (fin_tx, fin) = channel::<()>();

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
        thread::spawn(move || motor(rx, pid_shell, al_pty, fd_maestro));
    }

    //  Y si la sesión viene de otro sitio, lo primero que ve su VT nuevo es la
    //  pantalla que traía: para el terminal es lo mismo que si lo hubiera
    //  escupido el PTY.
    if !pintura.is_empty() {
        let _ = tx.send(Recado::Bytes(pintura.into_bytes()));
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
                if tx.send(Recado::Ajustes(Box::new(ajustes))).is_err() {
                    break;
                }
            }
        });
    }

    // ── el PTY habla ──────────────────────────────────────────────
    //
    //  Y de paso se escucha lo que la shell cuenta de sí misma. Los marcadores
    //  se leen del chorro ANTES de llegar al VT, igual que en la ventana: el
    //  API C de ghostty no los expone y no hace falta que lo haga.
    {
        let tx: Sender<Recado> = tx.clone();
        thread::spawn(move || {
            let avisos = trabajos::notificador_con(|parte| match parte {
                trabajos::Parte::Empezado { mandato, segundos } => decir(&Trabajo {
                    que: "trabajo",
                    estado: "empieza",
                    mandato,
                    salida: 0,
                    segundos,
                }),
                trabajos::Parte::Acabado {
                    mandato,
                    salida,
                    segundos,
                } => decir(&Trabajo {
                    que: "trabajo",
                    estado: "acaba",
                    mandato,
                    salida,
                    segundos,
                }),
            });

            let mut escaner = Escaner::new();
            let mut mandato = String::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = match lector.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };

                //  El trozo se parte por donde iba cada marca y se manda a
                //  cachos, con la marca en medio. Mandarlo entero y apuntar
                //  después sitúa el mandato donde acabó la ráfaga: con un
                //  `seq 1 40`, que sale de una tacada, la marca caía tres
                //  líneas por debajo de donde de verdad empezó.
                let sucesos = escaner.tragar_con_sitio(&buf[..n]);
                let mut cortado = 0usize;
                let mut roto = false;

                for (sitio, suceso) in sucesos {
                    let marca = matches!(suceso, Suceso::Comienza | Suceso::Termina { .. });
                    if marca && sitio > cortado {
                        if tx
                            .send(Recado::Bytes(buf[cortado..sitio].to_vec()))
                            .is_err()
                        {
                            roto = true;
                            break;
                        }
                        cortado = sitio;
                    }

                    match suceso {
                        Suceso::Mandato(m) => mandato = m,
                        Suceso::Comienza => {
                            //  El mandato se manda a las DOS partes: al motor,
                            //  que lo enseña cuando la salida está recogida, y
                            //  al contador de trabajos, que lo enseña en la
                            //  píldora. Por eso se clona antes de vaciarlo.
                            let _ = tx.send(Recado::Marca {
                                empieza: true,
                                salida: 0,
                                mandato: mandato.clone(),
                            });
                            let _ = avisos.send(Aviso::Empieza {
                                mandato: std::mem::take(&mut mandato),
                            });
                        }
                        Suceso::Termina { salida } => {
                            let _ = tx.send(Recado::Marca {
                                empieza: false,
                                salida,
                                mandato: String::new(),
                            });
                            let _ = avisos.send(Aviso::Acaba { salida });
                        }
                        Suceso::Campana => decir(&Campana {
                            que: "campana",
                            titulo: mandato.clone(),
                        }),
                        Suceso::Directorio(_) => {}
                    }
                }

                if roto {
                    break;
                }

                //  Y lo que venga detrás de la última marca. Sin marcas, esto
                //  es el trozo entero y todo sigue como siempre.
                if cortado < n && tx.send(Recado::Bytes(buf[cortado..n].to_vec())).is_err() {
                    break;
                }
            }

            let _ = fin_tx.send(());
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
                    maestro.medir(cols, filas);
                    let _ = tx.send(Recado::Medida { cols, filas });
                }
                Orden::Pinta => {
                    let _ = tx.send(Recado::Pinta);
                }
                Orden::Donde => {
                    let _ = tx.send(Recado::Donde);
                }
                Orden::Rueda {
                    lineas,
                    col,
                    fila,
                    historial,
                } => {
                    let _ = tx.send(Recado::Rueda {
                        lineas,
                        col,
                        fila,
                        historial,
                    });
                }
                Orden::Tope { arriba } => {
                    let _ = tx.send(Recado::Tope { arriba });
                }
                Orden::Pegar { valor } => {
                    let _ = tx.send(Recado::Pegar(valor));
                }
                Orden::Raton {
                    tipo,
                    boton,
                    col,
                    fila,
                    shift,
                    control,
                    alt,
                } => {
                    let suceso = match tipo.as_str() {
                        "pulsar" => SucesoRaton::Pulsar,
                        "soltar" => SucesoRaton::Soltar,
                        "mover" => SucesoRaton::Mover,
                        _ => continue,
                    };
                    let boton = match boton.as_str() {
                        "medio" => Boton::Medio,
                        "derecho" => Boton::Derecho,
                        "arriba" => Boton::RuedaArriba,
                        "abajo" => Boton::RuedaAbajo,
                        _ => Boton::Izquierdo,
                    };
                    let _ = tx.send(Recado::Raton(Clic {
                        suceso,
                        boton,
                        col: col.max(1),
                        fila: fila.max(1),
                        mods: KeyModifiers {
                            shift,
                            control,
                            alt,
                            super_key: false,
                        },
                    }));
                }
                Orden::TextoDe {
                    desde,
                    hasta,
                    col_desde,
                    col_hasta,
                    motivo,
                } => {
                    let _ = tx.send(Recado::TextoDe {
                        desde,
                        hasta,
                        col_desde,
                        col_hasta,
                        motivo,
                    });
                }
                Orden::Saltar { hacia } => {
                    let _ = tx.send(Recado::Saltar { hacia });
                }
                Orden::Tinte { color } => {
                    let _ = tx.send(Recado::Tinte { color });
                }
                Orden::Plegar { fila } => {
                    let _ = tx.send(Recado::Plegar { fila });
                }
                Orden::Emigrar => {
                    let _ = tx.send(Recado::Emigrar);
                }
                Orden::Buscar { texto, hacia } => {
                    let _ = tx.send(Recado::Buscar { texto, hacia });
                }
                Orden::Nota { entera } => {
                    let _ = tx.send(Recado::Nota { entera });
                }
            }
        }
        std::process::exit(0);
    });

    //  El final: si la shell es hija nuestra, se la espera; si la sesión vino
    //  heredada no hay a quién esperar —la adoptó el sistema al soltarla— y lo
    //  que marca el final es que se acabe el chorro.
    match hijo.as_mut() {
        Some(h) => {
            let _ = h.wait();
        }
        None => {
            let _ = fin.recv();
        }
    }
}
