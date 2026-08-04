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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    //  Y lo nuestro, que ssh_config no sabe decir: cómo agrupar, qué correr
    //  nada más entrar, de qué color se pone la terminal mientras estás
    //  dentro, y qué túneles se levantan con la conexión.
    pub etiquetas: Vec<String>,
    pub al_conectar: String,
    pub tinte: String,
    pub tuneles: String,
    //  La contraseña, si este servidor va con contraseña en vez de con clave.
    //  No se escribe en `~/.ssh/config` ni en `hosts.json`: vive en su propio
    //  fichero con 600, ver `ruta_claves`.
    pub contrasena: String,
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
    #[serde(default)]
    tinte: String,
    #[serde(default)]
    tuneles: String,
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

//  ── las contraseñas ───────────────────────────────────────────────
//
//  En su propio fichero y con 600, nunca en `~/.ssh/config` ni en
//  `hosts.json`: esos dos se abren, se enseñan y se copian sin pensarlo, y
//  una contraseña no puede viajar en algo que se trata así.
//
//  Van en claro, y hay que decirlo sin adornos: el trato es el mismo que el
//  de una clave privada sin frase de paso —quien tenga tu usuario, las
//  tiene—. Se guarda aquí porque en este equipo no hay ningún servicio de
//  secretos funcionando: `secret-tool` está, pero el de KDE no arranca.
//  Cuando lo haya, este es el único sitio que hay que cambiar.
//  ¿Está el otro lado pidiendo la contraseña?
//
//  Se mira en minúsculas y solo mientras hay una esperando: fuera de esa
//  ventana esto no se enciende jamás. El «assword» sin la primera letra vale
//  para «Password» y para «password» con una sola comparación, y las otras
//  dos formas son las de un servidor en castellano.
pub fn es_peticion_de_clave(texto: &str) -> bool {
    let t = texto.to_lowercase();
    t.contains("assword:") || t.contains("contrasena:") || t.contains("contraseña:")
}

//  ¿Y es la pregunta de la huella? La de «¿seguro que quieres seguir?» que sale
//  la primera vez que entras a una máquina.
//
//  Para los servidores guardados esto no debería saltar nunca —su bloque lleva
//  `StrictHostKeyChecking accept-new` y ssh no pregunta—, pero sigue haciendo
//  falta: los destinos escritos al vuelo no tienen bloque, y los guardados de
//  antes de esto tampoco.
pub fn es_pregunta_de_huella(texto: &str) -> bool {
    let t = texto.to_lowercase();
    t.contains("(yes/no") && t.contains('?')
}

pub fn ruta_claves() -> PathBuf {
    casa().join(".config/k4term/claves.json")
}

