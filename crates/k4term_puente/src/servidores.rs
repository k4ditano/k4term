//  Los servidores de uno, leídos de donde viven.
//
//  Los hosts salen de `~/.ssh/config` y no de una base de datos nuestra: es lo
//  que hace que guardar en k4 sirva también para `ssh` a pelo, `scp`, `git` y
//  `rsync`. Lo que ese fichero no sabe decir —favoritos, cuándo entraste—
//  vive aparte, en `~/.config/k4term/hosts.json`, para no ensuciar una
//  configuración que leen otros programas.
//
//  Esto lo lee también la barra, con su propio analizador en QML. Son dos
//  lectores del mismo fichero y es a propósito: el formato no es nuestro —es
//  `ssh_config`, que lleva ahí treinta años— así que no hay un contrato que
//  se pueda romper por un lado. Compartir el código costaría que la barra
//  dependiera de este binario, y la barra tiene que saber abrir servidores
//  aunque no esté k4term.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Servidor {
    pub alias: String,
    pub host: String,
    pub usuario: String,
    pub puerto: String,
    pub favorito: bool,
    pub ultimo: u64,
}

impl Servidor {
    //  Lo que se enseña debajo del nombre: a dónde va de verdad.
    pub fn detalle(&self) -> String {
        let usuario = if self.usuario.is_empty() {
            String::new()
        } else {
            format!("{}@", self.usuario)
        };
        let puerto = if self.puerto.is_empty() || self.puerto == "22" {
            String::new()
        } else {
            format!(":{}", self.puerto)
        };
        format!("{usuario}{}{puerto}", self.host)
    }
}

#[derive(Debug, Default, Deserialize)]
struct Extra {
    #[serde(default)]
    favorito: bool,
    #[serde(default)]
    ultimo: u64,
}

fn casa() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn ruta_ssh() -> PathBuf {
    casa().join(".ssh/config")
}

pub fn ruta_extras() -> PathBuf {
    casa().join(".config/k4term/hosts.json")
}

//  Todos los servidores, ordenados como se usan: primero los favoritos,
//  después por cuándo entraste —lo de ayer suele ser lo de hoy— y al final
//  por nombre.
pub fn leer() -> Vec<Servidor> {
    let texto = std::fs::read_to_string(ruta_ssh()).unwrap_or_default();
    let extras: HashMap<String, Extra> = std::fs::read_to_string(ruta_extras())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    let mut salida: Vec<Servidor> = Vec::new();

    for linea in texto.lines() {
        //  Los comentarios fuera, y la separación puede ser espacio o igual:
        //  `Port 22` y `Port=22` son lo mismo para ssh.
        let limpia = match linea.split('#').next() {
            Some(l) => l.trim(),
            None => continue,
        };
        if limpia.is_empty() {
            continue;
        }
        let Some(corte) = limpia.find([' ', '\t', '=']) else {
            continue;
        };
        let clave = limpia[..corte].to_lowercase();
        let valor = limpia[corte..].trim_start_matches([' ', '\t', '=']).trim();

        if clave == "host" {
            //  Los patrones (`Host *`) son valores por defecto, no sitios a
            //  los que ir: no se enseñan.
            let primero = valor.split_whitespace().next().unwrap_or_default();
            if primero.is_empty() || primero.contains('*') || primero.contains('?') {
                continue;
            }
            let extra = extras.get(primero);
            salida.push(Servidor {
                alias: primero.to_string(),
                host: primero.to_string(),
                usuario: String::new(),
                puerto: String::new(),
                favorito: extra.map(|e| e.favorito).unwrap_or(false),
                ultimo: extra.map(|e| e.ultimo).unwrap_or(0),
            });
            continue;
        }

        let Some(actual) = salida.last_mut() else {
            continue;
        };
        match clave.as_str() {
            "hostname" => actual.host = valor.to_string(),
            "user" => actual.usuario = valor.to_string(),
            "port" => actual.puerto = valor.to_string(),
            _ => {}
        }
    }

    salida.sort_by(|a, b| {
        b.favorito
            .cmp(&a.favorito)
            .then(b.ultimo.cmp(&a.ultimo))
            .then(a.alias.cmp(&b.alias))
    });
    salida
}

//  Apuntar que se ha entrado, para que la lista se ordene por uso también
//  cuando conectas desde la ventana. Se lee y se reescribe entero: son cuatro
//  claves y así no hace falta un formato nuestro con más reglas.
pub fn visitado(alias: &str, ahora: u64) {
    let ruta = ruta_extras();
    let mut mapa: serde_json::Value = std::fs::read_to_string(&ruta)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if !mapa.is_object() {
        mapa = serde_json::json!({});
    }
    let entrada = mapa
        .as_object_mut()
        .and_then(|m| {
            m.entry(alias.to_string())
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        })
        .map(|e| {
            e.insert("ultimo".to_string(), serde_json::json!(ahora));
        });
    if entrada.is_none() {
        return;
    }

    if let Some(padre) = ruta.parent() {
        let _ = std::fs::create_dir_all(padre);
    }
    if let Ok(texto) = serde_json::to_string_pretty(&mapa) {
        let _ = std::fs::write(&ruta, texto + "\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_analizador_lee_lo_que_hace_falta_y_deja_lo_demas() {
        //  Un fichero como los de verdad: comentarios, patrones, sangrías
        //  distintas y opciones que no nos interesan.
        let crudo = "\
# mi configuración
Host *
    ServerAliveInterval 60

Host casa
    HostName nas.local
    User abel
    Port=2222

Host trabajo   trabajo.corto
  HostName 10.0.0.9
  IdentityFile ~/.ssh/id_ed25519
";
        let ruta = std::env::temp_dir().join("k4term-prueba-ssh-config");
        std::fs::write(&ruta, crudo).unwrap();
        let texto = std::fs::read_to_string(&ruta).unwrap();
        std::fs::remove_file(&ruta).ok();

        //  Se ejerce el mismo recorrido que `leer`, sin depender del HOME de
        //  quien corra la prueba.
        let mut vistos = Vec::new();
        for linea in texto.lines() {
            let limpia = linea.split('#').next().unwrap_or("").trim();
            if limpia.is_empty() {
                continue;
            }
            let Some(corte) = limpia.find([' ', '\t', '=']) else {
                continue;
            };
            if limpia[..corte].to_lowercase() == "host" {
                let valor = limpia[corte..].trim_start_matches([' ', '\t', '=']).trim();
                let primero = valor.split_whitespace().next().unwrap_or_default();
                if !primero.contains('*') && !primero.contains('?') {
                    vistos.push(primero.to_string());
                }
            }
        }

        assert_eq!(vistos, vec!["casa", "trabajo"]);
    }
}
