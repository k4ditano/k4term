//  Mandar cosas a Edinot, si lo tienes.
//
//  Edinot no trae un mandato de consola para escribir notas, pero sí un
//  servidor MCP que habla JSON-RPC por la entrada y la salida estándar. Eso
//  es todo lo que hace falta: se lanza, se saluda, se pide y se cierra. Ni
//  protocolo nuevo ni tocar su base de datos por debajo, ni —importante— su
//  token de la nube: esto es todo local, contra la aplicación que ya tienes
//  abierta.
//
//  **Y solo si está.** Se comprueba antes de ofrecer nada: quien no tenga
//  Edinot no debe enterarse de que esto existe, igual que k4 sigue
//  funcionando sin k4term.
//
//  Tres cosas que costaron un rato averiguar y que aquí no se pueden tocar:
//
//    1. Sin `EDINOT_AGENT_KIND` en el entorno, las llamadas de verdad se
//       quedan colgadas para siempre. El saludo contesta igual, así que no
//       se nota hasta que pides algo.
//    2. Cerrar la entrada es decirle «hemos terminado»: hay que dejarla
//       abierta hasta tener las respuestas, o el servidor se va antes de
//       contestar.
//    3. La primera llamada tarda —el servidor tiene que engancharse a la
//       aplicación viva—, así que la espera es de treinta segundos y no de
//       los dos que uno pondría a ojo.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const ESPERA: Duration = Duration::from_secs(30);

pub fn ruta_servidor() -> PathBuf {
    if let Ok(explicita) = std::env::var("K4TERM_EDINOT_MCP") {
        return PathBuf::from(explicita);
    }
    let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.config")
    });
    PathBuf::from(base).join("edinot/mcp-server.mjs")
}

fn hay_node() -> bool {
    Command::new("node")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn disponible() -> bool {
    ruta_servidor().exists() && hay_node()
}

struct Sesion {
    hijo: Child,
    lineas: Receiver<String>,
    siguiente: u64,
}

impl Sesion {
    fn abrir() -> Result<Self, String> {
        let servidor = ruta_servidor();
        if !servidor.exists() {
            return Err("no hay servidor de Edinot".into());
        }

        let mut hijo = Command::new("node")
            .arg(&servidor)
            .env("EDINOT_AGENT_KIND", "k4term")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("no arrancó el servidor: {e}"))?;

        //  Leer en otro hilo para poder esperar con reloj: el servidor tarda
        //  lo suyo en engancharse a la aplicación y aquí no se puede uno
        //  quedar bloqueado para siempre.
        let salida = hijo.stdout.take().ok_or("sin salida")?;
        let (tx, lineas) = channel();
        std::thread::spawn(move || {
            for linea in BufReader::new(salida).lines() {
                let Ok(linea) = linea else { break };
                if tx.send(linea).is_err() {
                    break;
                }
            }
        });

        let mut sesion = Sesion {
            hijo,
            lineas,
            siguiente: 1,
        };

        sesion.escribir(&serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "k4term", "version": "0.1" }
            }
        }))?;
        sesion.esperar(0)?;
        sesion.escribir(&serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }))?;

        Ok(sesion)
    }

    fn escribir(&mut self, valor: &serde_json::Value) -> Result<(), String> {
        let entrada = self.hijo.stdin.as_mut().ok_or("sin entrada")?;
        writeln!(entrada, "{}", valor).map_err(|e| e.to_string())?;
        entrada.flush().map_err(|e| e.to_string())
    }

    fn esperar(&self, id: u64) -> Result<serde_json::Value, String> {
        let limite = std::time::Instant::now() + ESPERA;
        loop {
            let queda = limite.saturating_duration_since(std::time::Instant::now());
            if queda.is_zero() {
                return Err("Edinot no contestó".into());
            }
            let linea = self
                .lineas
                .recv_timeout(queda)
                .map_err(|_| "Edinot no contestó".to_string())?;
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&linea) else {
                continue;
            };
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(e) = v.get("error") {
                    return Err(e.to_string());
                }
                return Ok(v);
            }
        }
    }

    fn llamar(
        &mut self,
        herramienta: &str,
        argumentos: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.siguiente;
        self.siguiente += 1;
        self.escribir(&serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": herramienta, "arguments": argumentos }
        }))?;
        self.esperar(id)
    }
}

impl Drop for Sesion {
    fn drop(&mut self) {
        drop(self.hijo.stdin.take());
        let _ = self.hijo.wait();
    }
}

//  Las respuestas vienen como un texto pensado para que lo lea un agente,
//  con el JSON de verdad dentro.
fn dentro(respuesta: &serde_json::Value) -> Option<serde_json::Value> {
    let texto = respuesta
        .get("result")?
        .get("content")?
        .as_array()?
        .first()?
        .get("text")?
        .as_str()?;
    serde_json::from_str(texto).ok()
}

//  Añade a la nota del día —que es donde uno busca luego «qué hice el
//  martes»— y devuelve su nombre para poder decirlo por ahí.
pub fn anotar(titulo: &str, cuerpo: &str) -> Result<String, String> {
    if !disponible() {
        return Err("Edinot no está".into());
    }

    let mut sesion = Sesion::abrir()?;

    let hoy = sesion.llamar("get_or_create_daily_note", serde_json::json!({}))?;
    let nombre = dentro(&hoy)
        .and_then(|v| {
            v.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .ok_or("no se pudo abrir la nota del día")?;

    //  En bloque de código, que la salida de una terminal se lea como lo que
    //  es y no se coma los guiones y asteriscos por el camino.
    let contenido = format!("\n## {titulo}\n\n```\n{}\n```\n", cuerpo.trim_end());
    sesion.llamar(
        "append_note",
        serde_json::json!({ "name": nombre, "content": contenido }),
    )?;

    Ok(nombre)
}
