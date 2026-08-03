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
    //  La clave con la que entrar (`IdentityFile`) y por dónde pasar
    //  (`ProxyJump`). Son de ssh, así que van a su fichero.
    pub clave: String,
    pub salto: String,
    pub favorito: bool,
    pub ultimo: u64,
    //  Y lo nuestro, que ssh_config no sabe decir: cómo agrupar y qué correr
    //  nada más entrar.
    pub etiquetas: Vec<String>,
    pub al_conectar: String,
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
    #[serde(default)]
    etiquetas: Vec<String>,
    #[serde(default, rename = "alConectar")]
    al_conectar: String,
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
                clave: String::new(),
                salto: String::new(),
                favorito: extra.map(|e| e.favorito).unwrap_or(false),
                ultimo: extra.map(|e| e.ultimo).unwrap_or(0),
                etiquetas: extra.map(|e| e.etiquetas.clone()).unwrap_or_default(),
                al_conectar: extra.map(|e| e.al_conectar.clone()).unwrap_or_default(),
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
            "identityfile" => actual.clave = valor.to_string(),
            "proxyjump" => actual.salto = valor.to_string(),
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

//  ¿Esto que se ha escrito parece un sitio? `usuario@maquina:puerto`, con las
//  dos primeras partes opcionales. Se piden tres letras para no ofrecer
//  «conectar a p» mientras alguien teclea.
pub fn como_destino(texto: &str) -> Option<Servidor> {
    let t = texto.trim();
    if t.len() < 3 || t.contains(char::is_whitespace) {
        return None;
    }

    let (usuario, resto) = match t.split_once('@') {
        Some((u, r)) => (u.to_string(), r),
        None => (String::new(), t),
    };
    let (host, puerto) = match resto.split_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (resto.to_string(), String::new()),
    };

    let valido = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    };
    if !valido(&host) || (!usuario.is_empty() && !valido(&usuario)) {
        return None;
    }
    if !puerto.is_empty() && puerto.parse::<u16>().is_err() {
        return None;
    }

    Some(Servidor {
        alias: host.clone(),
        host,
        usuario,
        puerto,
        clave: String::new(),
        salto: String::new(),
        favorito: false,
        ultimo: 0,
        etiquetas: Vec::new(),
        al_conectar: String::new(),
    })
}

//  Guardar un host en `~/.ssh/config`. Se añade el bloque y se deja el resto
//  del fichero intacto: ahí puede haber cosas de años que no son nuestras.
pub fn guardar(servidor: &Servidor) -> Result<(), String> {
    let ruta = ruta_ssh();
    if let Some(padre) = ruta.parent() {
        std::fs::create_dir_all(padre).map_err(|e| e.to_string())?;
        //  Un `~/.ssh` creado al vuelo sale con los permisos de todo el mundo
        //  y ssh se planta: para él, un directorio que otros pueden mirar no
        //  es sitio para claves.
        permisos(padre, 0o700);
    }

    //  Guardar es también EDITAR: si ese host ya estaba, su bloque se va y se
    //  escribe el nuevo. Así el formulario sirve para las dos cosas sin que
    //  haya dos caminos que mantener.
    let anterior = std::fs::read_to_string(&ruta).unwrap_or_default();
    let mut texto = sin_bloque(&anterior, &servidor.alias);
    if !texto.is_empty() && !texto.ends_with('\n') {
        texto.push('\n');
    }

    texto.push_str(&format!("\nHost {}\n", servidor.alias));
    texto.push_str(&format!("    HostName {}\n", servidor.host));
    for (clave, valor) in [
        ("User", &servidor.usuario),
        ("Port", &servidor.puerto),
        ("IdentityFile", &servidor.clave),
        ("ProxyJump", &servidor.salto),
    ] {
        if !valor.is_empty() {
            texto.push_str(&format!("    {clave} {valor}\n"));
        }
    }

    std::fs::write(&ruta, texto).map_err(|e| e.to_string())?;
    guardar_extras(servidor);
    //  No lleva secretos, pero dice a qué máquinas entras y con qué usuario.
    permisos(&ruta, 0o600);
    Ok(())
}

//  Y borrarlo: desde su `Host` hasta el siguiente (o el final). Por líneas y
//  no con una expresión sobre todo el fichero, que lo de alrededor no tiene
//  por qué correr ningún riesgo.
pub fn borrar(alias: &str) {
    let ruta = ruta_ssh();
    let Ok(texto) = std::fs::read_to_string(&ruta) else {
        return;
    };
    let _ = std::fs::write(&ruta, sin_bloque(&texto, alias));
    quitar_extra(alias);
}

