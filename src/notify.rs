//! Aviso de sistema quando um turno longo termina.
//!
//! cyrix: usa o notificador nativo por `Command`, sem crate nova. Falha em
//! silêncio de propósito — notificação que não aparece não é motivo para
//! atrapalhar o turno que acabou de dar certo.

/// Deve avisar? Turno curto não merece notificação.
pub fn should_notify(after_secs: u64, elapsed_secs: u64) -> bool {
    after_secs > 0 && elapsed_secs >= after_secs
}

/// Escapa aspas e barras — o texto vai dentro de um literal do osascript.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "'")
        .chars()
        .take(180)
        .collect()
}

pub fn send(title: &str, body: &str) {
    let (title, body) = (escape(title), escape(body));
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification \"{body}\" with title \"{title}\"");
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("notify-send")
            .args([&title, &body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let ps = format!(
            "[reflection.assembly]::loadwithpartialname('System.Windows.Forms');\
             $n=New-Object System.Windows.Forms.NotifyIcon;\
             $n.Icon=[System.Drawing.SystemIcons]::Information;$n.Visible=$true;\
             $n.ShowBalloonTip(5000,'{title}','{body}',0)"
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn so_avisa_turno_longo_e_nunca_quando_desligado() {
        assert!(!should_notify(0, 9_999), "0 = desligado");
        assert!(!should_notify(30, 12));
        assert!(should_notify(30, 30));
        assert!(should_notify(30, 120));
    }

    #[test]
    fn texto_com_aspas_nao_quebra_o_script() {
        let e = escape("disse \"pronto\" e saiu\\");
        assert!(!e.contains('"'));
        assert!(!e.contains("\\\""));
    }
}
