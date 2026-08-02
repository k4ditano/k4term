//  El tema de la barra, leído del fichero que ella publica.
//
//  La barra escribe ~/.local/state/k4/tema.json al arrancar y cada vez que el
//  ambiente cambia (el tinte de la mazmorra, por ejemplo). Aquí se lee y se
//  vigila con inotify: cero procesos, cero sondeos, y la terminal cambia de
//  color a la vez que la isla.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tema {
    pub fondo: Color,
    pub tinta: Color,
    //  El depósito de chispa de la barra, seco. Viene por el mismo fichero
    //  porque es lo mismo: lo que la barra publica de sí misma.
    pub seco: bool,
}

#[derive(Deserialize)]
struct TemaCrudo {
    fondo: String,
    tinta: String,
    #[serde(default)]
    tokens: Option<TokensCrudo>,
}

#[derive(Deserialize)]
struct TokensCrudo {
    #[serde(default)]
    seco: bool,
}

//  Qt escribe «#rrggbb», y «#aarrggbb» cuando el color lleva alfa — el alfa
//  va DELANTE, que es la trampa de este formato.
pub fn color_de_hex(texto: &str) -> Option<Color> {
    let limpio = texto.trim().trim_start_matches('#');
    let (r, g, b) = match limpio.len() {
        6 => (0, 2, 4),
        8 => (2, 4, 6),
        _ => return None,
    };
    let byte = |i: usize| u8::from_str_radix(limpio.get(i..i + 2)?, 16).ok();
    Some(Color {
        r: byte(r)?,
        g: byte(g)?,
        b: byte(b)?,
    })
}

pub fn ruta_por_defecto() -> PathBuf {
    if let Ok(explicita) = std::env::var("K4TERM_TEMA") {
        return PathBuf::from(explicita);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/k4/tema.json")
}

pub fn leer(ruta: &Path) -> Option<Tema> {
    let bruto = std::fs::read_to_string(ruta).ok()?;
    let crudo: TemaCrudo = serde_json::from_str(&bruto).ok()?;
    Some(Tema {
        fondo: color_de_hex(&crudo.fondo)?,
        tinta: color_de_hex(&crudo.tinta)?,
        seco: crudo.tokens.map(|t| t.seco).unwrap_or(false),
    })
}

//  Devuelve un canal por el que van llegando los temas nuevos. Se vigila la
//  CARPETA y no el fichero: quien lo escribe lo reemplaza entero, y un
//  inotify clavado al inodo viejo se queda mirando un fichero que ya no es.
pub fn vigilar(ruta: PathBuf) -> Receiver<Tema> {
    let (tx, rx) = channel::<Tema>();

    std::thread::spawn(move || {
        let (tx_fs, rx_fs) = channel();
        let mut vigia = match notify::recommended_watcher(tx_fs) {
            Ok(v) => v,
            Err(_) => return,
        };

        let carpeta = match ruta.parent() {
            Some(c) => c.to_path_buf(),
            None => return,
        };
        if vigia.watch(&carpeta, RecursiveMode::NonRecursive).is_err() {
            return;
        }

        while let Ok(suceso) = rx_fs.recv() {
            let Ok(suceso) = suceso else { continue };
            if !matches!(
                suceso.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
            ) {
                continue;
            }
            if !suceso.paths.iter().any(|p| p == &ruta) {
                continue;
            }

            //  Un respiro antes de leer: quien escribe puede estar a medias, y
            //  un JSON truncado se descarta en silencio y no vuelve a
            //  intentarse hasta el siguiente aviso.
            std::thread::sleep(Duration::from_millis(30));
            if let Some(tema) = leer(&ruta) {
                if tx.send(tema).is_err() {
                    return;
                }
            }
        }
    });

    rx
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_hex_de_seis_y_de_ocho() {
        assert_eq!(
            color_de_hex("#1c1c1e"),
            Some(Color {
                r: 0x1c,
                g: 0x1c,
                b: 0x1e
            })
        );
        // con alfa delante, que es como lo escribe Qt
        assert_eq!(
            color_de_hex("#ff30d158"),
            Some(Color {
                r: 0x30,
                g: 0xd1,
                b: 0x58
            })
        );
    }

    #[test]
    fn rechaza_lo_que_no_es_color() {
        assert_eq!(color_de_hex("azul"), None);
        assert_eq!(color_de_hex("#abc"), None);
        assert_eq!(color_de_hex(""), None);
    }
}
