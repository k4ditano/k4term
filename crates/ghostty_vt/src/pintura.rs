//  Con qué dejar una pantalla como estaba en otro sitio.
//
//  El estado de un terminal vive en su memoria y no viaja: cuando una sesión
//  cambia de manos —de la isla a una ventana o al revés— lo que se puede
//  mandar es lo que se ve, escrito en las mismas secuencias que entiende
//  cualquier terminal. El que la recibe se lo mete por el VT y aparece la
//  pantalla de antes.
//
//  Lo que corre dentro, además, se repinta solo en cuanto le cambian el
//  tamaño; esto es para que no haya un parpadeo en negro mientras tanto, y
//  para las sesiones que están en un prompt y no repintan nada.

use crate::Terminal;

//  Cuánto historial se lleva por delante de la pantalla. Todo sería mucho y
//  casi nunca se mira; esto son varias pantallas hacia atrás.
const TOPE_HISTORIAL: u32 = 400;

pub fn repintar(term: &Terminal, filas: u16, titulo: &str) -> String {
    let mut out = String::new();

    //  El nombre primero: quien reciba la sesión debe llamarse igual que
    //  quien la soltó, y el título solo se vuelve a emitir si la aplicación
    //  de dentro repinta.
    if !titulo.is_empty() {
        out.push_str(&format!("\x1b]0;{titulo}\x07"));
    }

    let (arriba, _) = term.viewport_position().unwrap_or((0, 0));
    for f in arriba.saturating_sub(TOPE_HISTORIAL)..arriba {
        if let Some(l) = term.dump_screen_row(f) {
            out.push_str(l.trim_end());
            out.push_str("\r\n");
        }
    }

    //  Hasta dónde se pinta: la última fila con algo escrito, o la del
    //  cursor si va por debajo. Pintar el hueco vacío que quedaba por abajo
    //  no es inocente — al llegar a una isla, ese hueco la abre entera y deja
    //  el texto pegado al fondo con un vacío raro por encima.
    let (col, fila) = term.cursor_position().unwrap_or((1, 1));
    let mut ultima = 0u16;
    for f in 0..filas {
        if !term
            .dump_viewport_row(f)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            ultima = f + 1;
        }
    }
    let hasta = ultima.max(fila).max(1);

    for f in 0..hasta {
        let texto: Vec<char> = term
            .dump_viewport_row(f)
            .unwrap_or_default()
            .chars()
            .collect();

        for run in term.dump_viewport_row_style_runs(f).unwrap_or_default() {
            let ini = usize::from(run.start_col.saturating_sub(1));
            let fin = usize::from(run.end_col).min(texto.len());
            if ini >= fin {
                continue;
            }

            out.push_str(&format!(
                "\x1b[0m\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
                run.fg.r, run.fg.g, run.fg.b, run.bg.r, run.bg.g, run.bg.b
            ));
            //  Los mismos bits que se sirven a la barra: negrita, cursiva,
            //  subrayado y tachado.
            for (bit, sgr) in [
                (0x02, "\x1b[1m"),
                (0x04, "\x1b[3m"),
                (0x08, "\x1b[4m"),
                (0x40, "\x1b[9m"),
            ] {
                if run.flags & bit != 0 {
                    out.push_str(sgr);
                }
            }
            out.extend(&texto[ini..fin]);
        }

        out.push_str("\x1b[0m");
        if f + 1 < hasta {
            out.push_str("\r\n");
        }
    }

    //  Y el cursor donde estaba, pero contado DESDE ABAJO. En absoluto
    //  (`CSI fila;col H`) valdría solo si el sitio nuevo tuviera exactamente
    //  las mismas filas que el viejo: al pasar de una ventana alta a la isla,
    //  colocarlo en la fila 45 es lo que abría la caja entera para nada.
    let subir = hasta.saturating_sub(fila);
    if subir > 0 {
        out.push_str(&format!("\x1b[{subir}A"));
    }
    out.push_str(&format!("\x1b[{col}G"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    //  Lo que fallaba: una pantalla alta y medio vacía llegaba entera, con su
    //  hueco, y al otro lado eso abría la caja del todo con el texto pegado
    //  al fondo. Lo que se manda ahora acaba donde acaba el contenido.
    #[test]
    fn el_hueco_de_abajo_no_viaja() {
        let mut alto = Terminal::new(20, 40).unwrap();
        alto.feed(b"una\r\ndos").unwrap();

        let bytes = repintar(&alto, 40, "");
        assert_eq!(bytes.matches("\r\n").count(), 1);

        //  Y al llegar a un sitio bajo, el cursor queda pegado al texto.
        let mut bajo = Terminal::new(20, 6).unwrap();
        bajo.feed(bytes.as_bytes()).unwrap();
        assert_eq!(bajo.cursor_position().unwrap(), (4, 2));
    }

    #[test]
    fn la_pantalla_se_repinta_en_otro_terminal() {
        let mut uno = Terminal::new(20, 3).unwrap();
        uno.feed(b"\x1b[31mrojo\x1b[0m normal\r\nsegunda").unwrap();

        let bytes = repintar(&uno, 3, "prueba");

        let mut otro = Terminal::new(20, 3).unwrap();
        otro.feed(bytes.as_bytes()).unwrap();

        assert_eq!(otro.dump_viewport_row(0).unwrap().trim_end(), "rojo normal");
        assert_eq!(otro.dump_viewport_row(1).unwrap().trim_end(), "segunda");
        assert_eq!(otro.title().as_deref(), Some("prueba"));
        //  Y el color sobrevive al viaje: el primer tramo pinta igual en las
        //  dos, sea cual sea el rojo que traiga la paleta.
        let de_uno = uno.dump_viewport_row_style_runs(0).unwrap();
        let de_otro = otro.dump_viewport_row_style_runs(0).unwrap();
        assert_eq!(de_uno[0].fg, de_otro[0].fg);
        assert_ne!(de_otro[0].fg, de_otro[1].fg);
    }
}
