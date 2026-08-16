//! Gauntlet Loop — o agente decompõe o objetivo, critica cada parte e recomeça
//! até aprovar.
//!
//! O laço é deliberadamente burro: injeta uma diretiva no system prompt e, no
//! fim de cada turno, reenvia "continue o loop" enquanto a resposta não trouxer
//! o marcador de conclusão. Sem subagente, sem fila, sem processo novo — quem
//! decompõe e critica é o próprio modelo, dentro do turno.

/// O modelo escreve isto quando considera o objetivo cumprido.
pub const DONE_MARKER: &str = "[GAUNTLET:DONE]";

/// Mensagem reenviada automaticamente a cada iteração.
pub const CONTINUE_MESSAGE: &str = "continue o loop";

pub const DEFAULT_MAX_ITERATIONS: u32 = 10;

/// Bloco acrescentado ao system prompt quando o toggle está ligado.
pub const DIRECTIVE: &str = "\
GAUNTLET LOOP ATIVO. Decomponha o objetivo em partes avaliáveis
separadamente. Para cada parte, gere o artefato e depois avalie-o
em um passo SEPARADO com contexto limpo, como crítico severo,
comparando contra a referência de qualidade definida pelo usuário.
Se falhar, liste os defeitos concretos e refaça. Só marque uma
parte como concluída quando o crítico aprovar.

Quando o objetivo inteiro estiver cumprido e aprovado pelo crítico,
termine a resposta com o marcador exato [GAUNTLET:DONE].";

/// Acrescenta a diretiva ao system prompt. Não faz nada quando desligado.
pub fn apply_to_system(content: &mut String, on: bool) {
    if on {
        content.push_str("\n\n");
        content.push_str(DIRECTIVE);
    }
}

/// A resposta declara o objetivo cumprido?
pub fn is_done(reply: &str) -> bool {
    reply.contains(DONE_MARKER)
}

/// Por que o laço parou — serve para dizer ao usuário em vez de sumir calado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// A resposta trouxe o marcador.
    Done,
    /// Bateu o teto de iterações.
    Exhausted,
    /// O turno girou em falso (mesma tool, mesmos args) — insistir é queimar
    /// as iterações restantes à toa.
    Stuck,
    /// A resposta repetiu a anterior: o modelo está reiniciando do zero a cada
    /// `continue o loop` em vez de avançar.
    Repeating,
}

