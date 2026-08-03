//  El ratón, dicho como lo espera la aplicación de dentro.
//
//  Un terminal no «hace clic»: le cuenta a quien corre dentro dónde y con qué
//  botón, y el programa decide. Hay dos idiomas y hay que hablar los dos:
//
//    · SGR (DECSET 1006) — `ESC [ < b ; col ; fila M|m`. El moderno: los
//      números van en decimal, así que vale para cualquier tamaño y distingue
//      soltar de pulsar. Lo pide todo lo que se ha escrito en este siglo.
//    · X10, el de siempre — `ESC [ M` y tres bytes con 32 sumado. Se rompe
//      pasada la columna 223 y al soltar no dice qué botón era; se manda solo
//      cuando la aplicación no ha pedido SGR.
//
//  Vive en esta capa —y no en cada terminal— por lo mismo que los modos: la
//  ventana y la isla tienen que contarle a htop exactamente lo mismo.

use crate::KeyModifiers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boton {
    Izquierdo,
    Medio,
    Derecho,
    RuedaArriba,
    RuedaAbajo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suceso {
    Pulsar,
    Soltar,
    //  Arrastrar con un botón pulsado. Moverse sin botón también es esto, con
    //  el botón «ninguno», pero eso solo se manda en modo 1003 y de eso decide
    //  quien llama.
    Mover,
}

impl Boton {
    fn base(self) -> u8 {
        match self {
            Boton::Izquierdo => 0,
            Boton::Medio => 1,
            Boton::Derecho => 2,
            Boton::RuedaArriba => 64,
            Boton::RuedaAbajo => 65,
        }
    }

    fn es_rueda(self) -> bool {
        matches!(self, Boton::RuedaArriba | Boton::RuedaAbajo)
    }
}

fn modificadores(mods: KeyModifiers) -> u8 {
    let mut bits = 0u8;
    if mods.shift {
        bits |= 4;
    }
    if mods.alt {
        bits |= 8;
    }
    if mods.control {
        bits |= 16;
    }
    bits
}

//  `None` cuando ese suceso no se cuenta: la rueda no se suelta, y en X10 una
//  columna más allá de la 223 no cabe en un byte y mandarla sería mentir.
pub fn encode_mouse(
    suceso: Suceso,
    boton: Boton,
    col: u16,
    fila: u16,
    mods: KeyModifiers,
    sgr: bool,
) -> Option<Vec<u8>> {
    if boton.es_rueda() && suceso != Suceso::Pulsar {
        return None;
    }

    let col = col.max(1);
    let fila = fila.max(1);

    let mut codigo = boton.base() + modificadores(mods);
    if suceso == Suceso::Mover {
        codigo += 32;
    }

    if sgr {
        let letra = if suceso == Suceso::Soltar { 'm' } else { 'M' };
        return Some(format!("\x1b[<{codigo};{col};{fila}{letra}").into_bytes());
    }

    //  En el idioma viejo, soltar es «el botón 3»: no se sabe cuál era.
    if suceso == Suceso::Soltar {
        codigo = 3 + modificadores(mods);
    }

    if col > 223 || fila > 223 {
        return None;
    }

    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + codigo,
        32 + col as u8,
        32 + fila as u8,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NADA: KeyModifiers = KeyModifiers {
        shift: false,
        control: false,
        alt: false,
        super_key: false,
    };

    #[test]
    fn sgr_press_and_release_differ() {
        let pulsar = encode_mouse(Suceso::Pulsar, Boton::Izquierdo, 5, 3, NADA, true).unwrap();
        let soltar = encode_mouse(Suceso::Soltar, Boton::Izquierdo, 5, 3, NADA, true).unwrap();
        assert_eq!(pulsar, b"\x1b[<0;5;3M".to_vec());
        assert_eq!(soltar, b"\x1b[<0;5;3m".to_vec());
    }

    #[test]
    fn sgr_drag_adds_motion_bit_and_modifiers() {
        let mods = KeyModifiers {
            control: true,
            ..NADA
        };
        let mover = encode_mouse(Suceso::Mover, Boton::Derecho, 1, 1, mods, true).unwrap();
        //  2 (derecho) + 16 (control) + 32 (movimiento) = 50
        assert_eq!(mover, b"\x1b[<50;1;1M".to_vec());
    }

    #[test]
    fn wheel_only_reports_press() {
        assert!(encode_mouse(Suceso::Pulsar, Boton::RuedaArriba, 2, 2, NADA, true).is_some());
        assert!(encode_mouse(Suceso::Soltar, Boton::RuedaArriba, 2, 2, NADA, true).is_none());
    }

    #[test]
    fn legacy_release_loses_the_button_and_bounds_the_grid() {
        let soltar = encode_mouse(Suceso::Soltar, Boton::Medio, 4, 2, NADA, false).unwrap();
        assert_eq!(soltar, vec![0x1b, b'[', b'M', 32 + 3, 32 + 4, 32 + 2]);
        assert!(encode_mouse(Suceso::Pulsar, Boton::Izquierdo, 400, 2, NADA, false).is_none());
    }
}
