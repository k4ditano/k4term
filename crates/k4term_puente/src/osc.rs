//  Los marcadores que manda la shell, leídos del propio chorro del PTY.
//
//  El VT de ghostty no nos los cuenta —su API C de hoy no llega ahí—, pero no
//  hace falta: los bytes pasan por nosotros ANTES de llegar al terminal, así
//  que se miran al vuelo y se dejan seguir intactos. Sale gratis y no toca la
//  emulación.
//
//  Se entienden tres cosas, todas convenciones de la casa grande:
//
//    · OSC 133;C  y  OSC 133;D;<código>  — empieza y termina un mandato
//      (el «prompt semántico» de iTerm2, que hoy habla medio mundo).
//    · OSC 633;E;<mandato>               — qué se ha escrito (convención de
//      VS Code, la que usan las integraciones modernas).
//    · OSC 7;file://<equipo><ruta>       — dónde estás.
//
//  Quien los emite es integracion/k4term.zsh (o .fish). Sin esa integración
//  esto no ve nada y la terminal funciona igual: los avisos son un extra, no
//  un requisito.

const TOPE: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suceso {
    Comienza,
    Termina { salida: i32 },
    Mandato(String),
    Directorio(String),
    //  Un BEL a secas. Ojo: el mismo byte cierra las secuencias OSC, así que
    //  solo cuenta como campana fuera de una — y eso lo sabe el escáner
    //  porque lleva el estado.
    Campana,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Estado {
    Fuera,
    TrasEscape,
    Dentro,
    DentroTrasEscape,
}

#[derive(Debug)]
pub struct Escaner {
    estado: Estado,
    cuerpo: Vec<u8>,
}

impl Default for Escaner {
    fn default() -> Self {
        Self::new()
    }
}

impl Escaner {
    pub fn new() -> Self {
        Self {
            estado: Estado::Fuera,
            cuerpo: Vec::new(),
        }
    }

    //  Los bytes se pasan tal cual; lo que devuelve son los sucesos que haya
    //  reconocido. Aguanta que una secuencia venga partida entre dos lecturas,
    //  que con un PTY pasa a menudo.
    pub fn tragar(&mut self, bytes: &[u8]) -> Vec<Suceso> {
        self.tragar_con_sitio(bytes)
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    //  Lo mismo, diciendo además POR QUÉ BYTE iba cada suceso — el primero que
    //  ya no es suyo. Quien apunta en qué fila de la pantalla ocurrió algo lo
    //  necesita: si se traga el trozo entero y luego mira el cursor, la marca
    //  cae donde acabó la ráfaga y no donde estaba el mandato. Se vio: copiar
    //  la salida del último mandato empezaba tres líneas más abajo.
    pub fn tragar_con_sitio(&mut self, bytes: &[u8]) -> Vec<(usize, Suceso)> {
        let mut sucesos = Vec::new();

        for (i, &b) in bytes.iter().enumerate() {
            match self.estado {
                Estado::Fuera => {
                    if b == 0x1b {
                        self.estado = Estado::TrasEscape;
                    } else if b == 0x07 {
                        sucesos.push((i + 1, Suceso::Campana));
                    }
                }
                Estado::TrasEscape => {
                    if b == b']' {
                        self.estado = Estado::Dentro;
                        self.cuerpo.clear();
                    } else if b == 0x1b {
                        // otro escape seguido: seguimos esperando
                    } else {
                        self.estado = Estado::Fuera;
                    }
                }
                Estado::Dentro => match b {
                    0x07 | 0x9c => {
                        if let Some(s) = self.interpretar() {
                            sucesos.push((i + 1, s));
                        }
                        self.cerrar();
                    }
                    0x1b => self.estado = Estado::DentroTrasEscape,
                    _ => {
                        if self.cuerpo.len() >= TOPE {
                            // Una secuencia sin fin no es nuestra: se suelta.
                            self.cerrar();
                        } else {
                            self.cuerpo.push(b);
                        }
                    }
                },
                Estado::DentroTrasEscape => {
                    if b == b'\\' {
                        if let Some(s) = self.interpretar() {
                            sucesos.push((i + 1, s));
                        }
                        self.cerrar();
                    } else {
                        // ESC suelto dentro del cuerpo: no era el final
                        self.cuerpo.push(0x1b);
                        self.cuerpo.push(b);
                        self.estado = Estado::Dentro;
                    }
                }
            }
        }

        sucesos
    }

    fn cerrar(&mut self) {
        self.estado = Estado::Fuera;
        self.cuerpo.clear();
    }

    fn interpretar(&self) -> Option<Suceso> {
        let texto = std::str::from_utf8(&self.cuerpo).ok()?;
        let (codigo, resto) = texto.split_once(';')?;

        match codigo {
            "133" => {
                let mut trozos = resto.splitn(2, ';');
                match trozos.next()? {
                    "C" => Some(Suceso::Comienza),
                    "D" => {
                        let salida = trozos
                            .next()
                            .and_then(|s| s.trim().parse::<i32>().ok())
                            .unwrap_or(0);
                        Some(Suceso::Termina { salida })
                    }
                    _ => None,
                }
            }
            "633" => {
                let (clase, valor) = resto.split_once(';')?;
                (clase == "E").then(|| Suceso::Mandato(valor.to_string()))
            }
            "7" => {
                let sin_esquema = resto.strip_prefix("file://")?;
                // file://<equipo>/ruta — el equipo se descarta
                let barra = sin_esquema.find('/')?;
                Some(Suceso::Directorio(despercentar(&sin_esquema[barra..])))
            }
            _ => None,
        }
    }
}

//  Las rutas vienen como URL, así que un espacio llega como %20.
fn despercentar(texto: &str) -> String {
    let bytes = texto.as_bytes();
    let mut salida = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&texto[i + 1..i + 3], 16)
        {
            salida.push(byte);
            i += 3;
            continue;
        }
        salida.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&salida).into_owned()
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn reconoce_principio_y_final_con_codigo() {
        let mut e = Escaner::new();
        assert_eq!(e.tragar(b"\x1b]133;C\x07"), vec![Suceso::Comienza]);
        assert_eq!(
            e.tragar(b"\x1b]133;D;130\x07"),
            vec![Suceso::Termina { salida: 130 }]
        );
    }

    #[test]
    fn el_final_sin_codigo_se_toma_por_bueno() {
        let mut e = Escaner::new();
        assert_eq!(
            e.tragar(b"\x1b]133;D\x07"),
            vec![Suceso::Termina { salida: 0 }]
        );
    }

    #[test]
    fn lee_el_mandato_y_el_directorio() {
        let mut e = Escaner::new();
        assert_eq!(
            e.tragar(b"\x1b]633;E;cargo build --release\x07"),
            vec![Suceso::Mandato("cargo build --release".into())]
        );
        assert_eq!(
            e.tragar(b"\x1b]7;file://abel/home/abel/mis%20cosas\x07"),
            vec![Suceso::Directorio("/home/abel/mis cosas".into())]
        );
    }

    #[test]
    fn aguanta_una_secuencia_partida_en_dos_lecturas() {
        let mut e = Escaner::new();
        assert!(e.tragar(b"salida normal\x1b]133;").is_empty());
        assert_eq!(
            e.tragar(b"D;1\x07mas salida"),
            vec![Suceso::Termina { salida: 1 }]
        );
    }

    #[test]
    fn admite_el_final_largo_esc_barra() {
        let mut e = Escaner::new();
        assert_eq!(e.tragar(b"\x1b]133;C\x1b\\"), vec![Suceso::Comienza]);
    }

    #[test]
    fn el_texto_corriente_no_dispara_nada() {
        let mut e = Escaner::new();
        assert!(
            e.tragar(b"hola \x1b[31mrojo\x1b[0m y ] suelto\n")
                .is_empty()
        );
    }

    #[test]
    fn la_campana_suena_suelta_pero_no_cerrando_una_secuencia() {
        let mut e = Escaner::new();
        assert_eq!(e.tragar(b"ojo\x07"), vec![Suceso::Campana]);
        // aquí el mismo byte solo está cerrando el OSC: no es campana
        assert_eq!(
            e.tragar(b"\x1b]633;E;ls\x07"),
            vec![Suceso::Mandato("ls".into())]
        );
    }

    #[test]
    fn una_secuencia_sin_fin_no_crece_para_siempre() {
        let mut e = Escaner::new();
        let ruido = vec![b'x'; TOPE * 2];
        e.tragar(b"\x1b]133;");
        e.tragar(&ruido);
        assert!(e.cuerpo.len() <= TOPE);
    }
}
