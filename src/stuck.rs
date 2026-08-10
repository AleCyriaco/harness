//! Detector de laço: mesma tool, mesmos argumentos, repetida no mesmo turno.
//!
//! Existe porque `max_rounds` e o teto do Gauntlet param **tarde** — o modelo
//! pode gastar 20 rodadas relendo o mesmo arquivo antes de o teto salvar. Aqui
//! a repetição é barrada na hora: a chamada não roda e o resultado devolvido ao
//! modelo diz que ele está girando.
//!
//! Regra pura, sem I/O — quem conta as chamadas é o `agent.rs`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Segue o baile.
    Run,
    /// Repetição demais: não executa, devolve aviso.
    Block,
}

/// Quantas vezes esta chamada exata já rodou neste turno.
pub fn repeats(seen: &[(String, String)], name: &str, args: &str) -> usize {
    seen.iter().filter(|(n, a)| n == name && a == args).count()
}

/// `threshold` = quantas execuções idênticas são toleradas. Na de número
/// `threshold + 1` a chamada é barrada.
pub fn check(on: bool, seen: &[(String, String)], name: &str, args: &str, threshold: u32) -> Verdict {
    if !on || threshold == 0 {
        return Verdict::Run;
    }
    if repeats(seen, name, args) >= threshold as usize {
        Verdict::Block
    } else {
        Verdict::Run
    }
}

/// O que o modelo lê no lugar do resultado da tool.
pub fn message(name: &str, times: usize) -> String {
    format!(
        "error: loop detected — `{name}` was already called {times}x with identical arguments \
         in this turn, so it was not executed again. Change the approach (different arguments, \
         a different tool, or ask the user) or finish the task."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, a)| (n.to_string(), a.to_string()))
            .collect()
    }

    #[test]
    fn tres_iguais_bloqueia_na_quarta() {
        let seen = log(&[("read_file", "{\"path\":\"a\"}"); 3]);
        assert_eq!(
            check(true, &seen, "read_file", "{\"path\":\"a\"}", 3),
            Verdict::Block
        );
        // duas ainda passa
        let seen2 = log(&[("read_file", "{\"path\":\"a\"}"); 2]);
        assert_eq!(
            check(true, &seen2, "read_file", "{\"path\":\"a\"}", 3),
            Verdict::Run
        );
    }

    #[test]
    fn argumento_diferente_nao_e_laco() {
        let seen = log(&[("read_file", "{\"path\":\"a\"}"); 9]);
        assert_eq!(
            check(true, &seen, "read_file", "{\"path\":\"b\"}", 3),
            Verdict::Run
        );
    }

    #[test]
    fn desligado_nunca_bloqueia() {
        let seen = log(&[("run_command", "ls"); 50]);
        assert_eq!(check(false, &seen, "run_command", "ls", 3), Verdict::Run);
        // e threshold 0 também é "desligado", não "bloqueia tudo"
        assert_eq!(check(true, &seen, "run_command", "ls", 0), Verdict::Run);
    }

    #[test]
    fn a_mensagem_diz_o_numero_e_manda_mudar() {
        let m = message("graph_query", 3);
        assert!(m.contains("graph_query"));
        assert!(m.contains("3x"));
        assert!(m.starts_with("error:"), "o modelo trata como falha, não como saída válida");
    }
}