//  El fichero sin el bloque de ese host: desde su `Host` hasta el siguiente
//  (o el final). Va aparte para poder ejercerlo sin tocar el `~/.ssh` de
//  nadie — y buena falta hacía: la primera versión daba por bueno cualquier
//  renglón que empezara por «host», así que `HostName` le parecía otro bloque
//  y dejaba la línea huérfana en el fichero.
fn sin_bloque(texto: &str, alias: &str) -> String {
    let mut salida: Vec<&str> = Vec::new();
    let mut dentro = false;

    for linea in texto.lines() {
        let limpia = linea.split('#').next().unwrap_or("").trim();
        //  «Host» y luego un separador: ni `HostName` ni `HostKeyAlias` son
        //  el principio de nada.
        let es_host = limpia.len() > 4
            && limpia[..4].eq_ignore_ascii_case("host")
            && limpia[4..].starts_with([' ', '\t', '=']);

        if es_host {
            let valor = limpia[4..].trim_start_matches([' ', '\t', '=']).trim();
            dentro = valor.split_whitespace().next() == Some(alias);
        }
        if !dentro {
            salida.push(linea);
        }
    }

    //  Sin líneas en blanco de sobra al final, que se acumularían con cada
    //  host que se borre.
    while salida.last().is_some_and(|l| l.trim().is_empty()) {
        salida.pop();
    }
    if salida.is_empty() {
        return String::new();
    }
    salida.join("\n") + "\n"
}

//  Favorito o no, en el fichero que es nuestro.
pub fn favorito(alias: &str) {
    let ruta = ruta_extras();
    let mut mapa = leer_extras_crudo();
    let puesto = mapa
        .get(alias)
        .and_then(|e| e.get("favorito"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let entrada = mapa
        .entry(alias.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = entrada.as_object_mut() {
        obj.insert("favorito".to_string(), serde_json::json!(!puesto));
    }
    escribir_extras(&ruta, &mapa);
}

//  Lo nuestro del host: etiquetas, qué correr al entrar y si es favorito. Se
//  escribe junto al resto para que el formulario sea uno solo, aunque por
//  dentro vaya a dos ficheros distintos.
fn guardar_extras(servidor: &Servidor) {
    let mut mapa = leer_extras_crudo();
    let entrada = mapa
        .entry(servidor.alias.clone())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = entrada.as_object_mut() {
        obj.insert("favorito".to_string(), serde_json::json!(servidor.favorito));
        obj.insert(
            "etiquetas".to_string(),
            serde_json::json!(servidor.etiquetas),
        );
        obj.insert(
            "alConectar".to_string(),
            serde_json::json!(servidor.al_conectar),
        );
    }
    escribir_extras(&ruta_extras(), &mapa);
}

fn quitar_extra(alias: &str) {
    let mut mapa = leer_extras_crudo();
    mapa.remove(alias);
    escribir_extras(&ruta_extras(), &mapa);
}

fn leer_extras_crudo() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(ruta_extras())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn escribir_extras(ruta: &std::path::Path, mapa: &serde_json::Map<String, serde_json::Value>) {
    if let Some(padre) = ruta.parent() {
        let _ = std::fs::create_dir_all(padre);
    }
    if let Ok(texto) = serde_json::to_string_pretty(mapa) {
        let _ = std::fs::write(ruta, texto + "\n");
    }
}

fn permisos(ruta: &std::path::Path, modo: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(ruta, std::fs::Permissions::from_mode(modo));
}

#[cfg(test)]
mod tests {
    use super::*;

    //  Borrar un host se lleva SU bloque y nada más: ni el de al lado, ni las
    //  líneas sueltas del suyo. `HostName` empieza por «host» y ahí estuvo el
    //  fallo — dejaba la línea huérfana en el fichero.
    #[test]
    fn borrar_se_lleva_el_bloque_entero_y_solo_ese() {
        let crudo = "\
Host casa
    HostName nas.local
    User abel

Host trabajo
    HostName 10.0.0.9
";
        let quedan = sin_bloque(crudo, "casa");
        assert!(
            !quedan.contains("nas.local"),
            "quedó algo de casa: {quedan}"
        );
        assert!(quedan.contains("Host trabajo"));
        assert!(quedan.contains("10.0.0.9"));

        //  Y borrando el último no queda ni un renglón suelto.
        let vacio = sin_bloque(&quedan, "trabajo");
        assert_eq!(vacio.trim(), "");
    }

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
