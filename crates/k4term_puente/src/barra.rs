//  Hablarle a la barra.
//
//  Por el mismo IPC que usa todo el mundo en k4 —el de quickshell—, así que
//  no hay protocolo nuevo que mantener: si la barra no está, el mandato falla
//  y aquí no se entera nadie, que es exactamente lo que debe pasar.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn shell_qml() -> PathBuf {
    if let Ok(explicita) = std::env::var("K4TERM_SHELL_QML") {
        return PathBuf::from(explicita);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/quickshell/k4/shell.qml")
}

//  Cuánto tiene que llevar corriendo un mandato para que la barra se entere.
//  Por debajo de esto no se dice nada: quien lanza un `ls` no necesita que la
//  isla se lo cuente, y así tampoco pagamos un proceso por cada orden.
pub fn umbral_pildora() -> Duration {
    let segundos = std::env::var("K4TERM_PILDORA_SEGUNDOS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3);
    Duration::from_secs(segundos)
}

fn llamar(argumentos: &[&str]) {
    let qml = shell_qml();
    if !qml.exists() {
        return;
    }

    let _ = Command::new("quickshell")
        .args(["ipc", "-p"])
        .arg(&qml)
        .args(["call", "k4.term"])
        .args(argumentos)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

//  Los segundos que YA lleva: la barra se entera tarde a propósito (hasta que
//  cruza el umbral), y sin este dato su reloj arrancaría en cero y contaría
//  menos de lo que el mandato lleva de verdad.
pub fn avisar_inicio(pid: u32, mandato: &str, segundos: u64) {
    llamar(&[
        "inicio",
        &pid.to_string(),
        mandato,
        &segundos.to_string(),
    ]);
}

pub fn avisar_fin(pid: u32, mandato: &str, salida: i32, segundos: u64) {
    llamar(&[
        "fin",
        &pid.to_string(),
        mandato,
        &salida.to_string(),
        &segundos.to_string(),
    ]);
}

//  Al cerrar la ventana: lo que estuviera en marcha se va con ella, y la
//  barra no tiene por qué quedarse con un indicador de un muerto.
pub fn limpiar(pid: u32) {
    llamar(&["limpiar", &pid.to_string()]);
}

//  «Te está esperando». Una terminal que toca la campana con la ventana sin
//  foco casi siempre es un agente que ha terminado su turno y espera
//  respuesta; que lo diga la isla, que es donde estás mirando.
pub fn avisar_campana(pid: u32, titulo: &str) {
    llamar(&["campana", &pid.to_string(), titulo]);
}

//  Un recado suelto para que la barra lo enseñe: la terminal no tiene dónde
//  decir «guardado» sin taparse a sí misma, y la isla sí.
pub fn decir(titulo: &str, cuerpo: &str) {
    llamar(&["decir", titulo, cuerpo]);
}