fn claves() -> HashMap<String, String> {
    std::fs::read_to_string(ruta_claves())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn leer_clave(alias: &str) -> String {
    claves().get(alias).cloned().unwrap_or_default()
}

//  Una contraseña vacía BORRA la que hubiera: es la única forma de quitarla
//  desde el formulario, y dejarla ahí porque el campo se vació sería justo
//  lo contrario de lo que se ha pedido.
pub fn guardar_clave(alias: &str, clave: &str) {
    let mut todas = claves();
    if clave.is_empty() {
        todas.remove(alias);
    } else {
        todas.insert(alias.to_string(), clave.to_string());
    }
    escribir_claves(&todas);
}

pub fn borrar_clave(alias: &str) {
    let mut todas = claves();
    if todas.remove(alias).is_some() {
        escribir_claves(&todas);
    }
}

fn escribir_claves(todas: &HashMap<String, String>) {
    let ruta = ruta_claves();
    if let Some(padre) = ruta.parent() {
        let _ = std::fs::create_dir_all(padre);
        permisos(padre, 0o700);
    }
    //  Si se quedan sin ninguna, fuera el fichero: menos sitios donde mirar.
    if todas.is_empty() {
        let _ = std::fs::remove_file(&ruta);
        return;
    }
    if let Ok(texto) = serde_json::to_string_pretty(todas) {
        let _ = std::fs::write(&ruta, texto);
        permisos(&ruta, 0o600);
    }
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
    let guardadas = claves();

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
                //  La contraseña no está en el `ssh_config` ni en los extras:
                //  se busca en su fichero, por alias.
                contrasena: guardadas.get(primero).cloned().unwrap_or_default(),
                tinte: extra.map(|e| e.tinte.clone()).unwrap_or_default(),
                tuneles: extra.map(|e| e.tuneles.clone()).unwrap_or_default(),
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
        //  Un destino escrito al vuelo no está guardado, así que tampoco
        //  tiene contraseña que buscarle.
        contrasena: String::new(),
        favorito: false,
        ultimo: 0,
        etiquetas: Vec::new(),
        al_conectar: String::new(),
        tinte: String::new(),
        tuneles: String::new(),
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

    texto.push_str(&bloque_ssh(servidor));

    std::fs::write(&ruta, texto).map_err(|e| e.to_string())?;
    guardar_extras(servidor);
    guardar_clave(&servidor.alias, &servidor.contrasena);
    //  No lleva secretos, pero dice a qué máquinas entras y con qué usuario.
    permisos(&ruta, 0o600);
    Ok(())
}

//  El bloque que se escribe en `~/.ssh/config`, aparte para poder ejercerlo
//  sin tocar el `~/.ssh` de nadie. Aquí va lo que ssh entiende y NADA más: la
//  contraseña no aparece, y esa es la línea que no se cruza.
pub fn bloque_ssh(servidor: &Servidor) -> String {
    let mut texto = format!("\nHost {}\n", servidor.alias);
    texto.push_str(&format!("    HostName {}\n", servidor.host));
    //  La huella de una máquina nueva se acepta sola; la que CAMBIA sigue
    //  parando la conexión, que es el caso que importa. Va en el bloque y no
    //  en la orden para que lo que se teclea siga siendo `ssh nombre`: ver
    //  pasar una línea de opciones es justo lo que no se quiere ver.
    texto.push_str("    StrictHostKeyChecking accept-new\n");
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
    texto
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
    //  Y su contraseña: dejarla suelta sería guardar el secreto de una máquina
    //  a la que ya no vas.
    borrar_clave(alias);
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

//  El color de un sitio, en la paleta de la casa. Se guarda por NOMBRE y no
//  por hexadecimal porque lo que uno quiere decir es «producción es roja», no
//  «producción es #ff453a» — y así el día que cambie el tema, cambia con él.
pub fn color_del_tinte(nombre: &str) -> Option<(u8, u8, u8)> {
    match nombre.trim().to_lowercase().as_str() {
        "rojo" => Some((0xff, 0x45, 0x3a)),
        "ambar" | "ámbar" | "naranja" => Some((0xff, 0x9f, 0x0a)),
        "verde" => Some((0x30, 0xd1, 0x58)),
        "azul" => Some((0x0a, 0x84, 0xff)),
        "morado" => Some((0xbf, 0x5a, 0xf2)),
        "rosa" => Some((0xff, 0x37, 0x5f)),
        _ => {
            //  Y si alguien escribe un hexadecimal, se respeta: es su
            //  terminal.
            let h = nombre.trim().trim_start_matches('#');
            if h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()) {
                let n = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
                Some((n(0), n(2), n(4)))
            } else {
                None
            }
        }
    }
}

//  El fondo de la terminal, teñido hacia ese color. Poco: lo justo para que
//  se note de reojo y no para pintar una pared — leer sobre un rojo saturado
//  es imposible, y el sentido de esto es saber dónde estás sin dejar de
//  trabajar.
pub fn fondo_tenido(fondo: (u8, u8, u8), tinte: (u8, u8, u8)) -> (u8, u8, u8) {
    const FUERZA: f32 = 0.18;
    let mezcla = |a: u8, b: u8| (a as f32 * (1.0 - FUERZA) + b as f32 * FUERZA).round() as u8;
    (
        mezcla(fondo.0, tinte.0),
        mezcla(fondo.1, tinte.1),
        mezcla(fondo.2, tinte.2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    //  Borrar un host se lleva SU bloque y nada más: ni el de al lado, ni las
    //  líneas sueltas del suyo. `HostName` empieza por «host» y ahí estuvo el
    //  fallo — dejaba la línea huérfana en el fichero.
    #[test]
    fn el_tinte_entiende_nombres_y_hexadecimales() {
        assert_eq!(color_del_tinte("rojo"), Some((0xff, 0x45, 0x3a)));
        assert_eq!(color_del_tinte("  Ámbar "), Some((0xff, 0x9f, 0x0a)));
        assert_eq!(color_del_tinte("#101820"), Some((0x10, 0x18, 0x20)));
        assert_eq!(color_del_tinte("morado claro"), None);
        assert_eq!(color_del_tinte(""), None);

        //  Y teñir es un empujón, no una pared: el fondo sigue siendo fondo.
        let tenido = fondo_tenido((0x1c, 0x1c, 0x1e), (0xff, 0x45, 0x3a));
        assert!(tenido.0 > 0x1c && tenido.0 < 0x60, "quedó {tenido:?}");
    }

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

    //  ── las contraseñas ───────────────────────────────────────────
    //
    //  Lo que se comprueba no es que se guarden, que eso es un HashMap: es
    //  que NO acaben donde no deben. Un secreto que se cuela en el fichero
    //  que todo el mundo abre no avisa de nada.
    //  Y que la contraseña NO se cuele en los ficheros que se abren y se
    //  copian sin pensar. Es lo único que de verdad importa comprobar aquí:
    //  guardarla es un mapa, no perderla de vista es el trabajo.
    #[test]
    fn la_contrasena_no_toca_el_ssh_config() {
        let bloque = bloque_ssh(&Servidor {
            alias: "casa".into(),
            host: "192.168.1.10".into(),
            usuario: "abel".into(),
            contrasena: "secreta123".into(),
            ..Default::default()
        });
        assert!(bloque.contains("HostName 192.168.1.10"));
        //  Y la huella de una máquina nueva se acepta sola: así no sale la
        //  pregunta y no hay que contestarla a la vista de nadie.
        assert!(bloque.contains("StrictHostKeyChecking accept-new"));
        assert!(bloque.contains("User abel"));
        assert!(
            !bloque.contains("secreta123"),
            "la contraseña se ha escrito en el ssh_config"
        );
    }

    #[test]
    fn reconoce_la_pregunta_de_la_huella() {
        assert!(es_pregunta_de_huella(
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        ));
        //  Y no cualquier cosa con un «?» dentro.
        assert!(!es_pregunta_de_huella("¿seguro? escribe algo"));
        assert!(!es_pregunta_de_huella("yes/no"));
    }

    #[test]
    fn piden_prompt_de_contrasena() {
        //  Vale para las tres formas que se ven de verdad.
        for texto in ["abel@lento's password: ", "Password: ", "Contraseña: "] {
            assert!(es_peticion_de_clave(texto), "no reconoce «{texto}»");
        }
        for texto in ["Last login: Mon", "password saved", ""] {
            assert!(!es_peticion_de_clave(texto), "reconoce de más «{texto}»");
        }
    }
}
