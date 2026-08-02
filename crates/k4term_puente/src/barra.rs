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

//  Cuánto tiene que durar un mandato para merecer un aviso. Por debajo de
//  esto no se avisa: quien lanza algo de tres segundos sigue mirando.
pub fn umbral_aviso() -> Duration {
    let segundos = std::env::var("K4TERM_AVISO_SEGUNDOS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(20);
    Duration::from_secs(segundos)
}

pub fn avisar_trabajo(mandato: &str, salida: i32, segundos: u64) {
    let qml = shell_qml();
    if !qml.exists() {
        return;
    }

    let _ = Command::new("quickshell")
        .args(["ipc", "-p"])
        .arg(&qml)
        .args([
            "call",
            "k4.term",
            "trabajo",
            mandato,
            &salida.to_string(),
            &segundos.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
