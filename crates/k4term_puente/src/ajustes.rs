//  Los ajustes de k4term.
//
//  Un fichero de texto en ~/.config/k4term/k4term.conf, con `clave = valor`
//  y almohadillas para comentar. Sin dependencias y sin ceremonias: lo que
//  hay que poder tocar sin recompilar son cuatro cosas, y un formato que se
//  entiende sin documentación vale más aquí que uno con esquema.
//
//      # la fuente de la casa
//      fuente = MesloLGS Nerd Font Mono
//      tamaño = 13
//      margen = 12
//      shell  = /usr/bin/fish
//
//  Lo que no se diga se queda como estaba: los ajustes son un empujón sobre
//  los valores de fábrica, no una declaración completa.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Ajustes {
    pub fuente: String,
    pub tamano: f32,
    pub margen: f32,
    pub shell: Option<String>,
    //  Cristal: cuánto se ve de lo que hay detrás y con cuánto radio se
    //  recorta la lámina. Con opacidad 1 se comporta como siempre.
    pub opacidad: f32,
    pub radio: f32,
    //  Atenuar lo anterior al último mandato, de salida.
    pub tranquilo: bool,
}

impl Default for Ajustes {
    fn default() -> Self {
        Self {
            //  La misma que los iconos de la barra, que resulta ser una
            //  monoespaciada de terminal de toda la vida.
            fuente: "MesloLGS Nerd Font Mono".to_string(),
            tamano: 13.0,
            margen: 12.0,
            shell: None,
            //  De fábrica, cristal suave y las esquinas de la isla: es la
            //  cara de la casa, y quien no la quiera pone opacidad = 1.
            opacidad: 0.94,
            //  Cero: que redondee el compositor, que en Hyprland ya lo hace y
            //  con el radio que el usuario tenga puesto en su tema.
            radio: 0.0,
            tranquilo: false,
        }
    }
}

pub fn ruta_por_defecto() -> PathBuf {
    if let Ok(explicita) = std::env::var("K4TERM_AJUSTES") {
        return PathBuf::from(explicita);
    }
    let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.config")
    });
    PathBuf::from(base).join("k4term/k4term.conf")
}

impl Ajustes {
    pub fn leer() -> Self {
        let mut ajustes = Ajustes::default();
        let Ok(texto) = std::fs::read_to_string(ruta_por_defecto()) else {
            return ajustes;
        };
        ajustes.aplicar(&texto);
        ajustes
    }

    //  Se ignora en silencio lo que no se entienda. Un ajuste con una errata
    //  no debe impedir que la terminal abra: peor que un tamaño raro es no
    //  tener terminal para arreglarlo.
    pub fn aplicar(&mut self, texto: &str) {
        for linea in texto.lines() {
            let limpia = linea.split('#').next().unwrap_or("").trim();
            if limpia.is_empty() {
                continue;
            }
            let Some((clave, valor)) = limpia.split_once('=') else {
                continue;
            };
            let valor = valor.trim();
            if valor.is_empty() {
                continue;
            }

            match clave.trim().to_lowercase().as_str() {
                "fuente" | "font" => self.fuente = valor.to_string(),
                // con y sin tilde: nadie debería pelearse con el teclado
                "tamaño" | "tamano" | "size" => {
                    if let Ok(n) = valor.parse::<f32>() {
                        self.tamano = n.clamp(6.0, 72.0);
                    }
                }
                "margen" | "padding" => {
                    if let Ok(n) = valor.parse::<f32>() {
                        self.margen = n.clamp(0.0, 200.0);
                    }
                }
                "shell" => self.shell = Some(valor.to_string()),
                "opacidad" | "opacity" => {
                    if let Ok(n) = valor.parse::<f32>() {
                        self.opacidad = n.clamp(0.2, 1.0);
                    }
                }
                "radio" | "radius" => {
                    if let Ok(n) = valor.parse::<f32>() {
                        self.radio = n.clamp(0.0, 40.0);
                    }
                }
                "tranquilo" | "quiet" => {
                    self.tranquilo = matches!(valor.to_lowercase().as_str(), "si" | "sí" | "true" | "1")
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn lee_lo_que_entiende_y_deja_lo_demas() {
        let mut a = Ajustes::default();
        a.aplicar(
            "# un comentario\n\
             fuente = Fira Code   # con comentario al final\n\
             tamaño = 16\n\
             desconocido = 3\n\
             margen =\n",
        );
        assert_eq!(a.fuente, "Fira Code");
        assert_eq!(a.tamano, 16.0);
        // sin valor y sin conocer: se quedan como estaban
        assert_eq!(a.margen, Ajustes::default().margen);
        assert!(a.shell.is_none());
    }

    #[test]
    fn los_numeros_absurdos_se_recortan() {
        let mut a = Ajustes::default();
        a.aplicar("tamaño = 900\nmargen = -4\n");
        assert_eq!(a.tamano, 72.0);
        assert_eq!(a.margen, 0.0);
    }

    #[test]
    fn una_errata_no_tumba_el_resto() {
        let mut a = Ajustes::default();
        a.aplicar("tamaño = grande\nfuente = Iosevka\n");
        assert_eq!(a.tamano, Ajustes::default().tamano);
        assert_eq!(a.fuente, "Iosevka");
    }
}
