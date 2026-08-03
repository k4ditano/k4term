//  El Stream de ghostty despacha con `@hasDecl`: si el handler del puente no
//  declara un método, la secuencia se descarta EN SILENCIO. No hay error, no
//  hay aviso, y el terminal simplemente se porta mal.
//
//  Estas pruebas son la red para eso. Cada una ejerce una secuencia que
//  depende de un método distinto del handler, así que si alguien lo quita de
//  crates/ghostty_vt_sys/zig/lib.zig salta aquí y no en la cara del usuario.
//
//  La primera de todas es la que costó una sesión entera de terminal con un
//  `%` de sobra al arrancar: sin `restoreCursor`, zsh no podía volver a la
//  columna 1 para tapar su marca de fin de línea.

use ghostty_vt::Terminal;

#[test]
fn guarda_y_restaura_el_cursor() {
    let mut t = Terminal::new(10, 2).unwrap();

    //  ESC 7 guarda en la columna 3, ESC 8 vuelve ahí.
    t.feed(b"ABC\x1b7XYZ\x1b8Q").unwrap();

    let fila = t.dump_viewport_row(0).unwrap();
    assert_eq!(fila.trim_end(), "ABCQYZ", "fila={fila:?}");
}

#[test]
fn el_prompt_de_zsh_no_deja_marca() {
    //  Lo que manda zsh de verdad al redibujar el prompt, reducido: se coloca,
    //  guarda, escribe, restaura, borra y vuelve a escribir. Sin DECSC/DECRC
    //  la marca `%` se quedaba escrita y el prompt caía una línea más abajo.
    let mut t = Terminal::new(20, 3).unwrap();
    t.feed(b"\x1b7~ > \x1b8\x1b[J%\r\x1b[K~ > ").unwrap();

    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "~ >");
    assert_eq!(t.dump_viewport_row(1).unwrap().trim_end(), "");
    assert_eq!(t.cursor_position(), Some((5, 1)), "el cursor baja de línea");
}

#[test]
fn indice_y_indice_inverso() {
    let mut t = Terminal::new(10, 3).unwrap();

    //  IND baja una línea sin volver al margen; NEL baja y vuelve.
    t.feed(b"AB\x1bDC\x1bED").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "AB");
    assert_eq!(t.dump_viewport_row(1).unwrap().trim_end(), "  C");
    assert_eq!(t.dump_viewport_row(2).unwrap().trim_end(), "D");

    //  RI sube y CONSERVA la columna —no vuelve al margen, eso es NEL—, que
    //  es como `less` recorre hacia atrás. El cursor venía de la columna 1
    //  tras escribir la D, así que la X cae ahí.
    t.feed(b"\x1bMX").unwrap();
    assert_eq!(t.dump_viewport_row(1).unwrap().trim_end(), " XC");
}

#[test]
fn region_de_desplazamiento() {
    //  DECSTBM: lo que usan vim y less para mover solo el cuerpo y dejar
    //  quietas las líneas de estado. Sin `setTopAndBottomMargin` la región no
    //  se fija y desplazar arrastra la pantalla entera.
    let mut t = Terminal::new(10, 4).unwrap();
    t.feed(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD").unwrap();

    //  Región de la 2 a la 3, y un desplazamiento dentro.
    t.feed(b"\x1b[2;3r\x1b[3;1H\x1bD").unwrap();

    assert_eq!(
        t.dump_viewport_row(0).unwrap().trim_end(),
        "A",
        "la 1 no se toca"
    );
    assert_eq!(t.dump_viewport_row(1).unwrap().trim_end(), "C");
    assert_eq!(
        t.dump_viewport_row(3).unwrap().trim_end(),
        "D",
        "la 4 no se toca"
    );
}

#[test]
fn insertar_y_borrar_lineas() {
    let mut t = Terminal::new(10, 3).unwrap();
    t.feed(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC").unwrap();

    //  IL en la primera: baja todo.
    t.feed(b"\x1b[1;1H\x1b[L").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "");
    assert_eq!(t.dump_viewport_row(1).unwrap().trim_end(), "A");

    //  Y DL la deshace.
    t.feed(b"\x1b[1;1H\x1b[M").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "A");
}

#[test]
fn borrar_e_insertar_caracteres() {
    let mut t = Terminal::new(12, 1).unwrap();

    //  DCH: quita caracteres y arrastra el resto a la izquierda.
    t.feed(b"ABCDEF\x1b[1;3H\x1b[2P").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "ABEF");

    //  ECH: los borra en el sitio, sin arrastrar nada.
    t.feed(b"\x1b[1;1H\x1b[2X").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "  EF");

    //  ICH: hace hueco. Los dos blancos entran delante y la Z ocupa el
    //  primero de ellos, así que quedan tres antes de la E y no cuatro.
    t.feed(b"\x1b[1;1H\x1b[2@Z").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "Z   EF");
}

#[test]
fn cursor_relativo_y_repeticion() {
    let mut t = Terminal::new(12, 2).unwrap();

    //  CUF/HPR y VPR: moverse en relativo.
    t.feed(b"A\x1b[3aB").unwrap();
    assert_eq!(t.dump_viewport_row(0).unwrap().trim_end(), "A   B");

    //  REP: repite el último carácter impreso.
    t.feed(b"\x1b[2;1HX\x1b[3b").unwrap();
    assert_eq!(t.dump_viewport_row(1).unwrap().trim_end(), "XXXX");
}
