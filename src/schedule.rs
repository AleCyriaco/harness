//! Scheduling: reenviar um prompt de tempos em tempos, no mesmo chat.
//!
//! cyrix: o agendamento vive no daemon, em memória. O daemon sobrevive à GUI,
//! que é o caso de uso ("deixa rodando"), mas não sobrevive a um reboot. Gravar
//! em disco entra quando alguém perder um agendamento de verdade.

use std::time::Duration;

/// `30m`, `2h`, `90s`, `1d` — ou só o número, que vale minutos.
pub fn parse_every(s: &str) -> Option<Duration> {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    let (num, unit) = match t.chars().last()? {
        c if c.is_ascii_digit() => (t.as_str(), 'm'),
        u => (&t[..t.len() - u.len_utf8()], u),
    };
    let n: u64 = num.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    let secs = match unit {
        's' => n,
        'm' => n * 60,
        'h' => n * 3_600,
        'd' => n * 86_400,
        _ => return None,
    };
    // teto de 30 dias e piso de 30 s: abaixo disso é loop, não agenda
    if !(30..=30 * 86_400).contains(&secs) {
        return None;
    }
    Some(Duration::from_secs(secs))
}

/// Um agendamento: a cada `every`, mandar `prompt`.
#[derive(Debug, Clone)]
pub struct Job {
    pub every: Duration,
    pub prompt: String,
    /// Segundos desde o último disparo, contados pelo daemon.
    pub waited: u64,
    pub runs: u32,
}

impl Job {
    pub fn new(every: Duration, prompt: String) -> Self {
        Self {
            every,
            prompt,
            waited: 0,
            runs: 0,
        }
    }
}

/// Avança o relógio de todos e devolve os que dispararam agora.
/// A sessão ocupada não dispara — o tempo continua contando, mas o prompt não
/// entra no meio de um turno.
pub fn tick(jobs: &mut [Job], elapsed_secs: u64, busy: bool) -> Vec<String> {
    let mut fired = Vec::new();
    for j in jobs.iter_mut() {
        j.waited += elapsed_secs;
        if j.waited >= j.every.as_secs() {
            if busy {
                continue;
            }
            j.waited = 0;
            j.runs += 1;
            fired.push(j.prompt.clone());
        }
    }
    fired
}

pub fn describe(jobs: &[Job]) -> String {
    if jobs.is_empty() {
        return "no scheduled prompts in this chat".into();
    }
    let mut out = String::from("scheduled in this chat:\n");
    for (i, j) in jobs.iter().enumerate() {
        out.push_str(&format!(
            "{}. every {}s · ran {}x · next in {}s — {}\n",
            i + 1,
            j.every.as_secs(),
            j.runs,
            j.every.as_secs().saturating_sub(j.waited),
            j.prompt.chars().take(60).collect::<String>()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unidades_e_limites() {
        assert_eq!(parse_every("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_every("30m"), Some(Duration::from_secs(1_800)));
        assert_eq!(parse_every("2h"), Some(Duration::from_secs(7_200)));
        assert_eq!(parse_every("1d"), Some(Duration::from_secs(86_400)));
        // número puro = minutos
        assert_eq!(parse_every("15"), Some(Duration::from_secs(900)));
        // fora dos limites ou sem sentido
        assert_eq!(parse_every("5s"), None, "abaixo de 30s é laço, não agenda");
        assert_eq!(parse_every("400d"), None);
        assert_eq!(parse_every("0m"), None);
        assert_eq!(parse_every("amanhã"), None);
        assert_eq!(parse_every(""), None);
    }

    #[test]
    fn dispara_no_intervalo_e_reinicia_a_contagem() {
        let mut jobs = vec![Job::new(Duration::from_secs(60), "check ci".into())];
        assert!(tick(&mut jobs, 30, false).is_empty(), "ainda não deu a hora");
        let fired = tick(&mut jobs, 30, false);
        assert_eq!(fired, vec!["check ci".to_string()]);
        assert_eq!(jobs[0].runs, 1);
        assert!(tick(&mut jobs, 30, false).is_empty(), "recomeçou a contar");
    }

    #[test]
    fn sessao_ocupada_nao_recebe_prompt_no_meio_do_turno() {
        let mut jobs = vec![Job::new(Duration::from_secs(60), "x".into())];
        assert!(tick(&mut jobs, 120, true).is_empty());
        // o tempo continuou correndo: assim que desocupa, dispara
        assert_eq!(tick(&mut jobs, 0, false), vec!["x".to_string()]);
    }
}
