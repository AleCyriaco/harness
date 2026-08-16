//! Anexos: imagem colada ou arquivo solto vira parte da mensagem.
//!
//! Imagem viaja como `data:` URL dentro do corpo da chamada (é o que as APIs
//! compatíveis com OpenAI aceitam). Base64 é feito aqui à mão: somar uma crate
//! para 20 linhas contraria a regra do projeto.

/// Extensões que o modelo consegue **ver**. O resto é anexo de caminho.
pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

pub fn is_image(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn mime_of(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `data:image/png;base64,...` de um arquivo. `None` se não der para ler ou se
/// passar do teto — imagem gigante estoura o limite da requisição.
pub fn data_url(path: &std::path::Path, max_bytes: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() || bytes.len() > max_bytes {
        return None;
    }
    Some(format!("data:{};base64,{}", mime_of(path), base64(&bytes)))
}

/// Imagem do clipboard salva como PNG na pasta do chat. Devolve o caminho.
/// cyrix: usa o `osascript` do sistema — egui só entrega texto no paste, e uma
/// crate de clipboard por causa disso não se paga.
#[cfg(target_os = "macos")]
pub fn clipboard_image_to(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let name = format!(
        "pasted_{}.png",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    let path = dir.join(&name);
    let script = format!(
        "set p to POSIX file \"{}\"\n\
         try\n\
             set d to (the clipboard as «class PNGf»)\n\
         on error\n\
             return \"no-image\"\n\
         end try\n\
         set fh to open for access p with write permission\n\
         set eof fh to 0\n\
         write d to fh\n\
         close access fh\n\
         return \"ok\"",
        path.display()
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    let said = String::from_utf8_lossy(&out.stdout);
    if said.trim() == "ok" && path.is_file() {
        Some(path)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_image_to(_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Precisa de uma imagem no clipboard; roda com
    /// `cargo test -- --ignored clipboard_real --nocapture`
    #[test]
    #[ignore]
    fn clipboard_real() {
        let dir = std::env::temp_dir().join("harness_clip");
        match clipboard_image_to(&dir) {
            Some(p) => {
                let n = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                println!("salvou {} ({n} bytes)", p.display());
                assert!(n > 0);
                assert!(is_image(&p));
            }
            None => println!("clipboard sem imagem — nada a fazer"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base64_bate_com_os_vetores_do_rfc() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_aguenta_byte_alto() {
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
        assert_eq!(base64(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn so_extensao_de_imagem_conta() {
        assert!(is_image(Path::new("a/b/shot.PNG")));
        assert!(is_image(Path::new("x.jpeg")));
        assert!(!is_image(Path::new("main.rs")));
        assert!(!is_image(Path::new("sem_extensao")));
    }

    #[test]
    fn mime_segue_a_extensao() {
        assert_eq!(mime_of(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(mime_of(Path::new("a.gif")), "image/gif");
        assert_eq!(mime_of(Path::new("a.png")), "image/png");
        // desconhecido cai em png, que é o formato do nosso paste
        assert_eq!(mime_of(Path::new("a.bmp")), "image/png");
    }

    #[test]
    fn data_url_respeita_o_teto_e_o_vazio() {
        let dir = std::env::temp_dir().join(format!("harness_att_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("x.png");
        std::fs::write(&img, b"foobar").unwrap();
        let u = data_url(&img, 1024).unwrap();
        assert!(u.starts_with("data:image/png;base64,Zm9vYmFy"));
        // acima do teto não vira anexo
        assert!(data_url(&img, 3).is_none());
        // arquivo vazio também não
        let empty = dir.join("e.png");
        std::fs::write(&empty, b"").unwrap();
        assert!(data_url(&empty, 1024).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
