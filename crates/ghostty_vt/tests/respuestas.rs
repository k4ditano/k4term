//  Un terminal no solo recibe: le preguntan, y quien pregunta espera.
//
//  Estas pruebas fijan que las respuestas salen y con el formato de ghostty.
//  Sin ellas, un TUI que consulta el terminal se queda esperando algo que no
//  llega y se coloca a ciegas.

use ghostty_vt::Terminal;

#[test]
fn contesta_quien_es() {
    let mut t = Terminal::new(20, 4).unwrap();
    t.feed(b"\x1b[c").unwrap();
    //  VT220 con color, igual que ghostty.
    assert_eq!(t.take_responses(), b"\x1b[?62;22c");
    //  Y la cola se vacía: no se contesta dos veces a la misma pregunta.
    assert!(t.take_responses().is_empty());
}

#[test]
fn contesta_donde_esta_el_cursor() {
    let mut t = Terminal::new(20, 6).unwrap();
    t.feed(b"\x1b[3;7H").unwrap();
    t.feed(b"\x1b[6n").unwrap();
    assert_eq!(t.take_responses(), b"\x1b[3;7R");
}

#[test]
fn contesta_si_esta_vivo() {
    let mut t = Terminal::new(20, 4).unwrap();
    t.feed(b"\x1b[5n").unwrap();
    assert_eq!(t.take_responses(), b"\x1b[0n");
}

#[test]
fn contesta_por_un_modo() {
    let mut t = Terminal::new(20, 4).unwrap();
    //  El pegado con corchetes: apagado de fábrica, encendido después.
    t.feed(b"\x1b[?2004$p").unwrap();
    assert_eq!(t.take_responses(), b"\x1b[?2004;2$y");
    t.feed(b"\x1b[?2004h\x1b[?2004$p").unwrap();
    assert_eq!(t.take_responses(), b"\x1b[?2004;1$y");
}

#[test]
fn sin_preguntas_no_hay_respuestas() {
    let mut t = Terminal::new(20, 4).unwrap();
    t.feed(b"hola\r\nque tal").unwrap();
    assert!(t.take_responses().is_empty());
}

#[test]
fn recuerda_el_titulo_que_le_pidan() {
    let mut t = Terminal::new(20, 4).unwrap();
    assert_eq!(t.title(), None, "de recién nacido no tiene título");

    //  OSC 2: el título de la ventana, como lo pone claude o un `printf`.
    t.feed(b"\x1b]2;claude\x07").unwrap();
    assert_eq!(t.title().as_deref(), Some("claude"));

    //  Y se queda con el último, que es lo que interesa.
    t.feed(b"\x1b]2;codex\x07").unwrap();
    assert_eq!(t.title().as_deref(), Some("codex"));
}