/// Duas respostas dizem a mesma coisa? Compara palavras, ignorando caixa e
/// espaçamento — o modelo raramente repete byte a byte, mas repete o texto.
pub fn is_repeat(prev: &str, cur: &str) -> bool {
    let norm = |s: &str| {
        s.to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let (a, b) = (norm(prev), norm(cur));
    // texto curto demais não é evidência de laço
    if a.len() < 12 || b.len() < 12 {
        return false;
    }
    let head = a.len().min(b.len()).min(60);
    let same = a[..head].iter().zip(b[..head].iter()).filter(|(x, y)| x == y).count();
    same * 100 / head >= 85
}

/// Decide o que fazer no fim de um turno.
///
/// `on` já leva em conta o toggle no instante da decisão, então desligá-lo
/// interrompe o laço sem precisar de cancelamento em outro lugar.
pub fn next_step(
    on: bool,
    reply: &str,
    prev_reply: &str,
    stuck: bool,
    iterations: u32,
    max: u32,
) -> Option<Stop> {
    if !on {
        return None;
    }
    if is_done(reply) {
        return Some(Stop::Done);
    }
    if stuck {
        return Some(Stop::Stuck);
    }
    // repetir a resposta é girar em falso com outras palavras
    if iterations > 0 && is_repeat(prev_reply, reply) {
        return Some(Stop::Repeating);
    }
    if iterations >= max {
        return Some(Stop::Exhausted);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desligado_nunca_continua_nem_para() {
        assert_eq!(next_step(false, "qualquer coisa", "", false, 0, 10), None);
        // mesmo com o marcador: desligado é desligado
        assert_eq!(next_step(false, DONE_MARKER, "", false, 0, 10), None);
    }

    #[test]
    fn continua_ate_o_marcador() {
        // sem marcador e com folga: `None` = continuar
        assert_eq!(next_step(true, "faltou a parte 2", "", false, 3, 10), None);
        assert_eq!(
            next_step(true, "tudo certo [GAUNTLET:DONE]", "", false, 3, 10),
            Some(Stop::Done)
        );
    }

    #[test]
    fn teto_de_iteracoes_para_o_laco() {
        assert_eq!(next_step(true, "ainda não", "", false, 9, 10), None);
        assert_eq!(next_step(true, "ainda não", "", false, 10, 10), Some(Stop::Exhausted));
        // marcador vence o teto: terminou é terminou
        assert_eq!(next_step(true, DONE_MARKER, "", false, 99, 10), Some(Stop::Done));
    }

    #[test]
    fn turno_travado_para_o_laco_antes_do_teto() {
        // girar em falso não melhora com "continue o loop"
        assert_eq!(
            next_step(true, "ainda não", "", true, 1, 10),
            Some(Stop::Stuck)
        );
        // mas terminar é terminar, mesmo tendo travado no meio
        assert_eq!(
            next_step(true, DONE_MARKER, "", true, 1, 10),
            Some(Stop::Done)
        );
    }

    #[test]
    fn resposta_repetida_para_o_laco() {
        let a = "Vou retomar o estado do workspace e do que ja existe da GUI do Mole. \
                 Ja existe o clone e um esqueleto em gui/mac, vou mapear o que falta.";
        // mesma resposta na iteração seguinte = reiniciou do zero
        assert_eq!(next_step(true, a, a, false, 1, 10), Some(Stop::Repeating));
        // na primeira iteração não há anterior para comparar
        assert_eq!(next_step(true, a, a, false, 0, 10), None);
        // resposta diferente segue o laço
        let b = "Agora implementei o picker nativo e as secoes Purge e Installer, \
                 faltando apenas o streaming da saida do uninstall.";
        assert_eq!(next_step(true, b, a, false, 1, 10), None);
    }

    /// Caso real: o modelo repetiu a fala e só acrescentou uma frase no fim,
    /// chamando tools diferentes a cada rodada. Tem que contar como repetição.
    #[test]
    fn continuacao_quase_igual_conta_como_repeticao() {
        let a = "Orientar workspace e estado do loop. Loop: #8 #9 #10 em aberto. \
                 Ler artefactos e skill. Ler resto do agente, runbooks, tickets, \
                 screens e UI. Critica isolada. Ler o resto da UI e comparar copias.";
        let b = "Orientar workspace e estado do loop. Loop: #8 #9 #10 em aberto. \
                 Ler artefactos e skill. Ler resto do agente, runbooks, tickets, \
                 screens e UI. Critica isolada. Ler o resto da UI e comparar copias. \
                 Critica isolada: falhas reais em doc, agente e UI.";
        assert!(is_repeat(a, b), "mesma fala com uma frase a mais ainda é laço");
    }

    #[test]
    fn texto_curto_nao_conta_como_repeticao() {
        assert!(!is_repeat("ok", "ok"), "resposta curta não é evidência");
    }

    #[test]
    fn diretiva_so_entra_ligada_e_ensina_o_marcador() {
        let mut off = "base".to_string();
        apply_to_system(&mut off, false);
        assert_eq!(off, "base");

        let mut on = "base".to_string();
        apply_to_system(&mut on, true);
        assert!(on.starts_with("base"));
        assert!(on.contains("GAUNTLET LOOP ATIVO"));
        // sem isto o modelo nunca sinaliza o fim e o laço só para no teto
        assert!(on.contains(DONE_MARKER));
    }
}
