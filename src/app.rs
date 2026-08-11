use eframe::egui;
use std::path::PathBuf;
use std::thread;

use crate::agent::{AgentEvent, ApprovalDecision};
use crate::browser::{self, BrowserState};
use crate::tokenless::TokenLessLevel;
use crate::config::{self, Config};
use crate::daemon_client::{DaemonGuiClient, Incoming};
use crate::diagnostics::{self, DiagnosticsSnapshot};
use crate::llm::ChatMessage;
use crate::md;
use crate::memory;
use crate::modes::AppMode;
use crate::plan;
use crate::preview::{self, PreviewContent};
use crate::protocol::SessionSummary;
use crate::session::{self, Session, SessionMeta, UiLogLine};
use crate::side_panel::{self, PanelKind};
use crate::slash::{self, SlashAction};
use crate::swarm;
use crate::theme::{micro, pal};
use crate::toolcall::{one_line, ToolCall, ToolState};
use crate::ui as w;
use crate::update::{self, UpdateStatus};
use crate::webserver;

#[derive(Clone)]
struct UiMessage {
    role: String,
    text: String,
}

enum EitherAttach {
    Payload(serde_json::Value),
    Created {
        sid: String,
        chat_dir: String,
        title: String,
    },
}

/// One open GUI tab (multi-session). Inactive tabs keep full UI state.
#[derive(Clone)]
struct SessionTab {
    session: Session,
    messages: Vec<UiMessage>,
    llm_history: Vec<ChatMessage>,
    input: String,
    busy: bool,
    stream_buf: String,
    pending_approval: Option<(String, String)>,
    artifacts: Vec<PathBuf>,
    mode: AppMode,
    status: String,
}

impl SessionTab {
    fn from_parts(
        session: Session,
        messages: Vec<UiMessage>,
        mode: AppMode,
        status: String,
    ) -> Self {
        Self {
            session,
            messages,
            llm_history: Vec::new(),
            input: String::new(),
            busy: false,
            stream_buf: String::new(),
            pending_approval: None,
            artifacts: Vec::new(),
            mode,
            status,
        }
    }

    fn approx_bytes(&self) -> usize {
        crate::mem_stats::estimate_session_bytes(
            &self.session,
            &self.llm_history,
            self.messages
                .iter()
                .map(|m| (m.role.len(), m.text.len())),
        ) + self.stream_buf.len()
            + self.input.len()
    }

    fn tab_title(&self) -> String {
        let folder = if self.session.meta.chat_folder_name.is_empty() {
            crate::protocol::short_id(&self.session.meta.id)
        } else {
            self.session.meta.chat_folder_name.clone()
        };
        let title: String = self.session.meta.title.chars().take(18).collect();
        if title.is_empty() || title == "New session" {
            folder
        } else {
            format!("{folder} · {title}")
        }
    }

    fn matches_session(&self, id: &str) -> bool {
        self.session.meta.id == id
            || self.session.meta.daemon_session_id == id
            || (!self.session.meta.daemon_session_id.is_empty()
                && self.session.meta.daemon_session_id == id)
    }
}

/// Destinos do rail (60px). Substitui as 8 abas do painel direito.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    Chat,
    Files,
    /// Grafo estrutural do workspace + economia medida (grafo e token_less).
    Graph,
    Memory,
    Swarm,
    Diag,
    /// Web + Server fundidos (pé do rail).
    WebServer,
}

impl Dest {
    fn rail(self) -> (crate::ui::Glyph, &'static str) {
        use crate::ui::Glyph;
        match self {
            Dest::Chat => (Glyph::Square, "CHAT"),
            Dest::Files => (Glyph::Square, "FILES"),
            Dest::Graph => (Glyph::Nodes, "GRAPH"),
            Dest::Memory => (Glyph::Circle, "MEM"),
            Dest::Swarm => (Glyph::Diamond, "SWARM"),
            Dest::Diag => (Glyph::Bar, "DIAG"),
            Dest::WebServer => (Glyph::Globe, "WEB"),
        }
    }
}

/// Painel contextual que abre *dentro* do chat (era aba Preview / Side).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CtxTab {
    Preview,
    Side,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsSection {
    Models,
    Workspace,
    Approvals,
    Memory,
    Swarm,
    /// Extração de página, crawl e detector de laço.
    Web,
    Appearance,
    Updates,
}

impl SettingsSection {
    const ALL: [SettingsSection; 8] = [
        SettingsSection::Models,
        SettingsSection::Workspace,
        SettingsSection::Approvals,
        SettingsSection::Memory,
        SettingsSection::Swarm,
        SettingsSection::Web,
        SettingsSection::Appearance,
        SettingsSection::Updates,
    ];

    fn label(self) -> &'static str {
        match self {
            SettingsSection::Models => "Modelos e pool",
            SettingsSection::Workspace => "Workspace",
            SettingsSection::Approvals => "Approvals",
            SettingsSection::Memory => "Memory",
            SettingsSection::Swarm => "Swarm",
            SettingsSection::Web => "Web & loops",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Updates => "Updates",
        }
    }
}

/// Janela deslizante de tokens/s para o gráfico correndo.
///
/// A saída vem dos `StreamDelta` (chega token a token, então a linha anda de
/// verdade); a entrada só é conhecida quando a chamada fecha, então entra como
/// pico no instante em que o contador do daemon sobe.
struct TokenMeter {
    input: std::collections::VecDeque<f32>,
    output: std::collections::VecDeque<f32>,
    acc_in: f32,
    acc_out: f32,
    last_tick: std::time::Instant,
    seen_prompt: u64,
    seen_completion: u64,
}

/// ~24 s de janela a 200 ms por amostra.
const METER_SLOTS: usize = 120;
const METER_TICK_MS: u128 = 200;

impl TokenMeter {
    fn new() -> Self {
        Self {
            input: std::collections::VecDeque::new(),
            output: std::collections::VecDeque::new(),
            acc_in: 0.0,
            acc_out: 0.0,
            last_tick: std::time::Instant::now(),
            seen_prompt: 0,
            seen_completion: 0,
        }
    }

    /// Streaming: ~4 chars por token, o suficiente para a forma da curva.
    fn stream_chars(&mut self, chars: usize) {
        self.acc_out += chars as f32 / 4.0;
    }

    /// Contadores do daemon: a diferença desde a última leitura é o que entrou.
    fn absorb_totals(&mut self, prompt: u64, completion: u64) {
        if prompt < self.seen_prompt || completion < self.seen_completion {
            // daemon reiniciou: recomeça sem inventar pico
            self.seen_prompt = prompt;
            self.seen_completion = completion;
            return;
        }
        self.acc_in += (prompt - self.seen_prompt) as f32;
        self.seen_prompt = prompt;
        // saída já contabilizada pelo streaming; só completa o que faltou
        let d = completion - self.seen_completion;
        self.seen_completion = completion;
        if self.acc_out < d as f32 {
            self.acc_out = d as f32;
        }
    }

    fn tick(&mut self) {
        let ms = self.last_tick.elapsed().as_millis();
        if ms < METER_TICK_MS {
            return;
        }
        let secs = (ms as f32 / 1000.0).max(0.001);
        self.push(self.acc_in / secs, self.acc_out / secs);
        self.acc_in = 0.0;
        self.acc_out = 0.0;
        self.last_tick = std::time::Instant::now();
    }

    fn push(&mut self, i: f32, o: f32) {
        self.input.push_back(i);
        self.output.push_back(o);
        while self.input.len() > METER_SLOTS {
            self.input.pop_front();
        }
        while self.output.len() > METER_SLOTS {
            self.output.pop_front();
        }
    }

    fn last(&self) -> (f32, f32) {
        (
            self.input.back().copied().unwrap_or(0.0),
            self.output.back().copied().unwrap_or(0.0),
        )
    }

    fn active(&self) -> bool {
        self.input.iter().rev().take(5).any(|v| *v > 0.0)
            || self.output.iter().rev().take(5).any(|v| *v > 0.0)
    }

    fn series(&self) -> (Vec<f32>, Vec<f32>) {
        (
            self.input.iter().copied().collect(),
            self.output.iter().copied().collect(),
        )
    }
}

/// Linha da lista de chats, já normalizada entre sessão viva e salva.
struct Row {
    id: String,
    title: String,
    subtitle: String,
    hover: String,
    updated_at: String,
    busy: bool,
    pinned: bool,
    live: bool,
}

enum RowAction {
    Open(bool),
    StartRename(String),
    CancelRename,
    Pin(bool),
    /// A chave carrega o caminho da pasta, não o id.
    OpenFolder,
    Kill,
    AskDelete,
}

/// Uma entrada da paleta ⌘K.
struct CmdEntry {
    label: String,
    hint: String,
    action: CmdAction,
}

enum CmdAction {
    Slash(String),
    Go(Dest),
    OpenSettings,
    NewChat,
    ToggleTheme,
    LiveSession(String),
    SavedSession(String),
    OpenFile(PathBuf),
    UseLlm(String),
    CloseTab,
    TokenLess(TokenLessLevel),
    GraphBuild,
    PinCurrent,
    RenameCurrent,
    DeleteCurrent,
    ToggleUsage,
    PinUsage,
    Project,
    ResetAllChats,
}

pub struct HarnessApp {
    cfg: Config,
    mode: AppMode,
    session: Session,
    session_list: Vec<SessionMeta>,
    /// Open multi-session tabs (includes active; packed on switch).
    open_tabs: Vec<SessionTab>,
    active_tab: usize,
    /// Live sessions on the multi-client daemon (multi-session scale).
    daemon_live: Vec<SessionSummary>,
    daemon_info_line: String,
    mem_line: String,
    gui_rss_kb: Option<u64>,
    daemon_rss_kb: Option<u64>,
    daemon_sessions_bytes: u64,
    last_mem_refresh: std::time::Instant,
    input: String,
    messages: Vec<UiMessage>,
    llm_history: Vec<ChatMessage>,
    busy: bool,
    status: String,
    show_settings: bool,
    show_setup: bool,
    artifacts: Vec<PathBuf>,
    /// Connected multi-client daemon (agent runs there).
    daemon: Option<DaemonGuiClient>,
    pending_approval: Option<(String, String)>,
    draft_api_base: String,
    draft_api_key: String,
    draft_model: String,
    draft_workspace: String,
    draft_auto_shell: bool,
    draft_stream: bool,
    draft_update_repo: String,
    stream_buf: String,
    /// Destino ativo do rail.
    dest: Dest,
    /// Painel contextual do chat (Preview / Side).
    ctx_panel: bool,
    ctx_tab: CtxTab,
    /// ⌘K
    cmdk: bool,
    cmdk_query: String,
    cmdk_sel: usize,
    settings_section: SettingsSection,
    preview: Option<PreviewContent>,
    diagnostics: DiagnosticsSnapshot,
    update: UpdateStatus,
    update_busy: bool,
    browser_url: String,
    browser: BrowserState,
    memory_query: String,
    memory_input: String,
    memory_view: String,
    server_path: String,
    server_port: String,
    /// Estado do swarm vindo do daemon (é lá que os workers rodam).
    swarm_snap: crate::swarm::SwarmSnapshot,
    /// Auto-continues já gastos no objetivo atual do Gauntlet Loop.
    gauntlet_iter: u32,
    /// Página já anunciada no painel — evita reabrir a cada turno.
    announced_page: Option<PathBuf>,
    metrics: crate::metrics::Metrics,
    graph_stats: crate::graph::GraphStats,
    graph_query: String,
    graph_answer: String,
    /// Chat sendo renomeado na lista (id) + buffer do campo.
    rename_id: Option<String>,
    rename_buf: String,
    /// Confirmação de apagar: (id, título, pasta do chat).
    delete_target: Option<(String, String, String)>,
    /// Confirmação do reset (apagar TODOS os chats).
    confirm_reset: bool,
    /// Painel de uso: visível agora e fixado (persistido em Config).
    show_usage: bool,
    meter: TokenMeter,
    started_at: std::time::Instant,
    last_swarm_refresh: std::time::Instant,
}

impl HarnessApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = Config::load();
        let cfg_usage_pinned = cfg.usage_pinned;
        crate::theme::set_mode(&cc.egui_ctx, cfg.theme);
        let mode = cfg.mode;
        // Placeholder session until daemon create (or setup)
        let session = Session::new(mode, &cfg.workspace);
        let mut messages: Vec<UiMessage> = session
            .ui_log
            .iter()
            .map(|l| UiMessage {
                role: l.role.clone(),
                text: l.text.clone(),
            })
            .collect();
        if cfg.workspace_ready {
            if let Some(first) = messages.first_mut() {
                if first.role == "system" {
                    first.text = crate::app_text::welcome_with_workspace(mode, &cfg.workspace);
                }
            }
        }
        let session_list = session::list_sessions().unwrap_or_default();
        let show_setup = cfg.needs_setup();
        let web_port = cfg.web_server_port;
        let draft_ws = if cfg.workspace_ready {
            cfg.workspace.display().to_string()
        } else {
            config::suggested_workspace().display().to_string()
        };

        let status0: String = if show_setup {
            "setup required".into()
        } else {
            "connecting daemon…".into()
        };
        let mut app = Self {
            draft_api_base: cfg.api_base.clone(),
            draft_api_key: cfg.api_key.clone(),
            draft_model: cfg.model.clone(),
            draft_workspace: draft_ws,
            draft_auto_shell: cfg.auto_approve_shell,
            draft_stream: cfg.stream,
            draft_update_repo: cfg.update_repo.clone(),
            artifacts: scan_artifacts(&cfg.workspace, mode),
            cfg,
            mode,
            session: session.clone(),
            session_list,
            open_tabs: vec![SessionTab::from_parts(
                session,
                messages.clone(),
                mode,
                status0.clone(),
            )],
            active_tab: 0,
            daemon_live: Vec::new(),
            daemon_info_line: String::new(),
            mem_line: String::new(),
            gui_rss_kb: None,
            daemon_rss_kb: None,
            daemon_sessions_bytes: 0,
            last_mem_refresh: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(std::time::Instant::now),
            input: String::new(),
            messages,
            llm_history: Vec::new(),
            busy: false,
            status: status0,
            show_settings: false,
            show_setup,
            daemon: None,
            pending_approval: None,
            stream_buf: String::new(),
            dest: Dest::Chat,
            ctx_panel: false,
            ctx_tab: CtxTab::Preview,
            cmdk: false,
            cmdk_query: String::new(),
            cmdk_sel: 0,
            settings_section: SettingsSection::Models,
            preview: None,
            diagnostics: DiagnosticsSnapshot::default(),
            update: UpdateStatus {
                current: update::CURRENT_VERSION.into(),
                message: "not checked".into(),
                ..Default::default()
            },
            update_busy: false,
            browser_url: format!("http://127.0.0.1:{web_port}/"),
            browser: browser::get(),
            memory_query: String::new(),
            memory_input: String::new(),
            memory_view: String::new(),
            server_path: ".".into(),
            server_port: web_port.to_string(),
            swarm_snap: crate::swarm::SwarmSnapshot::default(),
            gauntlet_iter: 0,
            announced_page: None,
            metrics: crate::metrics::Metrics::default(),
            graph_stats: crate::graph::GraphStats::default(),
            graph_query: String::new(),
            graph_answer: String::new(),
            rename_id: None,
            rename_buf: String::new(),
            delete_target: None,
            confirm_reset: false,
            show_usage: cfg_usage_pinned,
            meter: TokenMeter::new(),
            started_at: std::time::Instant::now(),
            last_swarm_refresh: std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(std::time::Instant::now),
        };

        if app.cfg.check_updates_on_start && !app.cfg.update_repo.is_empty() {
            app.spawn_update_check();
        }
        app.refresh_memory_list();
        // GUI ↔ daemon: connect + create session when ready
        if app.cfg.workspace_ready && !app.show_setup {
            app.connect_daemon_and_session(None);
        }
        app
    }

    /// Start/connect daemon and open a session (new or reuse chat_dir).
    fn connect_daemon_and_session(&mut self, reuse_chat_dir: Option<String>) {
        match DaemonGuiClient::connect() {
            Ok(client) => {
                match client.create_session(self.mode, None, reuse_chat_dir) {
                    Ok((id, chat_dir, title)) => {
                        self.session = Session::from_daemon(
                            id,
                            chat_dir,
                            title,
                            self.mode,
                            &self.cfg.workspace,
                        );
                        self.messages = self
                            .session
                            .ui_log
                            .iter()
                            .map(|l| UiMessage {
                                role: l.role.clone(),
                                text: l.text.clone(),
                            })
                            .collect();
                        self.llm_history.clear();
                        self.artifacts = scan_artifacts(&self.session.chat_path(), self.mode);
                        let _ = session::save_session(&self.session);
                        self.session_list = session::list_sessions().unwrap_or_default();
                        self.status = format!(
                            "daemon · 📁 {}",
                            self.session.meta.chat_folder_name
                        );
                        // Seed first tab from this session
                        self.open_tabs = vec![SessionTab {
                            session: self.session.clone(),
                            messages: self.messages.clone(),
                            llm_history: self.llm_history.clone(),
                            input: String::new(),
                            busy: false,
                            stream_buf: String::new(),
                            pending_approval: None,
                            artifacts: self.artifacts.clone(),
                            mode: self.mode,
                            status: self.status.clone(),
                        }];
                        self.active_tab = 0;
                        self.daemon = Some(client);
                        self.refresh_daemon_sessions();
                    }
                    Err(e) => {
                        self.daemon = Some(client);
                        self.push_error(format!("daemon session: {e}"));
                        self.status = "daemon connected (no session)".into();
                        self.refresh_daemon_sessions();
                    }
                }
            }
            Err(e) => {
                self.push_error(format!(
                    "daemon offline: {e} — agent runs only when daemon is up"
                ));
                self.status = "daemon offline".into();
            }
        }
    }

    fn refresh_daemon_sessions(&mut self) {
        let Some(client) = &self.daemon else {
            self.daemon_live.clear();
            self.daemon_info_line.clear();
            return;
        };
        match client.list_sessions() {
            Ok(list) => self.daemon_live = list,
            Err(e) => {
                self.daemon_info_line = format!("list failed: {e}");
            }
        }
        if let Ok((live, max, clients, sock, rss_kb, sessions_bytes, _pid)) = client.daemon_info() {
            self.daemon_rss_kb = if rss_kb > 0 { Some(rss_kb) } else { None };
            self.daemon_sessions_bytes = sessions_bytes;
            self.daemon_info_line =
                format!("{live}/{max} live · {clients} clients · {sock}");
            if !self.busy {
                self.status = format!(
                    "daemon · {}/{} · 📁 {}",
                    live,
                    max,
                    self.session.meta.chat_folder_name
                );
            }
        }
        self.refresh_mem_stats(true);
    }

    fn refresh_mem_stats(&mut self, force: bool) {
        if !force && self.last_mem_refresh.elapsed() < std::time::Duration::from_secs(2) {
            return;
        }
        self.last_mem_refresh = std::time::Instant::now();
        self.gui_rss_kb = crate::mem_stats::self_rss_kb();
        // Include active tab estimate + packed tabs
        let mut tabs_bytes = 0usize;
        for (i, t) in self.open_tabs.iter().enumerate() {
            if i == self.active_tab {
                tabs_bytes += crate::mem_stats::estimate_session_bytes(
                    &self.session,
                    &self.llm_history,
                    self.messages.iter().map(|m| (m.role.len(), m.text.len())),
                ) + self.stream_buf.len()
                    + self.input.len();
            } else {
                tabs_bytes += t.approx_bytes();
            }
        }
        let live = self.daemon_live.len();
        let max = self.cfg.max_sessions;
        self.mem_line = crate::mem_stats::summary_line(
            self.gui_rss_kb,
            self.daemon_rss_kb,
            tabs_bytes,
            live,
            max,
        );
        if self.daemon_sessions_bytes > 0 {
            self.mem_line.push_str(&format!(
                " · daemon-sessions ~{}",
                crate::mem_stats::format_bytes(self.daemon_sessions_bytes as usize)
            ));
        }
    }

    fn pack_active_tab(&mut self) {
        if self.open_tabs.is_empty() {
            self.open_tabs.push(SessionTab::from_parts(
                self.session.clone(),
                self.messages.clone(),
                self.mode,
                self.status.clone(),
            ));
            self.active_tab = 0;
        }
        let i = self.active_tab.min(self.open_tabs.len().saturating_sub(1));
        self.active_tab = i;
        if let Some(tab) = self.open_tabs.get_mut(i) {
            tab.session = self.session.clone();
            tab.messages = self.messages.clone();
            tab.llm_history = self.llm_history.clone();
            tab.input = self.input.clone();
            tab.busy = self.busy;
            tab.stream_buf = self.stream_buf.clone();
            tab.pending_approval = self.pending_approval.clone();
            tab.artifacts = self.artifacts.clone();
            tab.mode = self.mode;
            tab.status = self.status.clone();
        }
    }

    fn unpack_active_tab(&mut self) {
        if self.open_tabs.is_empty() {
            return;
        }
        let i = self.active_tab.min(self.open_tabs.len() - 1);
        self.active_tab = i;
        let tab = self.open_tabs[i].clone();
        self.session = tab.session;
        self.messages = tab.messages;
        self.llm_history = tab.llm_history;
        self.input = tab.input;
        self.busy = tab.busy;
        self.stream_buf = tab.stream_buf;
        self.pending_approval = tab.pending_approval;
        self.artifacts = tab.artifacts;
        self.mode = tab.mode;
        self.cfg.mode = tab.mode;
        self.status = tab.status;
    }

    fn switch_tab(&mut self, idx: usize) {
        if idx >= self.open_tabs.len() || idx == self.active_tab {
            return;
        }
        self.pack_active_tab();
        self.active_tab = idx;
        self.unpack_active_tab();
        self.refresh_mem_stats(true);
    }

    fn close_tab(&mut self, idx: usize) {
        if self.open_tabs.len() <= 1 {
            return;
        }
        if idx >= self.open_tabs.len() {
            return;
        }
        // Don't close while that tab is busy (active or packed)
        let busy = if idx == self.active_tab {
            self.busy
        } else {
            self.open_tabs.get(idx).map(|t| t.busy).unwrap_or(false)
        };
        if busy {
            self.push_error("stop the agent on that tab before closing".into());
            return;
        }
        self.pack_active_tab();
        let sid = self.open_tabs[idx].session.meta.daemon_session_id.clone();
        if let Some(client) = &self.daemon {
            if !sid.is_empty() {
                let _ = client.detach(&sid);
            }
        }
        self.open_tabs.remove(idx);
        if self.active_tab >= self.open_tabs.len() {
            self.active_tab = self.open_tabs.len() - 1;
        } else if idx < self.active_tab {
            self.active_tab -= 1;
        }
        self.unpack_active_tab();
        self.refresh_mem_stats(true);
    }

    /// Open session in a new tab (or focus existing tab with same id).
    fn open_session_in_tab(&mut self, session: Session, messages: Vec<UiMessage>, history: Vec<ChatMessage>) {
        let id = if !session.meta.daemon_session_id.is_empty() {
            session.meta.daemon_session_id.clone()
        } else {
            session.meta.id.clone()
        };
        if let Some(i) = self
            .open_tabs
            .iter()
            .position(|t| t.matches_session(&id) || t.session.meta.id == session.meta.id)
        {
            // also check active flat state
            if self.session.meta.id == session.meta.id
                || self.session.meta.daemon_session_id == id
            {
                // already active
                return;
            }
            self.switch_tab(i);
            return;
        }
        if self.session.meta.id == session.meta.id
            || (!id.is_empty() && self.session.meta.daemon_session_id == id)
        {
            return;
        }
        self.pack_active_tab();
        let mode = session.meta.mode;
        let artifacts = scan_artifacts(&session.chat_path(), mode);
        let status = format!("tab · 📁 {}", session.meta.chat_folder_name);
        self.open_tabs.push(SessionTab {
            session,
            messages,
            llm_history: history,
            input: String::new(),
            busy: false,
            stream_buf: String::new(),
            pending_approval: None,
            artifacts,
            mode,
            status,
        });
        self.active_tab = self.open_tabs.len() - 1;
        self.unpack_active_tab();
        self.refresh_mem_stats(true);
    }

    fn sync_active_into_tabs(&mut self) {
        // Keep open_tabs[active] roughly in sync without full clone every frame —
        // only used before listing tabs for labels/busy/ram.
        if let Some(tab) = self.open_tabs.get_mut(self.active_tab) {
            tab.busy = self.busy;
            tab.session.meta.title = self.session.meta.title.clone();
            tab.session.meta.chat_folder_name = self.session.meta.chat_folder_name.clone();
            tab.mode = self.mode;
        }
    }

    fn active_tab_approx_bytes(&self) -> usize {
        crate::mem_stats::estimate_session_bytes(
            &self.session,
            &self.llm_history,
            self.messages.iter().map(|m| (m.role.len(), m.text.len())),
        ) + self.stream_buf.len()
            + self.input.len()
    }

    fn kill_daemon_session(&mut self, id: &str, delete_disk: bool) {
        if id == self.session.meta.id || id == self.session.meta.daemon_session_id {
            if self.busy {
                self.push_error("stop the agent before killing the active session".into());
                return;
            }
        }
        let result = self
            .daemon
            .as_ref()
            .map(|c| c.kill_session(id, delete_disk))
            .unwrap_or_else(|| Err(anyhow::anyhow!("daemon offline")));
        match result {
            Ok(()) => {
                let was_active =
                    id == self.session.meta.daemon_session_id || id == self.session.meta.id;
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: format!(
                        "session {} killed{}",
                        crate::protocol::short_id(id),
                        if delete_disk {
                            " (+ disk)"
                        } else {
                            " (disk kept)"
                        }
                    ),
                });
                if was_active {
                    self.new_session();
                }
                self.refresh_daemon_sessions();
                self.session_list = session::list_sessions().unwrap_or_default();
            }
            Err(e) => self.push_error(format!("kill session: {e}")),
        }
    }

    fn switch_to_daemon_session(&mut self, id: &str) {
        if id == self.session.meta.daemon_session_id || id == self.session.meta.id {
            return;
        }
        // Already open as a tab?
        if let Some(i) = self.open_tabs.iter().position(|t| t.matches_session(id)) {
            self.switch_tab(i);
            return;
        }
        self.persist();
        self.pack_active_tab();
        // Keep subscription on previous tab so background turns continue
        let mode = self.mode;
        let attach_or_create = if let Some(client) = &self.daemon {
            match client.attach(id) {
                Ok(payload) => Ok(EitherAttach::Payload(payload)),
                Err(_) => client
                    .create_session_ex(mode, None, None, Some(id.to_string()))
                    .map(|(sid, chat_dir, title)| EitherAttach::Created {
                        sid,
                        chat_dir,
                        title,
                    })
                    .map_err(|e| e.to_string()),
            }
        } else {
            Err("daemon offline".into())
        };
        match attach_or_create {
            Ok(EitherAttach::Payload(payload)) => {
                // Prefer opening as tab without destroying current chat
                let chat_dir = payload
                    .get("chat_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = payload
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("session")
                    .to_string();
                if let Ok(mut disk) = session::load_session(id) {
                    disk.meta.daemon_session_id = id.to_string();
                    let msgs: Vec<UiMessage> = disk
                        .ui_log
                        .iter()
                        .map(|l| UiMessage {
                            role: l.role.clone(),
                            text: l.text.clone(),
                        })
                        .collect();
                    let hist = disk.messages.clone();
                    self.open_session_in_tab(disk, msgs, hist);
                    self.merge_attach_history(&payload);
                } else {
                    let sess =
                        Session::from_daemon(id.to_string(), chat_dir, title, mode, &self.cfg.workspace);
                    let msgs: Vec<UiMessage> = sess
                        .ui_log
                        .iter()
                        .map(|l| UiMessage {
                            role: l.role.clone(),
                            text: l.text.clone(),
                        })
                        .collect();
                    self.open_session_in_tab(sess, msgs, Vec::new());
                    self.merge_attach_history(&payload);
                }
            }
            Ok(EitherAttach::Created {
                sid,
                chat_dir,
                title,
            }) => {
                if let Ok(mut disk) = session::load_session(id) {
                    disk.meta.daemon_session_id = sid;
                    let msgs: Vec<UiMessage> = disk
                        .ui_log
                        .iter()
                        .map(|l| UiMessage {
                            role: l.role.clone(),
                            text: l.text.clone(),
                        })
                        .collect();
                    let hist = disk.messages.clone();
                    self.open_session_in_tab(disk, msgs, hist);
                } else {
                    let sess =
                        Session::from_daemon(sid, chat_dir, title, mode, &self.cfg.workspace);
                    let msgs: Vec<UiMessage> = sess
                        .ui_log
                        .iter()
                        .map(|l| UiMessage {
                            role: l.role.clone(),
                            text: l.text.clone(),
                        })
                        .collect();
                    self.open_session_in_tab(sess, msgs, Vec::new());
                }
            }
            Err(e) => self.push_error(format!("switch session: {e}")),
        }
        self.refresh_daemon_sessions();
    }


    /// Puxa o estado do swarm do daemon. Rápido quando o painel está aberto,
    /// lento no resto (só para o ponto do rail e a status bar).
    fn refresh_swarm(&mut self, force: bool) {
        let period = if matches!(self.dest, Dest::Swarm | Dest::Graph) {
            600
        } else {
            3000
        };
        if !force
            && self.last_swarm_refresh.elapsed() < std::time::Duration::from_millis(period)
        {
            return;
        }
        self.last_swarm_refresh = std::time::Instant::now();
        let Some(client) = &self.daemon else {
            self.swarm_snap = crate::swarm::SwarmSnapshot::default();
            return;
        };
        if let Ok((swarm, metrics)) = client.runtime_info() {
            self.meter
                .absorb_totals(metrics.prompt_tokens, metrics.completion_tokens);
            self.swarm_snap = swarm;
            self.metrics = metrics;
        }
        // O grafo é por sessão (projeto apontado), então quem lê é a GUI.
        self.graph_stats = crate::graph::stats(&self.project_root(), false).unwrap_or_default();
    }

    /// Abre a confirmação de apagar (nunca apaga direto — é irreversível).
    fn ask_delete(&mut self, id: &str) {
        let (title, dir) = if let Some(s) = self.daemon_live.iter().find(|s| s.id == id) {
            (summary_title(s), s.chat_dir.clone())
        } else if let Some(m) = self.session_list.iter().find(|m| m.id == id) {
            (meta_title(m), m.chat_dir.clone())
        } else {
            (crate::protocol::short_id(id), String::new())
        };
        self.delete_target = Some((id.to_string(), title, dir));
    }

    /// Apaga a conversa. A pasta do chat (código, docs, planilhas) fica.
    fn delete_chat(&mut self, id: &str) {
        let was_active =
            id == self.session.meta.id || id == self.session.meta.daemon_session_id;
        if was_active && self.busy {
            self.push_error("stop the agent before deleting the open chat".into());
            return;
        }
        // fecha a aba aberta deste chat, se houver
        if let Some(idx) = self.open_tabs.iter().position(|t| t.matches_session(id)) {
            if self.open_tabs.len() > 1 {
                self.close_tab(idx);
            }
        }
        let live = self.daemon_live.iter().any(|s| s.id == id);
        let ok = if live {
            self.daemon
                .as_ref()
                .map(|c| c.kill_session(id, true).is_ok())
                .unwrap_or(false)
        } else {
            session::delete_session(id).is_ok()
        };
        if !ok {
            // daemon fora do ar ou sessão desconhecida: o disco ainda é nosso
            let _ = session::delete_session(id);
        }
        self.session_list = session::list_sessions().unwrap_or_default();
        self.refresh_daemon_sessions();
        if was_active {
            self.new_session();
        }
        self.status = "chat deleted".into();
    }

    /// Diálogo de confirmação — diz o que some e o que fica.
    fn delete_window(&mut self, ctx: &egui::Context) {
        let Some((id, title, dir)) = self.delete_target.clone() else {
            return;
        };
        let p = pal();
        let mut close = false;
        let mut confirm = false;
        egui::Window::new("delete")
            .title_bar(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
            .default_width(460.0)
            .frame(crate::theme::card_frame().inner_margin(egui::Margin::same(18)))
            .show(ctx, |ui| {
                ui.set_width(430.0);
                ui.label(crate::theme::ui_medium("Delete this chat?", 16.0).color(p.text));
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(one_line(&title, 90))
                            .size(13.0)
                            .color(p.text_dim),
                    )
                    .wrap(),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    w::dot(ui, p.error, 3.0);
                    ui.label(
                        egui::RichText::new("The conversation is gone — this cannot be undone.")
                            .size(12.5)
                            .color(p.text_dim),
                    );
                });
                ui.horizontal(|ui| {
                    w::dot(ui, p.ok, 3.0);
                    ui.label(
                        egui::RichText::new("Generated files stay where they are.")
                            .size(12.5)
                            .color(p.text_dim),
                    );
                });
                if !dir.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(crate::theme::meta(shorten_path(std::path::Path::new(
                            &dir,
                        ))));
                        if w::chip(ui, "open folder").clicked() {
                            let path = PathBuf::from(&dir);
                            if path.is_dir() {
                                open_path(&path);
                            }
                        }
                    });
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if w::chip(ui, "Cancel").clicked() {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if w::danger_button(ui, "Delete chat").clicked() {
                            confirm = true;
                        }
                    });
                });
            });

        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if confirm {
            self.delete_chat(&id);
            self.delete_target = None;
        } else if close || esc {
            self.delete_target = None;
        }
    }

    /// Apaga TODOS os chats: vivos (daemon, delete_disk) + salvos no disco.
    /// Depois abre um chat novo. Não passa por `persist()`/`new_session()` —
    /// eles regravariam o chat atual (apagado) no disco.
    fn reset_all_chats(&mut self) {
        if self.busy
            || self.open_tabs.iter().any(|t| t.busy)
            || self.daemon_live.iter().any(|s| s.busy)
        {
            self.push_error("stop the running agents before deleting all chats".into());
            return;
        }
        // ids a apagar: vivos ∪ salvos (sem repetir)
        let mut ids: Vec<String> = self.daemon_live.iter().map(|s| s.id.clone()).collect();
        for m in &self.session_list {
            let id = if !m.daemon_session_id.is_empty() {
                m.daemon_session_id.clone()
            } else {
                m.id.clone()
            };
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in &ids {
            let live = self.daemon_live.iter().any(|s| &s.id == id);
            let mut ok = false;
            if live {
                ok = self
                    .daemon
                    .as_ref()
                    .map(|c| c.kill_session(id, true).is_ok())
                    .unwrap_or(false);
            }
            if !ok {
                let _ = session::delete_session(id);
            }
        }
        self.open_tabs.clear();
        self.active_tab = 0;
        // chat novo: via daemon quando conectado, senão local
        let mut created = false;
        if let Some(client) = &self.daemon {
            match client.create_session(self.mode, None, None) {
                Ok((id, chat_dir, title)) => {
                    let sess = Session::from_daemon(
                        id,
                        chat_dir,
                        title,
                        self.mode,
                        &self.cfg.workspace,
                    );
                    self.messages = sess
                        .ui_log
                        .iter()
                        .map(|l| UiMessage {
                            role: l.role.clone(),
                            text: l.text.clone(),
                        })
                        .collect();
                    self.artifacts = scan_artifacts(&sess.chat_path(), self.mode);
                    let _ = session::save_session(&sess);
                    self.session = sess;
                    self.llm_history = Vec::new();
                    self.input.clear();
                    self.stream_buf.clear();
                    self.pending_approval = None;
                    self.status =
                        format!("daemon · 📁 {}", self.session.meta.chat_folder_name);
                    created = true;
                }
                Err(e) => self.push_error(format!("new session after reset: {e}")),
            }
        }
        if !created {
            let sess = Session::new(self.mode, &self.cfg.workspace);
            self.messages = sess
                .ui_log
                .iter()
                .map(|l| UiMessage {
                    role: l.role.clone(),
                    text: l.text.clone(),
                })
                .collect();
            self.artifacts = scan_artifacts(&sess.chat_path(), self.mode);
            let _ = session::save_session(&sess);
            self.session = sess;
            self.llm_history = Vec::new();
            self.input.clear();
            self.stream_buf.clear();
            self.pending_approval = None;
        }
        self.session_list = session::list_sessions().unwrap_or_default();
        self.refresh_daemon_sessions();
        self.status = format!("{} chats deleted · fresh start", ids.len());
    }

    /// Confirmação do reset — nada é apagado sem o segundo clique.
    fn reset_window(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset {
            return;
        }
        let p = pal();
        // contagem prévia (o closure abaixo pede &mut self)
        let mut ids: std::collections::BTreeSet<String> =
            self.daemon_live.iter().map(|s| s.id.clone()).collect();
        for m in &self.session_list {
            let id = if !m.daemon_session_id.is_empty() {
                m.daemon_session_id.clone()
            } else {
                m.id.clone()
            };
            if !id.is_empty() {
                ids.insert(id);
            }
        }
        let count = ids.len();
        let busy_count = self.daemon_live.iter().filter(|s| s.busy).count();
        let mut close = false;
        let mut confirm = false;
        egui::Window::new("reset")
            .title_bar(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
            .default_width(460.0)
            .frame(crate::theme::card_frame().inner_margin(egui::Margin::same(18)))
            .show(ctx, |ui| {
                ui.set_width(430.0);
                ui.label(crate::theme::ui_medium("Delete ALL chats?", 16.0).color(p.text));
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{count} chats will be deleted."))
                            .size(13.0)
                            .color(p.text_dim),
                    )
                    .wrap(),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    w::dot(ui, p.error, 3.0);
                    ui.label(
                        egui::RichText::new(
                            "Every conversation is gone — this cannot be undone.",
                        )
                        .size(12.5)
                        .color(p.text_dim),
                    );
                });
                ui.horizontal(|ui| {
                    w::dot(ui, p.ok, 3.0);
                    ui.label(
                        egui::RichText::new("Generated files stay in their chat folders.")
                            .size(12.5)
                            .color(p.text_dim),
                    );
                });
                if busy_count > 0 {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        w::dot(ui, p.accent, 3.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "{busy_count} agent(s) still running — stop them first."
                            ))
                            .size(12.5)
                            .color(p.accent),
                        );
                    });
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if w::chip(ui, "Cancel").clicked() {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if w::danger_button(ui, "Delete all").clicked() {
                            confirm = true;
                        }
                    });
                });
            });

        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if confirm {
            self.confirm_reset = false;
            self.reset_all_chats();
        } else if close || esc {
            self.confirm_reset = false;
        }
    }

    /// Onde o agente deste chat trabalha. Mesma regra do daemon.
    fn project_root(&self) -> PathBuf {
        session::effective_root(
            self.session.meta.project_dir.as_deref(),
            &self.session.meta.chat_dir,
        )
    }

    /// Abre o seletor e aponta o chat para a pasta escolhida.
    fn pick_project_dir(&mut self) {
        let start = self
            .session
            .meta
            .project_dir
            .clone()
            .unwrap_or_else(|| self.cfg.workspace.display().to_string());
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Project folder this chat works on")
            .set_directory(&start)
            .pick_folder()
        {
            let id = self.active_session_key();
            let path = dir.display().to_string();
            self.update_session_meta(&id, None, None, Some(Some(path.clone())));
            self.status = format!("project: {path}");
            self.messages.push(UiMessage {
                role: "system".into(),
                text: format!(
                    "This chat now works on {path}\nWrites there ask for approval; \
                     reads and search are free."
                ),
            });
        }
    }

    fn clear_project_dir(&mut self) {
        let id = self.active_session_key();
        self.update_session_meta(&id, None, None, Some(None));
        self.status = "project cleared".into();
    }

    /// Id que o daemon reconhece para o chat aberto.
    fn active_session_key(&self) -> String {
        if self.session.meta.daemon_session_id.is_empty() {
            self.session.meta.id.clone()
        } else {
            self.session.meta.daemon_session_id.clone()
        }
    }

    /// Renomeia/fixa passando pelo daemon — a sessão viva tem a própria cópia
    /// do título, então mexer só no disco seria sobrescrito no próximo save.
    fn update_session_meta(
        &mut self,
        id: &str,
        title: Option<&str>,
        pinned: Option<bool>,
        project_dir: Option<Option<String>>,
    ) {
        let mut ok = false;
        if let Some(client) = &self.daemon {
            ok = client
                .update_session(id, title, pinned, project_dir.clone())
                .is_ok();
        }
        if !ok {
            let _ = session::update_meta(id, title, pinned, project_dir.clone());
        }
        // reflete no chat aberto sem esperar o refresh
        if self.session.meta.id == id || self.session.meta.daemon_session_id == id {
            if let Some(t) = title {
                let t = t.trim();
                if !t.is_empty() {
                    self.session.meta.title = t.chars().take(80).collect();
                    self.session.meta.title_locked = true;
                }
            }
            if let Some(p) = pinned {
                self.session.meta.pinned = p;
            }
            if let Some(p) = project_dir {
                self.session.meta.project_dir = p.filter(|v| !v.trim().is_empty());
            }
        }
        self.session_list = session::list_sessions().unwrap_or_default();
        self.refresh_daemon_sessions();
    }

    /// Override do chat vence o padrão global.
    fn token_less_level(&self) -> TokenLessLevel {
        self.session.meta.token_less.unwrap_or(self.cfg.token_less)
    }

    fn set_token_less(&mut self, level: TokenLessLevel) {
        self.session.meta.token_less = Some(level);
        self.persist();
        self.status = format!("token less cost: {}", level.tag());
    }

    /// Fim de turno com Gauntlet Loop: reenvia sozinho enquanto a resposta não
    /// trouxer o marcador. Ler o toggle aqui é o que faz desligá-lo interromper.
    fn gauntlet_tick(&mut self, reply: &str, stuck: bool) {
        use crate::gauntlet::{next_step, Stop, CONTINUE_MESSAGE};
        let on = self.session.meta.gauntlet;
        let max = self.cfg.gauntlet_max_iterations;
        match next_step(on, reply, stuck, self.gauntlet_iter, max) {
            None if !on => {}
            None => {
                self.gauntlet_iter += 1;
                // o rascunho do usuário volta depois: o laço não pode comê-lo
                let draft = std::mem::replace(&mut self.input, CONTINUE_MESSAGE.into());
                self.send_user_message();
                self.input = draft;
                self.status = format!("gauntlet {}/{max} · running…", self.gauntlet_iter);
            }
            Some(Stop::Done) => {
                self.gauntlet_iter = 0;
                self.status = "gauntlet loop: done".into();
            }
            Some(Stop::Exhausted) => {
                self.gauntlet_iter = 0;
                self.status = format!("gauntlet loop: stopped at {max} iterations");
            }
            Some(Stop::Stuck) => {
                self.gauntlet_iter = 0;
                self.status = "gauntlet loop: stopped — the turn was looping".into();
            }
        }
    }

    fn set_gauntlet(&mut self, on: bool) {
        self.session.meta.gauntlet = on;
        self.gauntlet_iter = 0;
        self.persist();
        self.status = if on {
            format!("gauntlet loop on · max {}", self.cfg.gauntlet_max_iterations)
        } else {
            "gauntlet loop off".into()
        };
    }

    fn swarm_running(&self) -> usize {
        self.swarm_snap
            .agents
            .iter()
            .filter(|a| a.state == crate::swarm::AgentState::Running)
            .count()
    }

    fn refresh_memory_list(&mut self) {
        self.memory_view = memory::with_store(|s| {
            let n = s.count().unwrap_or(0);
            let hits = s.list_recent(30)?;
            Ok(format!("{n} memories\n{}", memory::format_hits(&hits)))
        })
        .unwrap_or_else(|e| format!("memory error: {e}"));
    }

    fn spawn_update_check(&mut self) {
        if self.update_busy {
            return;
        }
        self.update_busy = true;
        self.update.message = "checking for updates…".into();
        let repo = self.cfg.update_repo.clone();
        // Store result via status string + re-check is simple; use thread + channel would need more state.
        // For UI we poll a static — keep it simple: run blocking on background and set via mutex.
        thread::spawn(move || {
            let st = update::check_for_update(&repo).unwrap_or_else(|e| UpdateStatus {
                current: update::CURRENT_VERSION.into(),
                message: format!("update check failed: {e}"),
                ..Default::default()
            });
            if let Ok(mut g) = UPDATE_SLOT.lock() {
                *g = Some(st);
            }
        });
    }

    fn poll_update_slot(&mut self) {
        if let Ok(mut g) = UPDATE_SLOT.lock() {
            if let Some(st) = g.take() {
                self.update = st;
                self.update_busy = false;
                if self
                    .update
                    .latest
                    .as_ref()
                    .is_some_and(|l| l.as_str() != update::CURRENT_VERSION)
                {
                    self.status = self.update.message.clone();
                }
            }
        }
    }

    fn persist(&mut self) {
        self.session.meta.mode = self.mode;
        self.session.meta.workspace = self.cfg.workspace.display().to_string();
        // keep chat_dir linked — never overwrite with root
        if self.session.meta.chat_dir.is_empty() {
            self.session.ensure_chat_dir(&self.cfg.workspace);
        }
        self.session.messages = self.llm_history.clone();
        self.session.ui_log = self
            .messages
            .iter()
            .map(|m| UiLogLine {
                role: m.role.clone(),
                text: m.text.clone(),
            })
            .collect();
        if self.session.ui_log.len() > 300 {
            let drain = self.session.ui_log.len() - 240;
            self.session.ui_log.drain(0..drain);
        }
        let _ = session::save_session(&self.session);
        self.session_list = session::list_sessions().unwrap_or_default();
    }

    /// Novo chat pelo botão/⌘N. Só abre uma sessão nova quando o chat atual
    /// já tem conteúdo — vazio (sem pergunta/resposta) não gera linha nova
    /// na lista nem sessão extra no daemon.
    fn new_chat(&mut self) {
        if session::has_content(&self.session) {
            self.new_session();
        } else {
            // segue no mesmo chat vazio, só limpa o rascunho
            self.input.clear();
            self.stream_buf.clear();
        }
    }

    fn new_session(&mut self) {
        // Allow a new tab even if the current tab is busy (multi-session).
        if !self.cfg.workspace_ready {
            self.show_setup = true;
            return;
        }
        self.persist();
        self.pack_active_tab();
        if let Some(client) = &self.daemon {
            match client.create_session(self.mode, None, None) {
                Ok((id, chat_dir, title)) => {
                    let sess = Session::from_daemon(
                        id,
                        chat_dir,
                        title,
                        self.mode,
                        &self.cfg.workspace,
                    );
                    let msgs: Vec<UiMessage> = sess
                        .ui_log
                        .iter()
                        .map(|l| UiMessage {
                            role: l.role.clone(),
                            text: l.text.clone(),
                        })
                        .collect();
                    let _ = session::save_session(&sess);
                    self.open_session_in_tab(sess, msgs, Vec::new());
                    self.status = format!("daemon · 📁 {}", self.session.meta.chat_folder_name);
                }
                Err(e) => self.push_error(format!("new session: {e}")),
            }
        } else {
            self.connect_daemon_and_session(None);
        }
        self.session_list = session::list_sessions().unwrap_or_default();
        self.refresh_daemon_sessions();
    }

    fn open_chat_folder(&self) {
        let p = self.session.chat_path();
        if p.is_dir() {
            open_path(&p);
        } else if self.cfg.workspace.is_dir() {
            open_path(&self.cfg.workspace);
        }
    }

    fn load_session_id(&mut self, id: &str) {
        // Allow opening another session in a new tab even if current is busy
        match session::load_session(id) {
            Ok(mut s) => {
                if s.meta.chat_dir.is_empty() {
                    s.ensure_chat_dir(&self.cfg.workspace);
                    let _ = session::save_session(&s);
                }
                let messages: Vec<UiMessage> = s
                    .ui_log
                    .iter()
                    .map(|l| UiMessage {
                        role: l.role.clone(),
                        text: l.text.clone(),
                    })
                    .collect();
                let mut history = s.messages.clone();
                if history
                    .first()
                    .map(|m| m.role == "system")
                    .unwrap_or(false)
                {
                    history.remove(0);
                }

                let daemon_id = if !s.meta.daemon_session_id.is_empty() {
                    s.meta.daemon_session_id.clone()
                } else {
                    s.meta.id.clone()
                };
                let chat_dir = s.meta.chat_dir.clone();
                let title = s.meta.title.clone();
                let mode = s.meta.mode;

                // Open as tab first so current busy chat stays in its tab
                if self.open_tabs.iter().any(|t| t.matches_session(id))
                    || self.session.meta.id == id
                    || self.session.meta.daemon_session_id == daemon_id
                {
                    // focus existing
                    if let Some(i) = self.open_tabs.iter().position(|t| t.matches_session(id)) {
                        if !self.busy || i == self.active_tab {
                            self.switch_tab(i);
                        }
                    }
                    // if active already this id, continue reattach below on active
                } else {
                    self.persist();
                    self.open_session_in_tab(s.clone(), messages.clone(), history.clone());
                }

                if self.daemon.is_none() {
                    self.connect_daemon_and_session(Some(chat_dir));
                } else {
                    enum Reattach {
                        Created {
                            sid: String,
                            dir: String,
                            title: String,
                            payload: Option<serde_json::Value>,
                        },
                        Attached(serde_json::Value),
                        Reregistered {
                            sid: String,
                            dir: String,
                            title: String,
                        },
                        Err(String),
                    }
                    let outcome = if let Some(client) = &self.daemon {
                        match client.create_session_ex(
                            mode,
                            Some(title.clone()),
                            Some(chat_dir.clone()),
                            Some(daemon_id.clone()),
                        ) {
                            Ok((sid, dir, t)) => {
                                let payload = client.attach(&sid).ok();
                                Reattach::Created {
                                    sid,
                                    dir,
                                    title: t,
                                    payload,
                                }
                            }
                            Err(_) => match client.attach(&daemon_id) {
                                Ok(payload) => Reattach::Attached(payload),
                                Err(_) => match client.create_session(
                                    mode,
                                    Some(title),
                                    Some(chat_dir),
                                ) {
                                    Ok((sid, dir, t)) => Reattach::Reregistered {
                                        sid,
                                        dir,
                                        title: t,
                                    },
                                    Err(e) => Reattach::Err(e.to_string()),
                                },
                            },
                        }
                    } else {
                        Reattach::Err("no daemon".into())
                    };
                    match outcome {
                        Reattach::Created {
                            sid,
                            dir,
                            title: t,
                            payload,
                        } => {
                            self.session.meta.id = sid.clone();
                            self.session.meta.daemon_session_id = sid;
                            if !dir.is_empty() {
                                self.session.meta.chat_dir = dir;
                            }
                            if !t.is_empty() {
                                self.session.meta.title = t;
                            }
                            let _ = session::save_session(&self.session);
                            if let Some(p) = payload {
                                self.merge_attach_history(&p);
                            }
                            self.status = format!(
                                "tab · 📁 {}",
                                self.session.meta.chat_folder_name
                            );
                        }
                        Reattach::Attached(payload) => {
                            self.merge_attach_history(&payload);
                            self.status = format!(
                                "tab · 📁 {}",
                                self.session.meta.chat_folder_name
                            );
                        }
                        Reattach::Reregistered { sid, dir, title: t } => {
                            self.session.meta.id = sid.clone();
                            self.session.meta.daemon_session_id = sid;
                            self.session.meta.chat_dir = dir;
                            self.session.meta.title = t;
                            let _ = session::save_session(&self.session);
                            self.status = format!(
                                "tab · re-registered · 📁 {}",
                                self.session.meta.chat_folder_name
                            );
                        }
                        Reattach::Err(e) => self.push_error(format!("reattach: {e}")),
                    }
                    let _ = messages;
                    let _ = history;
                    self.pack_active_tab();
                    self.refresh_daemon_sessions();
                }
            }
            Err(e) => self.push_error(format!("load session: {e}")),
        }
    }

    fn merge_attach_history(&mut self, payload: &serde_json::Value) {
        if let Some(busy) = payload.get("busy").and_then(|b| b.as_bool()) {
            self.busy = busy;
        }
        let Some(hist) = payload.get("history").and_then(|h| h.as_array()) else {
            return;
        };
        if !self.llm_history.is_empty() {
            return;
        }
        for m in hist {
            let role = m
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("assistant");
            let content = m
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if content.is_empty() {
                continue;
            }
            self.llm_history.push(ChatMessage {
                role: role.into(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
            self.messages.push(UiMessage {
                role: role.into(),
                text: content,
            });
        }
    }

    fn set_mode(&mut self, mode: AppMode) {
        if self.busy || self.mode == mode {
            return;
        }
        self.mode = mode;
        self.cfg.mode = mode;
        self.session.meta.mode = mode;
        let _ = self.cfg.save();
        self.messages.push(UiMessage {
            role: "system".into(),
            text: format!(
                "Mode → {} — same chat folder 📁 {}",
                mode.label(),
                self.session.meta.chat_folder_name
            ),
        });
        self.artifacts = scan_artifacts(&self.session.chat_path(), mode);
        self.persist();
    }

    fn push_error(&mut self, text: String) {
        self.messages.push(UiMessage {
            role: "error".into(),
            text,
        });
    }

    fn open_preview(&mut self, path: PathBuf) {
        let content = preview::preview_path(&path);
        match &content {
            PreviewContent::WebPage { url, path: p, .. } => {
                self.browser_url = url.clone();
                self.status = format!("web preview: {p}");
            }
            _ => {
                self.status = format!("preview: {}", path.display());
            }
        }
        self.preview = Some(content);
        self.ctx_panel = true;
        self.ctx_tab = CtxTab::Preview;
    }

    fn run_diagnostics(&mut self) {
        let root = self.cfg.workspace.clone();
        self.status = "diagnostics…".into();
        let snap = diagnostics::run_workspace_diagnostics(&root, None);
        diagnostics::store_snapshot(snap.clone());
        self.diagnostics = snap;
        self.dest = Dest::Diag;
        self.status = self.diagnostics.summary.clone();
    }

    fn send_user_message(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.busy {
            return;
        }
        if self.cfg.needs_workspace() {
            self.show_setup = true;
            self.push_error(
                "Pick the default output folder (code, docs, sheets, pdfs) in setup."
                    .into(),
            );
            return;
        }
        if self.cfg.needs_api() {
            self.show_setup = true;
            self.push_error("API key missing — open Settings / Setup.".into());
            return;
        }
        // Slash commands (local, no LLM)
        match slash::parse(&text) {
            SlashAction::NotSlash => {}
            other => {
                self.input.clear();
                self.handle_slash(other);
                return;
            }
        }
        if self.daemon.is_none() {
            self.connect_daemon_and_session(Some(self.session.meta.chat_dir.clone()));
        }
        let Some(client) = &self.daemon else {
            self.push_error("daemon not connected".into());
            return;
        };
        let sid = if !self.session.meta.daemon_session_id.is_empty() {
            self.session.meta.daemon_session_id.clone()
        } else {
            self.session.meta.id.clone()
        };
        if sid.is_empty() {
            self.push_error("no daemon session — click + New chat".into());
            return;
        }

        self.session.ensure_chat_dir(&self.cfg.workspace);
        let _ = plan::load(&self.session.chat_path());
        let _ = crate::skills::ensure_default_skills(&self.session.chat_path());
        self.input.clear();
        self.session.touch_title_from_user(&text);
        self.messages.push(UiMessage {
            role: "user".into(),
            text: text.clone(),
        });
        self.llm_history.push(ChatMessage {
            role: "user".into(),
            content: Some(text.clone()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        if text != crate::gauntlet::CONTINUE_MESSAGE {
            self.gauntlet_iter = 0;
        }
        let token_less = self.session.meta.token_less.unwrap_or(self.cfg.token_less);
        if let Err(e) = client.user_message(&sid, &text, Some(token_less), Some(self.session.meta.gauntlet)) {
            self.push_error(format!("daemon send: {e}"));
            return;
        }
        self.busy = true;
        self.stream_buf.clear();
        self.pending_approval = None;
        self.status = format!("daemon · 📁 {} · running…", self.session.meta.chat_folder_name);
    }

    fn stop_agent(&mut self) {
        let sid = self.session.meta.daemon_session_id.clone();
        if sid.is_empty() {
            return;
        }
        if let Some(client) = &self.daemon {
            let _ = client.cancel(&sid);
            self.status = "cancelling…".into();
        }
    }

    fn handle_slash(&mut self, action: SlashAction) {
        match action {
            SlashAction::NotSlash => {}
            SlashAction::Help(t) | SlashAction::Unknown(t) => {
                let text = if t == "__llm_list__" {
                    crate::llm_pool::list_text(&self.cfg)
                } else if t == "__llm_weights__" {
                    crate::llm_pool::weights_text(&self.cfg, self.mode)
                } else if t == "__llm_failover_toggle__" {
                    self.cfg.llm_auto_failover = !self.cfg.llm_auto_failover;
                    let _ = self.cfg.save();
                    format!(
                        "llm_auto_failover = {} (memory always kept on switch)",
                        self.cfg.llm_auto_failover
                    )
                } else if t == "__llm_rotate_on__" {
                    self.cfg.llm_rotate_enabled = true;
                    let _ = self.cfg.save();
                    if let Some(n) = crate::llm_pool::maybe_rotate(&mut self.cfg, self.mode) {
                        format!("rotation ON — {n}")
                    } else {
                        format!(
                            "rotation ON every {} min\n{}",
                            self.cfg.llm_rotate_minutes,
                            crate::llm_pool::weights_text(&self.cfg, self.mode)
                        )
                    }
                } else if t == "__llm_rotate_off__" {
                    self.cfg.llm_rotate_enabled = false;
                    let _ = self.cfg.save();
                    "rotation OFF — use /llm use <name> to pick manually".into()
                } else if let Some(mins) = t.strip_prefix("__llm_rotate_mins__:") {
                    match mins.trim().parse::<u32>() {
                        Ok(m) if m >= 1 => {
                            self.cfg.llm_rotate_minutes = m.clamp(1, 60 * 24 * 7);
                            self.cfg.llm_rotate_enabled = true;
                            self.cfg.llm_rotate_slot.clear(); // force re-pick
                            let _ = self.cfg.save();
                            let note = crate::llm_pool::maybe_rotate(&mut self.cfg, self.mode)
                                .unwrap_or_else(|| {
                                    format!("every {} min · active={}", m, self.cfg.active_llm)
                                });
                            format!("rotation every {m} minutes\n{note}")
                        }
                        _ => "Usage: /llm every 60  (minutes, min 1)".into(),
                    }
                } else if t == "__sessions__" {
                    self.refresh_daemon_sessions();
                    let mut lines = vec![
                        self.daemon_info_line.clone(),
                        self.mem_line.clone(),
                        format!(
                            "gui_tabs={} active={}",
                            self.open_tabs.len(),
                            crate::protocol::short_id(&self.session.meta.daemon_session_id)
                        ),
                    ];
                    for (i, tab) in self.open_tabs.iter().enumerate() {
                        let mark = if i == self.active_tab { "→" } else { " " };
                        let busy = if i == self.active_tab {
                            self.busy
                        } else {
                            tab.busy
                        };
                        let bytes = if i == self.active_tab {
                            self.active_tab_approx_bytes()
                        } else {
                            tab.approx_bytes()
                        };
                        lines.push(format!(
                            "{mark} tab{} {} {} ~{}",
                            i + 1,
                            if busy { "●" } else { "○" },
                            tab.tab_title(),
                            crate::mem_stats::format_bytes(bytes)
                        ));
                    }
                    lines.push("--- daemon live ---".into());
                    for s in &self.daemon_live {
                        lines.push(format!(
                            "{} {} [{}] sub={} msgs={} ~{} {} — {}",
                            if s.busy { "●" } else { "○" },
                            s.short_id,
                            s.mode,
                            s.subscribers,
                            s.history_msgs,
                            crate::mem_stats::format_bytes(s.approx_bytes as usize),
                            s.folder,
                            s.title
                        ));
                    }
                    if self.daemon_live.is_empty() {
                        lines.push("(no live sessions)".into());
                    }
                    lines.join("\n")
                } else if t.starts_with('/') {
                    format!("Unknown command {t}\n{}", slash::help_text())
                } else {
                    t
                };
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text,
                });
            }
            SlashAction::ClearChat => {
                self.llm_history.clear();
                self.messages.clear();
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: format!(
                        "Chat cleared · folder {}",
                        self.session.meta.chat_folder_name
                    ),
                });
            }
            SlashAction::OpenFolder => self.open_chat_folder(),
            SlashAction::OpenRoot => open_path(&self.cfg.workspace),
            SlashAction::SetMode(m) => self.set_mode(m),
            SlashAction::SetModel(m) => {
                if let Some(profile) = m.strip_prefix("__profile__:") {
                    match crate::provider_doctor::apply_profile(&mut self.cfg, profile.trim()) {
                        Ok(msg) => {
                            // Clone before mut borrow for upsert
                            let ep = crate::llm_pool::LlmEndpoint {
                                name: profile.trim().into(),
                                api_base: self.cfg.api_base.clone(),
                                api_key: self.cfg.api_key.clone(),
                                model: self.cfg.model.clone(),
                                enabled: true,
                                priority: 0,
                                weight: 50,
                                use_for_code: true,
                                use_for_office: true,
                                price_in: 0.0,
                                price_out: 0.0,
                                // meta.ai fala Responses API, não Chat Completions
                                wire: if profile.trim() == "meta" {
                                    "responses".into()
                                } else {
                                    String::new()
                                },
                                use_for_workers: profile.trim() == "ollama"
                                    || profile.trim() == "lmstudio",
                            };
                            crate::llm_pool::upsert_endpoint(&mut self.cfg, ep);
                            self.cfg.active_llm = profile.trim().into();
                            // runtime_active velho (de failover/rotação) tem
                            // prioridade sobre active_llm no resolve_endpoint —
                            // troca antes do seed para não vazar o endpoint antigo.
                            crate::llm_pool::set_runtime_active(&self.cfg.active_llm);
                            crate::llm_pool::set_failover_note("");
                            // Seed depois do upsert: ensure_pool sincroniza os
                            // campos flat a partir do endpoint ativo (o perfil).
                            crate::llm_pool::ensure_pool(&mut self.cfg);
                            self.draft_api_base = self.cfg.api_base.clone();
                            self.draft_api_key = self.cfg.api_key.clone();
                            self.draft_model = self.cfg.model.clone();
                            let _ = self.cfg.save();
                            self.messages.push(UiMessage {
                                role: "system".into(),
                                text: format!("{msg}\n(Chat + memory kept)"),
                            });
                        }
                        Err(e) => self.push_error(e.to_string()),
                    }
                } else if let Some(name) = m.strip_prefix("__llm__:") {
                    match crate::llm_pool::set_active(&mut self.cfg, name.trim()) {
                        Ok(msg) => {
                            self.draft_api_base = self.cfg.api_base.clone();
                            self.draft_api_key = self.cfg.api_key.clone();
                            self.draft_model = self.cfg.model.clone();
                            let _ = self.cfg.save();
                            self.messages.push(UiMessage {
                                role: "system".into(),
                                text: format!("{msg}\n(Chat history + memory kept)"),
                            });
                        }
                        Err(e) => self.push_error(e.to_string()),
                    }
                } else {
                    self.cfg.model = m.clone();
                    self.draft_model = m.clone();
                    // update active pool entry model
                    if let Some(ep) = self
                        .cfg
                        .llm_pool
                        .iter_mut()
                        .find(|e| e.name == self.cfg.active_llm)
                    {
                        ep.model = m.clone();
                    }
                    let _ = self.cfg.save();
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: format!("model → {m} (same provider; memory kept)"),
                    });
                }
            }
            SlashAction::MemorySearch(q) => {
                let text = crate::memory::with_store(|s| s.search(&q, 10))
                    .map(|h| crate::memory::format_hits(&h))
                    .unwrap_or_else(|e| e.to_string());
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text,
                });
                self.dest = Dest::Memory;
            }
            SlashAction::MemoryStore(t) => {
                match crate::memory::with_store(|s| s.store(&t, "slash")) {
                    Ok(id) => self.messages.push(UiMessage {
                        role: "system".into(),
                        text: format!("stored memory #{id}"),
                    }),
                    Err(e) => self.push_error(e.to_string()),
                }
                self.refresh_memory_list();
            }
            SlashAction::Diagnostics => self.run_diagnostics(),
            SlashAction::SwarmList => {
                self.refresh_swarm(true);
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: swarm_snapshot_text(&self.swarm_snap),
                });
                self.dest = Dest::Swarm;
            }
            SlashAction::ServerStart { path, port } => {
                let port = port.unwrap_or(self.cfg.web_server_port);
                let root = self.session.chat_path().join(&path);
                match webserver::start(root, port) {
                    Ok(s) => {
                        self.browser_url = s.url.clone();
                        let _ = browser::open_in_app(&s.url);
                        self.messages.push(UiMessage {
                            role: "system".into(),
                            text: format!("server {} · webview opened", s.url),
                        });
                        self.dest = Dest::WebServer;
                    }
                    Err(e) => self.push_error(e.to_string()),
                }
            }
            SlashAction::ServerStop => {
                webserver::stop();
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: "server stopped".into(),
                });
            }
            SlashAction::WebOpen(url) => match browser::open_in_app(&url) {
                Ok(()) => {
                    self.browser_url = url;
                    self.status = "webview opened".into();
                }
                Err(e) => self.push_error(e.to_string()),
            },
            SlashAction::TokenLess(level) => {
                let text = match level {
                    Some(l) => {
                        self.session.meta.token_less = Some(l);
                        self.persist();
                        format!(
                            "token less cost: {} neste chat ({}). Só encurta a saída — entrada e \
                             raciocínio seguem iguais.",
                            l.tag(),
                            l.label()
                        )
                    }
                    None => format!(
                        "token less cost deste chat: {} (padrão global: {})\n\
                         /tokenless off|lite|full|ultra",
                        self.token_less_level().tag(),
                        self.cfg.token_less.tag()
                    ),
                };
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text,
                });
            }
            SlashAction::Graph(arg) => {
                self.dest = Dest::Graph;
                let a = arg.trim();
                if a.is_empty() {
                    self.check_graph_stale();
                    let g = &self.graph_stats;
                    let text = if g.files == 0 {
                        "graph not indexed — /graph build".to_string()
                    } else {
                        format!(
                            "grafo: {} arquivos · {} símbolos · {} referências · {} clusters\n\
                             {} arquivo(s) mudaram desde a build · {} tokens de leitura evitados",
                            g.files,
                            g.symbols,
                            g.edges,
                            g.clusters,
                            g.stale_files,
                            fmt_tokens(self.metrics.graph_saved_tokens)
                        )
                    };
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text,
                    });
                } else if let Some(sym) = a
                    .strip_prefix("impact ")
                    .or_else(|| a.strip_prefix("impacto "))
                {
                    self.graph_query = sym.trim().to_string();
                    self.run_graph_impact();
                    let answer = self.graph_answer.clone();
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: answer,
                    });
                } else if a == "build" || a == "index" {
                    self.run_graph_build(false);
                    let msg = self.status.clone();
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: msg,
                    });
                } else {
                    self.graph_query = a.to_string();
                    self.run_graph_query();
                    let answer = self.graph_answer.clone();
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: answer,
                    });
                }
            }
            SlashAction::Rename(t) => {
                let id = self.active_session_key();
                self.update_session_meta(&id, Some(&t), None, None);
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: format!("chat renamed: {t}"),
                });
            }
            SlashAction::Pin(v) => {
                let id = self.active_session_key();
                let now = v.unwrap_or(!self.session.meta.pinned);
                self.update_session_meta(&id, None, Some(now), None);
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: if now {
                        "chat pinned to top".into()
                    } else {
                        "chat unpinned".into()
                    },
                });
            }
            SlashAction::ToggleUsage => self.show_usage = !self.show_usage,
            SlashAction::Project(arg) => {
                let a = arg.trim();
                if a.is_empty() {
                    self.pick_project_dir();
                } else if a == "off" || a == "none" {
                    self.clear_project_dir();
                } else if !std::path::Path::new(a).is_absolute() {
                    self.push_error("project path must be absolute".into());
                } else {
                    let id = self.active_session_key();
                    self.update_session_meta(&id, None, None, Some(Some(a.to_string())));
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: format!("This chat now works on {a}"),
                    });
                }
            }
            SlashAction::Delete => {
                let id = self.active_session_key();
                self.ask_delete(&id);
            }
            SlashAction::SideClear => {
                side_panel::clear();
                self.ctx_panel = true;
                self.ctx_tab = CtxTab::Side;
            }
            SlashAction::Compact => {
                self.llm_history = crate::llm::compact_history(
                    &self.llm_history,
                    self.cfg.history_cap,
                    self.cfg.tool_result_cap,
                );
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: "history compacted".into(),
                });
            }
            SlashAction::Status => {
                let t = slash::status_line(
                    &self.cfg,
                    &self.session.meta.chat_folder_name,
                    self.mode,
                );
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: t,
                });
            }
        }
        self.persist();
    }

    fn decide_approval(&mut self, d: ApprovalDecision) {
        let sid = self.session.meta.daemon_session_id.clone();
        if let Some(client) = &self.daemon {
            if !sid.is_empty() {
                let _ = client.approve(&sid, d.clone());
            }
        }
        // Volta a linha de "precisa aprovação" para rodando/negado.
        if let Some((name, _)) = self.pending_approval.clone() {
            for m in self.messages.iter_mut().rev() {
                if m.role != "tool" {
                    continue;
                }
                if let Some(mut tc) = ToolCall::parse(&m.text) {
                    if tc.name == name && tc.state == ToolState::NeedsApproval {
                        if matches!(d, ApprovalDecision::Deny) {
                            tc.state = ToolState::Err;
                            tc.metric = "denied".into();
                        } else {
                            tc.state = ToolState::Running;
                            tc.metric.clear();
                        }
                        m.text = tc.encode();
                        break;
                    }
                }
            }
        }
        self.pending_approval = None;
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        // Drain daemon connection (events + disconnect)
        let mut events: Vec<AgentEvent> = Vec::new();
        let mut bg_events: Vec<(String, AgentEvent)> = Vec::new();
        let mut bus_notes: Vec<String> = Vec::new();
        let mut other_done: Vec<String> = Vec::new();
        let mut disconnected = None;
        let active = self.session.meta.daemon_session_id.clone();
        let active_id = self.session.meta.id.clone();
        if let Some(client) = &self.daemon {
            while let Some(inc) = client.try_recv() {
                match inc {
                    Incoming::Event { session_id, event } => {
                        let mine = session_id == active || session_id == active_id;
                        if mine {
                            events.push(event);
                        } else {
                            if matches!(event, AgentEvent::Done { .. }) {
                                other_done.push(crate::protocol::short_id(&session_id));
                            }
                            bg_events.push((session_id, event));
                        }
                    }
                    Incoming::RawEvent {
                        session_id,
                        event,
                        payload,
                    } if event == "bus" => {
                        let from = payload
                            .get("from")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let body = payload
                            .get("body")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        bus_notes.push(format!(
                            "bus {}→{}: {}",
                            crate::protocol::short_id(from),
                            crate::protocol::short_id(&session_id),
                            body.chars().take(120).collect::<String>()
                        ));
                    }
                    Incoming::Disconnected(e) => disconnected = Some(e),
                    Incoming::Reply(_) | Incoming::RawEvent { .. } => {}
                }
            }
        }
        // Apply background tab events (inactive multi-session tabs)
        for (sid, ev) in bg_events {
            self.apply_event_to_tab(&sid, ev);
        }
        for n in bus_notes {
            self.messages.push(UiMessage {
                role: "system".into(),
                text: n,
            });
        }
        for sid in other_done {
            self.messages.push(UiMessage {
                role: "system".into(),
                text: format!("tab/session {sid} finished (background)"),
            });
            if let Some(s) = self.daemon_live.iter_mut().find(|s| s.short_id == sid) {
                s.busy = false;
            }
        }
        if let Some(e) = disconnected {
            self.daemon = None;
            self.busy = false;
            for t in &mut self.open_tabs {
                t.busy = false;
            }
            self.push_error(format!("daemon disconnected: {e}"));
            self.status = "daemon offline".into();
            return;
        }
        let any_busy = self.busy || self.open_tabs.iter().any(|t| t.busy);
        if events.is_empty() {
            if any_busy {
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
            self.refresh_mem_stats(false);
            return;
        }

        let mut done = false;
        let mut final_reply = None;
        let mut cancelled = false;
        let mut turn_stuck = false;
        let mut turn_failed = false;

        for ev in events {
            match ev {
                AgentEvent::Status(s) => self.status = s,
                AgentEvent::StreamDelta(d) => {
                    self.meter.stream_chars(d.chars().count());
                    self.stream_buf.push_str(&d);
                    if let Some(last) = self.messages.last_mut() {
                        if last.role == "assistant" {
                            last.text = self.stream_buf.clone();
                        } else {
                            self.messages.push(UiMessage {
                                role: "assistant".into(),
                                text: self.stream_buf.clone(),
                            });
                        }
                    } else {
                        self.messages.push(UiMessage {
                            role: "assistant".into(),
                            text: self.stream_buf.clone(),
                        });
                    }
                    self.status = "streaming…".into();
                }
                AgentEvent::ToolStart { name, args } => {
                    self.messages.push(UiMessage {
                        role: "tool".into(),
                        text: ToolCall::start(&name, &args).encode(),
                    });
                    self.status = format!("tool: {name}");
                }
                AgentEvent::ToolResult { name, result } => {
                    finish_tool(&mut self.messages, &name, &result);
                    if name == "get_diagnostics" {
                        self.diagnostics = diagnostics::load_snapshot();
                    }
                    if name.starts_with("swarm_") {
                        self.dest = Dest::Swarm;
                    }
                    if name == "side_panel" || name.starts_with("plan_") {
                        self.ctx_panel = true;
                self.ctx_tab = CtxTab::Side;
                    }
                    if name.starts_with("write_file")
                        || name == "replace_in_file"
                        || name == "apply_patch"
                    {
                        // refresh artifacts after edits
                        self.artifacts = scan_artifacts(&self.session.chat_path(), self.mode);
                    }
                }
                AgentEvent::NeedApproval { name, args_preview } => {
                    mark_awaiting(&mut self.messages, &name, &args_preview);
                    self.status = format!("approval: {name}");
                    self.pending_approval = Some((name, args_preview));
                }
                AgentEvent::Done { reply, stuck } => {
                    final_reply = Some(reply);
                    turn_stuck = stuck;
                    done = true;
                }
                AgentEvent::Error(e) => {
                    self.push_error(e);
                    turn_failed = true;
                    done = true;
                }
                AgentEvent::Cancelled => {
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: "cancelled".into(),
                    });
                    cancelled = true;
                    done = true;
                }
            }
        }

        if done {
            let last_reply = final_reply.clone().unwrap_or_default();
            if let Some(reply) = final_reply {
                if self
                    .messages
                    .last()
                    .map(|m| m.role != "assistant")
                    .unwrap_or(true)
                {
                    self.messages.push(UiMessage {
                        role: "assistant".into(),
                        text: reply.clone(),
                    });
                } else if let Some(last) = self.messages.last_mut() {
                    last.text = reply.clone();
                }
                self.llm_history.push(ChatMessage {
                    role: "assistant".into(),
                    content: Some(reply),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            if self.messages.len() > 220 {
                let drain = self.messages.len() - 180;
                self.messages.drain(0..drain);
            }
            if self.llm_history.len() > self.cfg.history_cap + 10 {
                let drain = self.llm_history.len() - self.cfg.history_cap;
                self.llm_history.drain(0..drain);
            }
            self.busy = false;
            self.status = if cancelled {
                format!("cancelled · daemon · {}", self.session.meta.chat_folder_name)
            } else {
                format!("idle · daemon · {}", self.session.meta.chat_folder_name)
            };
            self.pending_approval = None;
            self.stream_buf.clear();
            self.artifacts = scan_artifacts(&self.session.chat_path(), self.mode);
            // Gerou uma página nova? Deixa o painel pronto e o mostra uma vez.
            // `preview_path_quiet` sobe o servidor mas não abre janela — quem
            // decide ver o jogo é o usuário, no botão Run.
            if let Some(html) = html_artifacts(&self.artifacts).first() {
                if self.announced_page.as_deref() != Some(html.as_path()) {
                    let content = preview::preview_path_quiet(html);
                    if let PreviewContent::WebPage { url, .. } = &content {
                        self.browser_url = url.clone();
                    }
                    self.preview = Some(content);
                    self.ctx_tab = CtxTab::Preview;
                    self.ctx_panel = true;
                    self.announced_page = Some(html.clone());
                }
            }
            self.persist();
            self.pack_active_tab();
            self.refresh_mem_stats(true);
            // turno que falhou não é "objetivo incompleto": reenviar
            // "continue o loop" em cima do erro só multiplica a falha
            if !cancelled && !turn_failed {
                self.gauntlet_tick(&last_reply, turn_stuck);
            } else if turn_failed && self.session.meta.gauntlet && self.gauntlet_iter > 0 {
                self.gauntlet_iter = 0;
                self.status = "gauntlet loop: stopped — the turn failed".into();
            }
        } else if self.busy || self.open_tabs.iter().any(|t| t.busy) {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    /// Update an inactive open tab when its daemon session streams events.
    fn apply_event_to_tab(&mut self, session_id: &str, ev: AgentEvent) {
        let Some(idx) = self
            .open_tabs
            .iter()
            .position(|t| t.matches_session(session_id))
        else {
            return;
        };
        if idx == self.active_tab {
            // Should have been routed as active — ignore to avoid double-apply
            return;
        }
        let Some(tab) = self.open_tabs.get_mut(idx) else {
            return;
        };
        match ev {
            AgentEvent::Status(s) => tab.status = s,
            AgentEvent::StreamDelta(d) => {
                tab.stream_buf.push_str(&d);
                if let Some(last) = tab.messages.last_mut() {
                    if last.role == "assistant" {
                        last.text = tab.stream_buf.clone();
                    } else {
                        tab.messages.push(UiMessage {
                            role: "assistant".into(),
                            text: tab.stream_buf.clone(),
                        });
                    }
                } else {
                    tab.messages.push(UiMessage {
                        role: "assistant".into(),
                        text: tab.stream_buf.clone(),
                    });
                }
                tab.busy = true;
            }
            AgentEvent::ToolStart { name, args } => {
                tab.messages.push(UiMessage {
                    role: "tool".into(),
                    text: ToolCall::start(&name, &args).encode(),
                });
                tab.busy = true;
            }
            AgentEvent::ToolResult { name, result } => {
                finish_tool(&mut tab.messages, &name, &result);
            }
            AgentEvent::NeedApproval { name, args_preview } => {
                mark_awaiting(&mut tab.messages, &name, &args_preview);
                tab.pending_approval = Some((name, args_preview));
                tab.busy = true;
            }
            AgentEvent::Done { reply, .. } => {
                if !reply.is_empty() {
                    if tab.stream_buf.is_empty() {
                        tab.messages.push(UiMessage {
                            role: "assistant".into(),
                            text: reply.clone(),
                        });
                    } else if let Some(last) = tab.messages.last_mut() {
                        if last.role == "assistant" {
                            last.text = reply.clone();
                        }
                    }
                    tab.llm_history.push(ChatMessage {
                        role: "assistant".into(),
                        content: Some(reply),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
                tab.session.messages = tab.llm_history.clone();
                tab.busy = false;
                tab.stream_buf.clear();
                tab.pending_approval = None;
                tab.status = format!("idle · 📁 {}", tab.session.meta.chat_folder_name);
                let _ = session::save_session(&tab.session);
            }
            AgentEvent::Error(e) => {
                tab.messages.push(UiMessage {
                    role: "error".into(),
                    text: e,
                });
                tab.busy = false;
                tab.pending_approval = None;
                tab.stream_buf.clear();
            }
            AgentEvent::Cancelled => {
                tab.busy = false;
                tab.pending_approval = None;
                tab.stream_buf.clear();
                tab.status = "cancelled".into();
            }
        }
    }

    fn apply_settings(&mut self) {
        self.cfg.api_base = self.draft_api_base.trim().to_string();
        self.cfg.api_key = self.draft_api_key.trim().to_string();
        self.cfg.model = self.draft_model.trim().to_string();
        // Seed do pool (defaults + meta) usa os campos flat como semente.
        crate::llm_pool::ensure_pool(&mut self.cfg);
        // ensure_pool sincroniza os campos flat *a partir* do endpoint ativo —
        // reaplica os drafts para a edição do usuário não ser revertida.
        self.cfg.api_base = self.draft_api_base.trim().to_string();
        self.cfg.api_key = self.draft_api_key.trim().to_string();
        self.cfg.model = self.draft_model.trim().to_string();
        let active = if self.cfg.active_llm.is_empty() {
            "primary".to_string()
        } else {
            self.cfg.active_llm.clone()
        };
        // Clone fields before mut borrow for upsert
        if self.cfg.llm_pool.iter().any(|e| e.name == active) {
            // Já existe: atualiza credenciais e modelo, preservando
            // wire, pesos, preços e flags que o usuário ajustou no painel.
            let (base, key, model) = (
                self.cfg.api_base.clone(),
                self.cfg.api_key.clone(),
                self.cfg.model.clone(),
            );
            for e in self.cfg.llm_pool.iter_mut() {
                if e.name == active {
                    e.api_base = base.clone();
                    e.api_key = key.clone();
                    e.model = model.clone();
                    e.enabled = true;
                }
            }
        } else {
            let ep = crate::llm_pool::LlmEndpoint {
                name: active.clone(),
                api_base: self.cfg.api_base.clone(),
                api_key: self.cfg.api_key.clone(),
                model: self.cfg.model.clone(),
                enabled: true,
                priority: 0,
                weight: 50,
                use_for_code: true,
                use_for_office: true,
                use_for_workers: false,
                price_in: 0.0,
                price_out: 0.0,
                wire: String::new(),
            };
            crate::llm_pool::upsert_endpoint(&mut self.cfg, ep);
        }
        self.cfg.active_llm = active;
        crate::llm_pool::set_runtime_active(&self.cfg.active_llm);
        let ws = PathBuf::from(self.draft_workspace.trim());
        if !config::workspace_path_ok(&ws) {
            self.push_error(
                "Invalid default folder — pick an absolute path (e.g. Documents/Harness)."
                    .into(),
            );
            return;
        }
        self.cfg.workspace = ws;
        self.cfg.workspace_ready = true;
        self.cfg.mode = self.mode;
        self.cfg.auto_approve_shell = self.draft_auto_shell;
        self.cfg.stream = self.draft_stream;
        self.cfg.update_repo = self.draft_update_repo.trim().to_string();
        if let Err(e) = config::ensure_workspace_layout(&self.cfg.workspace) {
            self.push_error(format!("create workspace folders: {e}"));
            return;
        }
        if let Err(e) = self.cfg.save() {
            self.push_error(format!("save config: {e}"));
        } else {
            self.show_setup = self.cfg.needs_setup();
            self.connect_daemon_and_session(None);
            self.messages.push(UiMessage {
                role: "system".into(),
                text: format!(
                    "Root: {}\nDaemon session: {}\nAgent runs on multi-client daemon (reattach OK).",
                    self.cfg.workspace.display(),
                    self.session.chat_path().display()
                ),
            });
        }
        self.show_settings = false;
    }
}

static UPDATE_SLOT: std::sync::Mutex<Option<UpdateStatus>> = std::sync::Mutex::new(None);

/// Fecha a última chamada aberta de `name` com o resultado (em vez de empilhar
/// uma segunda linha "✓ name: …").
fn finish_tool(messages: &mut Vec<UiMessage>, name: &str, result: &str) {
    for m in messages.iter_mut().rev() {
        if m.role != "tool" {
            continue;
        }
        if let Some(mut tc) = ToolCall::parse(&m.text) {
            if tc.name == name && !tc.state.is_final() {
                tc.finish(result);
                m.text = tc.encode();
                return;
            }
        }
    }
    let mut tc = ToolCall::start(name, "");
    tc.finish(result);
    messages.push(UiMessage {
        role: "tool".into(),
        text: tc.encode(),
    });
}

/// Marca a linha da tool como "precisa aprovação" (aprovação inline, sem modal).
fn mark_awaiting(messages: &mut Vec<UiMessage>, name: &str, preview: &str) {
    for m in messages.iter_mut().rev() {
        if m.role != "tool" {
            continue;
        }
        if let Some(mut tc) = ToolCall::parse(&m.text) {
            if tc.name == name && !tc.state.is_final() {
                tc.state = ToolState::NeedsApproval;
                tc.metric = "needs approval".into();
                if !preview.trim().is_empty() {
                    tc.body = preview.to_string();
                    if tc.target.is_empty() {
                        tc.target = one_line(preview, 72);
                    }
                }
                m.text = tc.encode();
                return;
            }
        }
    }
    let mut tc = ToolCall::start(name, preview);
    tc.state = ToolState::NeedsApproval;
    tc.metric = "needs approval".into();
    messages.push(UiMessage {
        role: "tool".into(),
        text: tc.encode(),
    });
}

// ---------------------------------------------------------------------------
// Shell: rail · lista de sessões · chat · status bar · ⌘K
// ---------------------------------------------------------------------------

impl HarnessApp {
    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.cfg.theme = self.cfg.theme.toggled();
        crate::theme::set_mode(ctx, self.cfg.theme);
        let _ = self.cfg.save();
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (send, palette, theme, new_chat, esc) = ctx.input(|i| {
            let cmd = i.modifiers.ctrl || i.modifiers.command;
            (
                i.key_pressed(egui::Key::Enter) && cmd,
                i.key_pressed(egui::Key::K) && cmd,
                i.key_pressed(egui::Key::D) && cmd && i.modifiers.shift,
                i.key_pressed(egui::Key::N) && cmd,
                i.key_pressed(egui::Key::Escape),
            )
        });
        if send && !self.busy && !self.cmdk {
            self.send_user_message();
        }
        if palette {
            self.cmdk = !self.cmdk;
            self.cmdk_query.clear();
            self.cmdk_sel = 0;
        }
        if theme {
            self.toggle_theme(ctx);
        }
        if new_chat {
            self.new_chat();
        }
        if esc {
            if self.cmdk {
                self.cmdk = false;
            } else if self.ctx_panel {
                self.ctx_panel = false;
            } else if self.show_usage && !self.cfg.usage_pinned {
                self.show_usage = false;
            }
        }
    }

    /// Rail vertical de 68px — cinco destinos + Web/Server + engrenagem.
    fn rail(&mut self, ctx: &egui::Context) {
        let p = pal();
        let swarm_workers = self.swarm_running();
        let any_busy = self.busy || self.open_tabs.iter().any(|t| t.busy);
        egui::SidePanel::left("rail")
            .exact_width(68.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_rail)
                    .inner_margin(egui::Margin::symmetric(6, 14)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.vertical_centered(|ui| {
                    // marca
                    let (logo, _) =
                        ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
                    crate::icon::paint_mark(ui.painter(), logo);
                    ui.add_space(14.0);

                    if w::rail_item(
                        ui,
                        crate::ui::Glyph::Pulse,
                        "USAGE",
                        self.show_usage,
                        self.meter.active(),
                    )
                    .on_hover_text("Usage panel — pin it to keep it open")
                    .clicked()
                    {
                        self.show_usage = !self.show_usage;
                    }
                    for d in [
                        Dest::Chat,
                        Dest::Files,
                        Dest::Graph,
                        Dest::Memory,
                        Dest::Swarm,
                        Dest::Diag,
                    ] {
                        let (glyph, label) = d.rail();
                        // ponto = tem trabalho pendente aqui
                        let dot = match d {
                            Dest::Chat => any_busy,
                            Dest::Swarm => swarm_workers > 0,
                            Dest::Graph => {
                                self.graph_stats.files == 0 || self.graph_stats.stale_files > 0
                            }
                            _ => false,
                        };
                        if w::rail_item(ui, glyph, label, self.dest == d, dot).clicked() {
                            self.dest = d;
                        }
                    }
                });

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    let gear = ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("⚙").size(14.0).color(p.muted),
                            )
                            .fill(p.card)
                            .stroke(egui::Stroke::new(1.0, p.border))
                            .corner_radius(egui::CornerRadius::same(8))
                            .min_size(egui::vec2(30.0, 30.0)),
                        )
                        .on_hover_text("Settings");
                    if gear.clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    ui.add_space(8.0);
                    let running = webserver::status().running;
                    let (glyph, _) = Dest::WebServer.rail();
                    if w::rail_item(
                        ui,
                        glyph,
                        "WEB",
                        self.dest == Dest::WebServer,
                        running,
                    )
                    .on_hover_text("Web + Server")
                    .clicked()
                    {
                        self.dest = Dest::WebServer;
                    }
                });
            });
    }

    /// Lista única de chats (substitui a barra de abas de sessão).
    /// Lista única de chats (substitui a barra de abas de sessão).
    fn sessions_panel(&mut self, ctx: &egui::Context) {
        let p = pal();
        egui::SidePanel::left("sessions")
            .default_width(268.0)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_side)
                    .inner_margin(egui::Margin::symmetric(10, 14)),
            )
            .show(ctx, |ui| {
                let running = self.daemon_live.iter().filter(|s| s.busy).count();
                ui.horizontal(|ui| {
                    ui.label(crate::theme::ui_medium("Chats", 12.0).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("↻").size(12.0).color(p.muted),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Refresh")
                            .clicked()
                        {
                            self.refresh_daemon_sessions();
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("🗑").size(12.0).color(p.muted),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Delete ALL chats (asks first)")
                            .clicked()
                        {
                            self.confirm_reset = true;
                        }
                        ui.label(crate::theme::meta(format!("{running} running")));
                    });
                });
                ui.add_space(6.0);

                let search = ui.add(
                    egui::Button::new(
                        egui::RichText::new("⌘K   search chats, files, actions…")
                            .size(12.0)
                            .color(p.muted),
                    )
                    .fill(p.card)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .corner_radius(egui::CornerRadius::same(9))
                    .min_size(egui::vec2(ui.available_width(), 32.0)),
                );
                if search.clicked() {
                    self.cmdk = true;
                    self.cmdk_query.clear();
                    self.cmdk_sel = 0;
                }
                ui.add_space(6.0);
                let new_chat = ui.add(
                    egui::Button::new(crate::theme::ui_medium("New chat", 12.5).color(p.bg))
                        .fill(p.text)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(egui::CornerRadius::same(9))
                        .min_size(egui::vec2(ui.available_width(), 33.0)),
                );
                if new_chat.on_hover_text("⌘N").clicked() {
                    self.new_chat();
                }
                ui.add_space(6.0);

                // ações coletadas durante o desenho, aplicadas depois
                let mut act: Option<(String, RowAction)> = None;
                let mut commit_rename = false;

                egui::ScrollArea::vertical()
                    .id_salt("all_sessions")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;

                        // Monta a lista unificada: vivas primeiro (têm estado),
                        // depois salvas que não estão vivas.
                        let live_ids: std::collections::HashSet<&str> =
                            self.daemon_live.iter().map(|s| s.id.as_str()).collect();
                        let mut rows: Vec<Row> = Vec::new();
                        for s in &self.daemon_live {
                            // chat recém-criado sem mensagens não vira linha da
                            // lista — só aparece depois da primeira pergunta
                            if s.history_msgs == 0 && !s.busy {
                                continue;
                            }
                            rows.push(Row {
                                id: s.id.clone(),
                                title: summary_title(s),
                                subtitle: if s.busy {
                                    format!(
                                        "{} · {} msgs",
                                        if s.folder.is_empty() {
                                            s.short_id.clone()
                                        } else {
                                            s.folder.clone()
                                        },
                                        s.history_msgs
                                    )
                                } else if s.folder.is_empty() {
                                    s.short_id.clone()
                                } else {
                                    s.folder.clone()
                                },
                                hover: s.chat_dir.clone(),
                                updated_at: s.updated_at.clone(),
                                busy: s.busy,
                                pinned: s.pinned,
                                live: true,
                            });
                        }
                        for m in &self.session_list {
                            if live_ids.contains(m.id.as_str()) {
                                continue;
                            }
                            rows.push(Row {
                                id: m.id.clone(),
                                title: meta_title(m),
                                subtitle: m.chat_folder_name.clone(),
                                hover: m.chat_dir.clone(),
                                updated_at: m.updated_at.clone(),
                                busy: false,
                                pinned: m.pinned,
                                live: false,
                            });
                        }

                        let today = today_key();
                        let groups: [(&str, Vec<&Row>); 4] = [
                            (
                                "pinned",
                                rows.iter().filter(|r| r.pinned).collect(),
                            ),
                            (
                                "running",
                                rows.iter().filter(|r| !r.pinned && r.busy).collect(),
                            ),
                            (
                                "today",
                                rows.iter()
                                    .filter(|r| {
                                        !r.pinned && !r.busy && day_key(&r.updated_at) == today
                                    })
                                    .collect(),
                            ),
                            (
                                "earlier",
                                rows.iter()
                                    .filter(|r| {
                                        !r.pinned && !r.busy && day_key(&r.updated_at) != today
                                    })
                                    .collect(),
                            ),
                        ];

                        for (label, items) in groups {
                            if items.is_empty() {
                                continue;
                            }
                            ui.add_space(6.0);
                            ui.label(micro(label));
                            for r in items {
                                let selected = r.id == self.session.meta.daemon_session_id
                                    || r.id == self.session.meta.id;

                                // renomeando esta linha: vira campo de texto
                                if self.rename_id.as_deref() == Some(r.id.as_str()) {
                                    let edit = ui.add(
                                        egui::TextEdit::singleline(&mut self.rename_buf)
                                            .desired_width(ui.available_width() - 4.0)
                                            .hint_text("chat name"),
                                    );
                                    edit.request_focus();
                                    let enter =
                                        ui.input(|i| i.key_pressed(egui::Key::Enter));
                                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                    if enter {
                                        commit_rename = true;
                                    } else if esc || edit.lost_focus() {
                                        act = Some((r.id.clone(), RowAction::CancelRename));
                                    }
                                    continue;
                                }

                                let resp = session_row(
                                    ui,
                                    ("row", &r.id),
                                    &r.title,
                                    &r.subtitle,
                                    r.busy,
                                    selected,
                                    r.pinned,
                                );
                                let resp = resp.on_hover_text(&r.hover);
                                if resp.clicked() {
                                    act = Some((r.id.clone(), RowAction::Open(r.live)));
                                }
                                resp.context_menu(|ui| {
                                    if ui.button("Rename").clicked() {
                                        act = Some((
                                            r.id.clone(),
                                            RowAction::StartRename(r.title.clone()),
                                        ));
                                        ui.close_menu();
                                    }
                                    if ui
                                        .button(if r.pinned {
                                            "Unpin"
                                        } else {
                                            "Pin to top"
                                        })
                                        .clicked()
                                    {
                                        act = Some((r.id.clone(), RowAction::Pin(!r.pinned)));
                                        ui.close_menu();
                                    }
                                    if ui.button("Open folder").clicked() {
                                        act = Some((
                                            r.hover.clone(),
                                            RowAction::OpenFolder,
                                        ));
                                        ui.close_menu();
                                    }
                                    if r.live && ui.button("End session").clicked() {
                                        act = Some((r.id.clone(), RowAction::Kill));
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    if ui
                                        .button(
                                            egui::RichText::new("Delete chat…").color(p.error),
                                        )
                                        .clicked()
                                    {
                                        act = Some((r.id.clone(), RowAction::AskDelete));
                                        ui.close_menu();
                                    }
                                });
                            }
                        }
                        ui.add_space(16.0);
                    });

                if commit_rename {
                    if let Some(id) = self.rename_id.take() {
                        let t = self.rename_buf.clone();
                        self.update_session_meta(&id, Some(&t), None, None);
                    }
                    self.rename_buf.clear();
                }
                if let Some((key, action)) = act {
                    match action {
                        RowAction::Open(live) => {
                            self.dest = Dest::Chat;
                            if live {
                                self.switch_to_daemon_session(&key);
                            } else {
                                self.load_session_id(&key);
                            }
                        }
                        RowAction::StartRename(title) => {
                            self.rename_buf = title;
                            self.rename_id = Some(key);
                        }
                        RowAction::CancelRename => {
                            self.rename_id = None;
                            self.rename_buf.clear();
                        }
                        RowAction::Pin(v) => self.update_session_meta(&key, None, Some(v), None),
                        RowAction::OpenFolder => {
                            let path = PathBuf::from(&key);
                            if path.is_dir() {
                                open_path(&path);
                            }
                        }
                        RowAction::Kill => self.kill_daemon_session(&key, false),
                        RowAction::AskDelete => self.ask_delete(&key),
                    }
                }
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let p = pal();
        egui::TopBottomPanel::bottom("statusbar")
            .exact_height(26.0)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(22, 0)),
            )
            .show(ctx, |ui| {
                w::hairline(ui);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let chat = self.session.chat_path();
                    ui.label(crate::theme::meta(shorten_path(&chat)))
                        .on_hover_text(chat.display().to_string());

                    let live = self.daemon_live.len();
                    let cap = self.cfg.max_sessions;
                    let (dot_c, txt) = if self.daemon.is_some() {
                        (p.ok, format!("● daemon {live}/{cap}"))
                    } else {
                        (p.error, "○ daemon offline".to_string())
                    };
                    ui.label(
                        egui::RichText::new(txt)
                            .monospace()
                            .size(10.5)
                            .color(dot_c),
                    );
                    if self.session.meta.gauntlet {
                        ui.label(
                            egui::RichText::new(format!(
                                "gauntlet {}/{}",
                                self.gauntlet_iter, self.cfg.gauntlet_max_iterations
                            ))
                            .monospace()
                            .size(10.5)
                            .color(p.accent),
                        )
                        .on_hover_text("Auto-continues spent on this goal.");
                    }

                    let workers = self.swarm_running();
                    if workers > 0 {
                        ui.label(crate::theme::meta(format!("swarm {workers} workers")));
                    }

                    // grafo: cobertura + economia acumulada, clicável
                    let g = &self.graph_stats;
                    let (gtxt, gcolor) = if g.files == 0 {
                        ("graph —".to_string(), p.muted)
                    } else if g.stale_files > 0 {
                        (
                            format!("graph {} sym · {} stale", fmt_tokens(g.symbols as i64), g.stale_files),
                            p.accent,
                        )
                    } else {
                        (format!("graph {} sym", fmt_tokens(g.symbols as i64)), p.ok)
                    };
                    let saved = self.metrics.graph_saved_tokens;
                    let gfull = if saved > 0 {
                        format!("{gtxt} · −{} tok", fmt_tokens(saved))
                    } else {
                        gtxt
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(gfull)
                                    .monospace()
                                    .size(10.5)
                                    .color(gcolor),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Structural graph and savings — click to open")
                        .clicked()
                    {
                        self.dest = Dest::Graph;
                    }

                    // token_less: nível + economia medida, clicável
                    let tlc = self.token_less_level();
                    let ctxt = match self.metrics.token_less_delta(tlc.tag()) {
                        Some(d) if tlc.is_on() => {
                            format!("token less {} {:+.0}%", tlc.tag(), d * 100.0)
                        }
                        _ if tlc.is_on() => format!("token less {}", tlc.tag()),
                        _ => "token less off".to_string(),
                    };
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(ctxt)
                                    .monospace()
                                    .size(10.5)
                                    .color(if tlc.is_on() { p.accent } else { p.muted }),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Token Less Cost: response compression and measured savings — click to open")
                        .clicked()
                    {
                        self.dest = Dest::Graph;
                    }
                    if self.pending_approval.is_some() {
                        ui.label(
                            egui::RichText::new("approval pending")
                                .monospace()
                                .size(10.5)
                                .color(p.accent),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(crate::theme::meta("⌘K commands")).frame(false),
                            )
                            .clicked()
                        {
                            self.cmdk = true;
                            self.cmdk_query.clear();
                            self.cmdk_sel = 0;
                        }
                        ui.label(crate::theme::meta(short_mem(&self.mem_line)))
                            .on_hover_text(&self.mem_line);
                    });
                });
            });
    }

    /// Topo do chat: título + pasta + toggle Code/Office + painel contextual.
    fn chat_top_bar(&mut self, ctx: &egui::Context) {
        let p = pal();
        egui::TopBottomPanel::top("chatbar")
            .exact_height(52.0)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(22, 0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let title: String = if self.session.meta.title.is_empty()
                        || self.session.meta.title == "New session"
                    {
                        "New chat".into()
                    } else {
                        one_line(&self.session.meta.title, 46)
                    };
                    ui.label(crate::theme::ui_medium(title, 14.0).color(p.text));
                    if ui
                        .add(
                            egui::Button::new(crate::theme::meta(
                                self.session.meta.chat_folder_name.clone(),
                            ))
                            .frame(false),
                        )
                        .on_hover_text(self.session.chat_path().display().to_string())
                        .clicked()
                    {
                        self.open_chat_folder();
                    }
                    if self.busy {
                        ui.add_space(4.0);
                        ui.spinner();
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Stop").size(12.0).color(p.error),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            self.stop_agent();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let open = self.ctx_panel;
                        let panel_btn = ui.add(
                            egui::Button::new(
                                crate::theme::mono_medium("Panel", 11.5)
                                    .color(if open { p.text } else { p.muted }),
                            )
                            .frame(false),
                        );
                        w::chevron(ui, open, if open { p.text } else { p.muted });
                        if panel_btn
                            .on_hover_text("Preview and side panel (Esc closes)")
                            .clicked()
                        {
                            self.ctx_panel = !self.ctx_panel;
                        }
                        ui.add_space(6.0);
                        let proj = self.session.meta.project_dir.clone();
                        let label = match &proj {
                            Some(d) => format!(
                                "▣ {}",
                                std::path::Path::new(d)
                                    .file_name()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_else(|| d.clone())
                            ),
                            None => "▣ project".to_string(),
                        };
                        let chip = ui.add(
                            egui::Button::new(
                                crate::theme::mono_medium(label, 11.5).color(if proj.is_some() {
                                    p.accent
                                } else {
                                    p.muted
                                }),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::new(
                                1.0,
                                if proj.is_some() { p.accent } else { p.border_soft },
                            ))
                            .corner_radius(egui::CornerRadius::same(7))
                            .min_size(egui::vec2(0.0, 24.0)),
                        );
                        let chip = match &proj {
                            Some(d) => chip.on_hover_text(format!(
                                "{d}\nThis chat reads and edits here. Right-click to clear."
                            )),
                            None => chip.on_hover_text(
                                "Point this chat at a project folder. Without one the agent \
                                 is confined to the chat folder.",
                            ),
                        };
                        if chip.clicked() {
                            self.pick_project_dir();
                        }
                        if chip.secondary_clicked() && proj.is_some() {
                            self.clear_project_dir();
                        }
                        ui.add_space(6.0);
                        let sel = match self.mode {
                            AppMode::Code => 0,
                            AppMode::Office => 1,
                        };
                        if let Some(i) = w::segmented(ui, &["Code", "Office"], sel) {
                            let m = if i == 0 { AppMode::Code } else { AppMode::Office };
                            if m != self.mode {
                                self.set_mode(m);
                            }
                        }
                    });
                });
                w::hairline(ui);
            });
    }

    /// Composer: card 700px, raio 14, chips com borda.
    fn composer(&mut self, ctx: &egui::Context) {
        let p = pal();
        egui::TopBottomPanel::bottom("composer")
            .exact_height(140.0)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(22, 8)),
            )
            .show(ctx, |ui| {
                let full = ui.available_width();
                let card_w = (full - 8.0).clamp(320.0, 700.0);
                let pad = ((full - card_w) * 0.5).max(0.0);

                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.allocate_ui_with_layout(
                        egui::vec2(card_w, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            crate::theme::card_frame().show(ui, |ui| {
                                let inner_w = (card_w - 36.0).max(240.0);
                                ui.set_max_width(inner_w);
                                ui.set_min_width(inner_w);

                                ui.add(
                                    egui::TextEdit::multiline(&mut self.input)
                                        .desired_rows(2)
                                        .desired_width(inner_w)
                                        .frame(false)
                                        .hint_text(match self.mode {
                                            AppMode::Code => "Ask anything, or / for commands",
                                            AppMode::Office => {
                                                "Docs, sheets, PDF — or / for commands"
                                            }
                                        }),
                                );
                                ui.add_space(8.0);

                                ui.horizontal(|ui| {
                                    ui.set_min_height(30.0);
                                    let model = one_line(&self.cfg.model, 18);
                                    if w::chip(ui, &format!("{model} ▾"))
                                        .on_hover_text("Trocar LLM do pool (⌘K)")
                                        .clicked()
                                    {
                                        self.cmdk = true;
                                        self.cmdk_query = "llm ".into();
                                        self.cmdk_sel = 0;
                                    }
                                    let tlc = self.token_less_level();
                                    let chip_txt = if tlc.is_on() {
                                        format!("token less: {}", tlc.tag())
                                    } else {
                                        "token less".to_string()
                                    };
                                    let tlc_chip = w::pill_toggle(ui, &chip_txt, tlc.is_on());
                                    if tlc_chip
                                        .on_hover_text(
                                            "Token Less Cost — resposta comprimida neste chat. \
                                             Clique para alternar off → lite → full → ultra \
                                             (/tokenless).\n\
                                             Encolhe só a saída; código e comandos ficam intactos.",
                                        )
                                        .clicked()
                                    {
                                        self.set_token_less(tlc.next());
                                    }
                                    let g_on = self.session.meta.gauntlet;
                                    if w::pill_toggle(ui, "gauntlet loop", g_on)
                                        .on_hover_text(format!(
                                            "Gauntlet Loop — the agent splits the goal, \
                                             critiques each part and redoes what fails.\n\
                                             Auto-sends \"{}\" until the reply carries {} \
                                             or {} iterations are spent. Turning it off stops it.",
                                            crate::gauntlet::CONTINUE_MESSAGE,
                                            crate::gauntlet::DONE_MARKER,
                                            self.cfg.gauntlet_max_iterations,
                                        ))
                                        .clicked()
                                    {
                                        self.set_gauntlet(!g_on);
                                    }
                                    if w::chip(ui, "+ file")
                                        .on_hover_text("Attach a file path to the message")
                                        .clicked()
                                    {
                                        if let Some(f) = rfd::FileDialog::new().pick_file() {
                                            if !self.input.is_empty()
                                                && !self.input.ends_with(' ')
                                            {
                                                self.input.push(' ');
                                            }
                                            self.input.push_str(&f.display().to_string());
                                        }
                                    }

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if w::primary_button(ui, "Send", !self.busy)
                                                .on_hover_text("Ctrl+Enter · ⌘Enter on Mac")
                                                .clicked()
                                            {
                                                self.send_user_message();
                                            }
                                            ui.add_space(4.0);
                                            ui.label(crate::theme::meta("⌘Enter"));
                                            if self.busy {
                                                ui.add_space(6.0);
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            egui::RichText::new("Stop")
                                                                .size(12.0)
                                                                .color(p.error),
                                                        )
                                                        .frame(false),
                                                    )
                                                    .clicked()
                                                {
                                                    self.stop_agent();
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                        },
                    );
                });
            });
    }

    /// Painel contextual do chat: Preview / Side (não é mais aba do rail).
    fn context_panel(&mut self, ctx: &egui::Context) {
        let p = pal();
        // o clique em Run vira ação aqui fora: dentro do painel não dá para
        // chamar `push_error`, e era assim que a falha sumia
        let mut want_open: Option<String> = None;
        egui::SidePanel::right("ctx")
            .default_width(320.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_side)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let sel = match self.ctx_tab {
                        CtxTab::Preview => 0,
                        CtxTab::Side => 1,
                    };
                    if let Some(i) = w::segmented(ui, &["Preview", "Side"], sel) {
                        self.ctx_tab = if i == 0 { CtxTab::Preview } else { CtxTab::Side };
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(crate::theme::meta("×")).frame(false))
                            .clicked()
                        {
                            self.ctx_panel = false;
                        }
                    });
                });
                ui.add_space(8.0);
                match self.ctx_tab {
                    CtxTab::Preview => match &self.preview {
                        Some(pv) => want_open = render_preview(ui, pv),
                        None => {
                            let htmls = html_artifacts(&self.artifacts);
                            if htmls.is_empty() {
                                ui.label(crate::theme::meta(
                                    "Pick a file under FILES to preview",
                                ));
                            } else {
                                ui.label(crate::theme::meta("Pages in this chat"));
                                ui.add_space(6.0);
                                for h in htmls {
                                    let name = h
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("page")
                                        .to_string();
                                    if ui
                                        .button(
                                            egui::RichText::new(format!("▶ Run  {name}"))
                                                .size(14.0),
                                        )
                                        .on_hover_text(h.display().to_string())
                                        .clicked()
                                    {
                                        self.open_preview(h.clone());
                                    }
                                }
                            }
                        }
                    },
                    CtxTab::Side => self.side_panel_body(ui),
                }
            });
        if let Some(url) = want_open {
            match browser::open_in_app(&url) {
                Ok(()) => {
                    self.status = format!("opened {url} in the WebView window");
                }
                Err(e) => self.push_error(format!(
                    "could not open the WebView window: {e} — the page is served at {url}"
                )),
            }
        }
    }

    fn side_panel_body(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let sp = side_panel::get();
        let head = match sp.kind {
            PanelKind::Empty => "The agent posts files and notes here".to_string(),
            PanelKind::File => format!("📄 {}", sp.title),
            PanelKind::Diff => format!("± {}", sp.title),
            PanelKind::Note => format!("📝 {}", sp.title),
            PanelKind::Plan => "☑ Plan".to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(crate::theme::ui_medium(head, 13.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(crate::theme::meta("clear")).frame(false))
                    .clicked()
                {
                    side_panel::clear();
                }
            });
        });
        if let Some(path) = &sp.path {
            ui.label(crate::theme::meta(path.display().to_string()));
        }
        ui.add_space(6.0);
        w::hairline(ui);
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&sp.body)
                            .monospace()
                            .size(12.0)
                            .color(p.text_dim),
                    )
                    .wrap(),
                );
            });
    }

    // -- corpo central ------------------------------------------------------

    fn chat_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let max_w = 700.0_f32;
        let full = ui.available_width();
        let pad = ((full - max_w) * 0.5).max(0.0);
        let time = ui.input(|i| i.time);

        let mut approve: Option<ApprovalDecision> = None;
        let mut open_file: Option<PathBuf> = None;
        let mut md_action: Option<md::MdAction> = None;

        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(10.0);

                let only_system = self
                    .messages
                    .iter()
                    .all(|m| m.role == "system" || m.role == "error");
                if only_system && self.messages.len() <= 2 {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.18);
                        let (r, _) = ui
                            .allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
                        crate::icon::paint_mark(ui.painter(), r);
                        ui.add_space(12.0);
                        ui.label(crate::theme::ui_medium("All set", 22.0).color(p.text));
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Ask for code, a document, a sheet or a PDF — ⌘K opens everything",
                            )
                            .size(13.5)
                            .color(p.muted),
                        );
                    });
                }

                let n = self.messages.len();
                let mut i = 0usize;
                while i < n {
                    let msg = &self.messages[i];
                    if msg.role == "system"
                        && (msg.text.contains("Chat folder:")
                            || msg.text.contains("Daemon session")
                            || msg.text.starts_with("Welcome"))
                    {
                        i += 1;
                        continue;
                    }

                    // Bloco de tool calls consecutivas → um Frame só
                    if msg.role == "tool" && ToolCall::parse(&msg.text).is_some() {
                        let start = i;
                        let mut end = i;
                        while end < n
                            && self.messages[end].role == "tool"
                            && ToolCall::parse(&self.messages[end].text).is_some()
                        {
                            end += 1;
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(pad + 48.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2((max_w - 48.0).min(full), 0.0),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    let calls: Vec<ToolCall> = self.messages[start..end]
                                        .iter()
                                        .filter_map(|m| ToolCall::parse(&m.text))
                                        .collect();
                                    let needs_ask =
                                        calls.iter().any(|c| c.state == ToolState::NeedsApproval);
                                    let mut frame = crate::theme::tool_frame();
                                    if needs_ask {
                                        frame = frame
                                            .fill(p.card)
                                            .stroke(egui::Stroke::new(1.0, p.accent));
                                    }
                                    frame.show(ui, |ui| {
                                        for (k, call) in calls.iter().enumerate() {
                                            if k > 0 {
                                                w::hairline(ui);
                                            }
                                            if let Some(d) =
                                                tool_row(ui, start + k, call, time)
                                            {
                                                approve = Some(d);
                                            }
                                        }
                                    });
                                },
                            );
                        });
                        ui.add_space(10.0);
                        i = end;
                        continue;
                    }

                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        ui.allocate_ui_with_layout(
                            egui::vec2(max_w.min(full), 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                ui.set_max_width(max_w.min(full));
                                match msg.role.as_str() {
                                    "user" => {
                                        ui.horizontal_top(|ui| {
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(40.0, 0.0),
                                                egui::Layout::top_down(egui::Align::LEFT),
                                                |ui| {
                                                    ui.add_space(5.0);
                                                    ui.label(micro("you"));
                                                },
                                            );
                                            egui::Frame::new()
                                                .fill(p.user_bg)
                                                .corner_radius(egui::CornerRadius {
                                                    nw: 4,
                                                    ne: 12,
                                                    sw: 12,
                                                    se: 12,
                                                })
                                                .inner_margin(egui::Margin::symmetric(14, 11))
                                                .show(ui, |ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(&msg.text)
                                                                .size(14.5)
                                                                .color(p.text),
                                                        )
                                                        .wrap(),
                                                    );
                                                });
                                        });
                                    }
                                    "assistant" => {
                                        ui.horizontal_top(|ui| {
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(40.0, 0.0),
                                                egui::Layout::top_down(egui::Align::LEFT),
                                                |ui| {
                                                    ui.add_space(2.0);
                                                    let (r, _) = ui.allocate_exact_size(
                                                        egui::vec2(20.0, 20.0),
                                                        egui::Sense::hover(),
                                                    );
                                                    crate::icon::paint_mark(ui.painter(), r);
                                                },
                                            );
                                            ui.vertical(|ui| {
                                                ui.horizontal(|ui| {
                                                    if ui
                                                        .add(
                                                            egui::Button::new(
                                                                egui::RichText::new("copy")
                                                                    .monospace()
                                                                    .size(9.5)
                                                                    .color(p.muted),
                                                            )
                                                            .frame(false),
                                                        )
                                                        .on_hover_text(
                                                            "Copy this answer (select any text with the mouse to copy a part)",
                                                        )
                                                        .clicked()
                                                    {
                                                        ui.ctx().copy_text(msg.text.clone());
                                                        self.status = "answer copied".into();
                                                    }
                                                });
                                                if md_action.is_none() {
                                                    md_action = md::render_markdown(ui, &msg.text);
                                                } else {
                                                    md::render_markdown(ui, &msg.text);
                                                }
                                            });
                                        });
                                    }
                                    "tool" => {
                                        // sessões antigas: linha mono simples
                                        ui.label(
                                            egui::RichText::new(one_line(&msg.text, 160))
                                                .monospace()
                                                .size(12.0)
                                                .color(p.muted),
                                        );
                                    }
                                    "error" => {
                                        ui.label(
                                            egui::RichText::new(&msg.text)
                                                .size(13.0)
                                                .color(p.error),
                                        );
                                    }
                                    _ => {
                                        ui.label(
                                            egui::RichText::new(&msg.text)
                                                .size(12.5)
                                                .color(p.muted),
                                        );
                                    }
                                }
                            },
                        );
                    });
                    ui.add_space(12.0);
                    i += 1;
                }
                ui.add_space(24.0);
            });

        if let Some(d) = approve {
            self.decide_approval(d);
        }
        if let Some(path) = open_file.take() {
            self.open_preview(path);
        }
        if let Some(action) = md_action {
            match action {
                md::MdAction::CopyText(text) => {
                    ui.ctx().copy_text(text.clone());
                    self.status = format!("copied ({})", text.chars().take(60).collect::<String>());
                }
                md::MdAction::RunCommand(cmd) => {
                    self.run_chat_command(cmd);
                }
            }
        }
    }

    /// Roda um comando do bloco ```sh/bash no diretório do chat e mostra a
    /// saída como mensagem no próprio chat.
    fn run_chat_command(&mut self, cmd: String) {
        if self.busy {
            self.push_error("agent is running — stop it before running a command".into());
            return;
        }
        let cwd = self.session.chat_path();
        let label = cmd.chars().take(90).collect::<String>();
        self.messages.push(UiMessage {
            role: "tool".into(),
            text: format!("▶ {}", label),
        });
        let output = std::process::Command::new("sh")
            .arg("-lc")
            .arg(&cmd)
            .current_dir(&cwd)
            .output();
        let (ok, out) = match output {
            Ok(o) => {
                let mut s = String::new();
                if !o.stdout.is_empty() {
                    s.push_str(&String::from_utf8_lossy(&o.stdout));
                }
                if !o.stderr.is_empty() {
                    if !s.is_empty() {
                        s.push('\n');
                    }
                    s.push_str(&String::from_utf8_lossy(&o.stderr));
                }
                (o.status.success(), s)
            }
            Err(e) => {
                self.push_error(format!("run: {e}"));
                return;
            }
        };
        let trimmed = out.trim();
        if trimmed.is_empty() {
            self.messages.push(UiMessage {
                role: "system".into(),
                text: format!(
                    "{} command finished {} — no output",
                    if ok { "✓" } else { "✗" },
                    if ok { "clean" } else { "with error" }
                ),
            });
        } else {
            let cap = trimmed.chars().take(24_000).collect::<String>();
            let extra = if trimmed.chars().count() > 24_000 {
                format!("\n…[{} chars truncated]", trimmed.chars().count() - 24_000)
            } else {
                String::new()
            };
            self.messages.push(UiMessage {
                role: if ok { "system".into() } else { "error".into() },
                text: format!("```\n{cap}{extra}\n```"),
            });
        }
        self.status = format!("command {} — {}", if ok { "ok" } else { "failed" }, label);
    }

    fn files_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.horizontal(|ui| {
            ui.label(crate::theme::ui_medium("Chat files", 14.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w::chip(ui, "refresh").clicked() {
                    self.artifacts = scan_artifacts(&self.session.chat_path(), self.mode);
                }
                if w::chip(ui, "open folder").clicked() {
                    self.open_chat_folder();
                }
            });
        });
        ui.label(crate::theme::meta(
            self.session.chat_path().display().to_string(),
        ));
        ui.add_space(8.0);

        let mut preview_path: Option<PathBuf> = None;
        let mut external: Option<PathBuf> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.artifacts.is_empty() {
                    ui.label(crate::theme::meta("nothing generated in this chat yet"));
                }
                for path in &self.artifacts {
                    let name = path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(&name)
                                        .monospace()
                                        .size(12.5)
                                        .color(p.text_dim),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            preview_path = Some(path.clone());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(egui::Button::new(crate::theme::meta("↗")).frame(false))
                                .on_hover_text("Open outside harness")
                                .clicked()
                            {
                                external = Some(path.clone());
                            }
                        });
                    });
                }
            });
        if let Some(path) = preview_path {
            self.open_preview(path);
            self.dest = Dest::Chat;
        }
        if let Some(path) = external {
            open_path(&path);
        }
    }

    /// Grafo estrutural + economia medida (grafo e token_less).
    /// Painel lateral de uso: contexto, KPIs, gráfico ao vivo e origem do gasto.
    /// Fixo (`usage_pinned`) fica aberto em todos os destinos e volta no boot.
    fn usage_panel(&mut self, ctx: &egui::Context) {
        if !self.show_usage {
            return;
        }
        let p = pal();
        let m = self.metrics.clone();
        let pinned = self.cfg.usage_pinned;
        let mut toggle_pin = false;
        let mut close = false;

        egui::SidePanel::right("usage")
            .default_width(300.0)
            .min_width(260.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(p.bg_side)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(crate::theme::ui_medium("Usage", 13.0).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(crate::theme::meta("×")).frame(false))
                            .on_hover_text("Hide (Esc)")
                            .clicked()
                        {
                            close = true;
                        }
                        let pin = ui.add(
                            egui::Button::new(
                                crate::theme::mono_medium(
                                    if pinned { "pinned" } else { "pin" },
                                    10.5,
                                )
                                .color(if pinned { p.accent } else { p.muted }),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::new(
                                1.0,
                                if pinned { p.accent } else { p.border_soft },
                            ))
                            .corner_radius(egui::CornerRadius::same(7))
                            .min_size(egui::vec2(0.0, 22.0)),
                        );
                        if pin
                            .on_hover_text(
                                "Pinned: stays open everywhere and reopens on start",
                            )
                            .clicked()
                        {
                            toggle_pin = true;
                        }
                    });
                });
                ui.add_space(8.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("usage_body")
                    .show(ui, |ui| {
                        self.usage_context_card(ui);
                        ui.add_space(10.0);
                        self.usage_live_card(ui);
                        ui.add_space(10.0);
                        self.usage_kpis(ui, &m);
                        ui.add_space(10.0);
                        self.usage_by_source(ui, &m);
                        ui.add_space(16.0);
                    });
            });

        if toggle_pin {
            self.cfg.usage_pinned = !self.cfg.usage_pinned;
            let _ = self.cfg.save();
        }
        if close {
            self.show_usage = false;
            if self.cfg.usage_pinned {
                self.cfg.usage_pinned = false;
                let _ = self.cfg.save();
            }
        }
    }

    /// Contexto da conversa aberta contra a regra real de compactação do
    /// harness, que é por número de mensagens (`history_cap`), não por token.
    fn usage_context_card(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let msgs = self.llm_history.len();
        let cap = self.cfg.history_cap.max(1);
        let frac = (msgs as f32 / cap as f32).clamp(0.0, 1.0);
        let est_tokens =
            crate::mem_stats::estimate_history_bytes(&self.llm_history) as u64 / 4;
        let healthy = msgs < cap;

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::theme::ui_medium("Context", 12.5).color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        crate::theme::mono_medium(
                            format!("~{} tok", fmt_tokens(est_tokens as i64)),
                            13.0,
                        )
                        .color(p.text),
                    );
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                w::dot(ui, if healthy { p.ok } else { p.accent }, 3.0);
                ui.label(
                    egui::RichText::new(if healthy {
                        "healthy"
                    } else {
                        "compacting"
                    })
                    .monospace()
                    .size(10.5)
                    .color(if healthy { p.ok } else { p.accent }),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(crate::theme::meta(format!("{msgs}/{cap} msgs")));
                });
            });
            ui.add_space(4.0);
            w::split_bar(ui, frac, if healthy { p.ok } else { p.accent }, p.border_soft);
            ui.add_space(4.0);
            ui.label(crate::theme::meta(
                "harness compacts by message count, not tokens",
            ));
        });
    }

    /// Dois gráficos que correm, um por sentido — escalas independentes.
    fn usage_live_card(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let (sent, received) = self.meter.series();
        let (in_now, out_now) = self.meter.last();
        let live = self.meter.active();
        let m = self.metrics.clone();

        // Enviado = tokens de prompt; só é conhecido quando a chamada fecha,
        // então a curva sobe em degrau.
        self.usage_stream_card(
            ui,
            "Sent",
            &sent,
            p.ok,
            in_now,
            m.prompt_tokens,
            live,
            "prompt tokens, counted when each call closes",
        );
        ui.add_space(10.0);
        // Recebido = streaming, chega token a token.
        self.usage_stream_card(
            ui,
            "Received",
            &received,
            p.accent,
            out_now,
            m.completion_tokens,
            live,
            "completion tokens, sampled live from the stream",
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn usage_stream_card(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        series: &[f32],
        color: egui::Color32,
        rate: f32,
        total: u64,
        live: bool,
        note: &str,
    ) {
        let p = pal();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                w::dot(ui, color, 3.0);
                ui.label(crate::theme::ui_medium(title, 12.5).color(p.text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        crate::theme::mono_medium(format!("{:.0}/s", rate), 12.5)
                            .color(if live && rate > 0.0 { color } else { p.muted }),
                    );
                });
            });
            ui.add_space(6.0);
            w::spark_chart(ui, 54.0, series, color);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(crate::theme::meta(format!(
                    "{} total",
                    fmt_tokens(total as i64)
                )));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(crate::theme::meta("24 s"));
                });
            });
            ui.label(crate::theme::meta(note));
        });
    }

    fn usage_kpis(&mut self, ui: &mut egui::Ui, m: &crate::metrics::Metrics) {
        let p = pal();
        let secs = self.started_at.elapsed().as_secs();
        let runtime = if secs >= 3600 {
            format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{}m {:02}s", secs / 60, secs % 60)
        };
        let hit = m.hit_rate();
        let cost = if m.cost_usd > 0.0 {
            format!("${:.3}", m.cost_usd)
        } else {
            "—".to_string()
        };

        card(ui, |ui| {
            ui.label(crate::theme::ui_medium("Session", 12.5).color(p.text));
            ui.add_space(8.0);
            let cells: [(String, &str, egui::Color32); 6] = [
                (
                    if m.prompt_tokens > 0 {
                        format!("{:.1}%", hit * 100.0)
                    } else {
                        "—".into()
                    },
                    "cache hit",
                    if hit > 0.5 { p.ok } else { p.text },
                ),
                (cost, "cost", p.text),
                (runtime, "runtime", p.text),
                (m.calls.to_string(), "requests", p.text),
                (fmt_tokens(m.prompt_tokens as i64), "in", p.text),
                (fmt_tokens(m.completion_tokens as i64), "out", p.text),
            ];
            // `columns` em vez de horizontal + allocate: com altura 0 as células
            // escorregavam na diagonal em vez de ficarem lado a lado.
            for row in cells.chunks(2) {
                ui.columns(2, |cols| {
                    for (i, (val, label, color)) in row.iter().enumerate() {
                        cols[i].label(crate::theme::ui_medium(val.clone(), 15.0).color(*color));
                        cols[i].label(micro(label));
                    }
                });
                ui.add_space(8.0);
            }
            w::hairline(ui);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(micro("total tokens"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        crate::theme::ui_medium(fmt_tokens(m.total_tokens() as i64), 15.0)
                            .color(p.text),
                    );
                });
            });
            ui.add_space(4.0);
            ui.label(crate::theme::meta(if m.cost_usd > 0.0 {
                "cost from the pool price per 1M tokens"
            } else {
                "cost needs price_in/price_out set on the endpoint"
            }));
        });
    }

    fn usage_by_source(&mut self, ui: &mut egui::Ui, m: &crate::metrics::Metrics) {
        let p = pal();
        card(ui, |ui| {
            ui.label(crate::theme::ui_medium("By source", 12.5).color(p.text));
            ui.add_space(6.0);
            if m.by_source.is_empty() {
                ui.label(crate::theme::meta("no requests yet"));
                return;
            }
            let total: u64 = m.by_source.iter().map(|s| s.total()).sum::<u64>().max(1);
            for src in &m.by_source {
                let share = src.total() as f32 / total as f32;
                ui.horizontal(|ui| {
                    w::dot(ui, if src.name == "main" { p.accent } else { p.ok }, 3.0);
                    ui.label(crate::theme::mono_medium(&src.name, 11.5).color(p.text_dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(crate::theme::meta(format!("{} req", src.calls)));
                        ui.label(crate::theme::meta(format!("{:.0}%", share * 100.0)));
                    });
                });
                w::split_bar(
                    ui,
                    share,
                    if src.name == "main" { p.accent } else { p.ok },
                    p.border_soft,
                );
                ui.label(crate::theme::meta(format!(
                    "in {} · out {} · hit {:.0}%",
                    fmt_tokens(src.prompt_tokens as i64),
                    fmt_tokens(src.completion_tokens as i64),
                    src.hit_rate() * 100.0
                )));
                ui.add_space(6.0);
            }
        });
    }

    fn graph_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let g = self.graph_stats.clone();
        let m = self.metrics.clone();
        let built = g.files > 0;

        ui.horizontal(|ui| {
            ui.label(crate::theme::ui_medium("Workspace graph", 14.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w::chip(ui, if built { "reindex" } else { "index" }).clicked() {
                    self.run_graph_build(false);
                }
                if built && w::chip(ui, "all").on_hover_text("reindex from scratch").clicked() {
                    self.run_graph_build(true);
                }
            });
        });
        ui.label(crate::theme::meta(
            "structure extracted without an LLM: indexing costs zero tokens",
        ));
        ui.add_space(10.0);

        // --- estado do trabalho ---
        ui.label(micro("coverage"));
        ui.add_space(4.0);
        if !built {
            ui.label(crate::theme::meta(
                "not indexed — click index, or ask the agent: graph_build",
            ));
        } else {
            egui::Frame::new()
                .fill(p.card)
                .stroke(egui::Stroke::new(1.0, p.border_soft))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width() - 4.0);
                    ui.horizontal(|ui| {
                        for (n, label) in [
                            (g.files.to_string(), "files"),
                            (g.symbols.to_string(), "symbols"),
                            (g.edges.to_string(), "refs"),
                            (g.clusters.to_string(), "clusters"),
                            (format!("{} KB", g.indexed_bytes / 1024), "indexed"),
                        ] {
                            ui.vertical(|ui| {
                                ui.label(crate::theme::ui_medium(n, 16.0).color(p.text));
                                ui.label(micro(label));
                            });
                            ui.add_space(18.0);
                        }
                    });
                });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let stale = g.stale_files;
                if stale > 0 {
                    w::dot(ui, p.accent, 3.0);
                    ui.label(
                        egui::RichText::new(format!("{stale} file(s) changed since the build"))
                            .monospace()
                            .size(10.5)
                            .color(p.accent),
                    );
                } else {
                    w::dot(ui, p.ok, 3.0);
                    ui.label(crate::theme::meta("up to date"));
                }
                if w::chip(ui, "check").clicked() {
                    self.check_graph_stale();
                }
                ui.label(crate::theme::meta(if g.built_at.is_empty() {
                    "never".to_string()
                } else {
                    format!("build {}", &g.built_at[..19.min(g.built_at.len())])
                }));
            });
        }

        // --- economia do grafo ---
        ui.add_space(14.0);
        ui.label(micro("graph savings"));
        ui.add_space(4.0);
        if m.graph_queries == 0 {
            ui.label(crate::theme::meta(
                "no queries yet — savings show up once the agent uses graph_query",
            ));
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    crate::theme::ui_medium(fmt_tokens(m.graph_saved_tokens), 18.0)
                        .color(p.ok),
                );
                ui.label(crate::theme::meta(format!(
                    "read tokens avoided across {} query(ies)",
                    m.graph_queries
                )));
            });
            ui.label(crate::theme::meta(
                "estimate: what read_file would have returned (capped at tool_result_cap) minus the graph answer",
            ));
        }

        // --- consulta manual ---
        ui.add_space(14.0);
        ui.label(micro("query"));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.graph_query)
                    .desired_width(320.0)
                    .hint_text("symbol, file or path…"),
            );
            let go = w::chip(ui, "search").clicked()
                || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if go && !self.graph_query.trim().is_empty() {
                self.run_graph_query();
            }
            if w::chip(ui, "impact")
                .on_hover_text("What breaks if this symbol changes")
                .clicked()
                && !self.graph_query.trim().is_empty()
            {
                self.run_graph_impact();
            }
        });
        if !self.graph_answer.is_empty() {
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .id_salt("graph_answer")
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&self.graph_answer)
                                .monospace()
                                .size(12.0)
                                .color(p.text_dim),
                        )
                        .wrap(),
                    );
                });
        }

        // --- economia do token_less ---
        ui.add_space(16.0);
        w::hairline(ui);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(crate::theme::ui_medium("Token Less Cost", 14.0).color(p.text));
            let cur = self.token_less_level();
            ui.label(
                egui::RichText::new(cur.tag())
                    .monospace()
                    .size(11.0)
                    .color(if cur.is_on() { p.accent } else { p.muted }),
            );
            ui.label(crate::theme::meta("in this chat"));
        });
        ui.add_space(6.0);
        if m.token_less.is_empty() {
            ui.label(crate::theme::meta("no measured replies yet"));
        } else {
            for l in &m.token_less {
                ui.horizontal(|ui| {
                    ui.label(
                        crate::theme::mono_medium(&l.tag, 12.0).color(
                            if l.tag == self.token_less_level().tag() {
                                p.text
                            } else {
                                p.text_dim
                            },
                        ),
                    );
                    ui.label(crate::theme::meta(format!(
                        "{:.0} tokens/reply · {} replies",
                        l.avg(),
                        l.replies
                    )));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match m.token_less_delta(&l.tag) {
                            Some(d) if l.tag != "off" => ui.label(
                                egui::RichText::new(format!("{:+.0}%", d * 100.0))
                                    .monospace()
                                    .size(11.0)
                                    .color(if d < 0.0 { p.ok } else { p.error }),
                            ),
                            _ => ui.label(crate::theme::meta("—")),
                        };
                    });
                });
            }
            ui.add_space(4.0);
            ui.label(crate::theme::meta(
                "observed against the measured average at off (min. 3 replies on each side)",
            ));
        }
        ui.label(crate::theme::meta(
            "Token Less Cost shrinks output only; the graph shrinks input",
        ));
        ui.add_space(20.0);
    }

    fn run_graph_build(&mut self, full: bool) {
        let root = self.project_root();
        let t0 = std::time::Instant::now();
        match crate::graph::build(&root, !full) {
            Ok(st) => {
                self.status = format!(
                    "graph: {} files, {} symbols in {} ms",
                    st.files,
                    st.symbols,
                    t0.elapsed().as_millis()
                );
                self.graph_stats = st;
            }
            Err(e) => self.push_error(format!("graph: {e}")),
        }
    }

    fn check_graph_stale(&mut self) {
        let root = self.project_root();
        if let Ok(st) = crate::graph::stats(&root, true) {
            self.graph_stats = st;
        }
    }

    /// Raio de impacto do símbolo digitado na caixa de busca.
    fn run_graph_impact(&mut self) {
        let root = self.project_root();
        let q = self.graph_query.trim().to_string();
        match crate::graph::impact(&root, &q, 2) {
            Ok(res) => self.graph_answer = res.render(),
            Err(e) => self.graph_answer = format!("error: {e}"),
        }
    }

    fn run_graph_query(&mut self) {
        let root = self.project_root();
        let q = self.graph_query.clone();
        match crate::graph::query(&root, &q, 12, self.cfg.tool_result_cap as u64) {
            Ok(res) => self.graph_answer = res.render(),
            Err(e) => self.graph_answer = format!("error: {e}"),
        }
    }

    fn memory_view_ui(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.label(crate::theme::ui_medium("Memory", 14.0).color(p.text));
        ui.label(crate::theme::meta("local store (SQLite) · lexical match, not semantic"));
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.memory_input)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("Fact to remember…"),
        );
        ui.horizontal(|ui| {
            if w::chip(ui, "store").clicked() {
                let t = self.memory_input.trim().to_string();
                if !t.is_empty() {
                    match memory::with_store(|s| s.store(&t, "ui")) {
                        Ok(id) => {
                            self.memory_input.clear();
                            self.status = format!("memory #{id}");
                            self.refresh_memory_list();
                        }
                        Err(e) => self.push_error(format!("memory: {e}")),
                    }
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.memory_query)
                    .desired_width(180.0)
                    .hint_text("search…"),
            );
            if w::chip(ui, "search").clicked() {
                let q = self.memory_query.clone();
                self.memory_view = memory::with_store(|s| {
                    let hits = s.search(&q, 12)?;
                    Ok(memory::format_hits(&hits))
                })
                .unwrap_or_else(|e| e.to_string());
            }
            if w::chip(ui, "recent").clicked() {
                self.refresh_memory_list();
            }
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&self.memory_view)
                            .monospace()
                            .size(12.0)
                            .color(p.text_dim),
                    )
                    .wrap(),
                );
            });
    }

    fn swarm_view(&mut self, ui: &mut egui::Ui) {
        use crate::swarm::AgentState;
        let p = pal();
        let running = self.swarm_running();
        let mut stop: Option<String> = None;

        ui.horizontal(|ui| {
            ui.label(crate::theme::ui_medium("Swarm", 14.0).color(p.text));
            ui.label(crate::theme::meta(format!(
                "{running} running · max {}",
                self.cfg.swarm_max_workers
            )));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w::chip(ui, "refresh").clicked() {
                    self.last_swarm_refresh = std::time::Instant::now()
                        - std::time::Duration::from_secs(10);
                }
                if w::chip(ui, "stop all").clicked() {
                    stop = Some("all".into());
                }
            });
        });
        ui.label(crate::theme::meta(if self.daemon.is_some() {
            "workers run in the daemon — this panel reads the state from there"
        } else {
            "daemon offline"
        }));
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let snap = self.swarm_snap.clone();

                if snap.agents.is_empty() {
                    ui.label(crate::theme::meta(
                        "no workers — the agent spawns them with swarm_spawn (Code mode)",
                    ));
                } else {
                    ui.label(micro("workers"));
                    ui.add_space(4.0);
                }
                for a in &snap.agents {
                    let (color, label) = match a.state {
                        AgentState::Running => (p.accent, "running"),
                        AgentState::Done => (p.ok, "done"),
                        AgentState::Error => (p.error, "error"),
                        AgentState::Stopped => (p.muted, "stopped"),
                        AgentState::Idle => (p.muted, "idle"),
                    };
                    egui::Frame::new()
                        .fill(p.card)
                        .stroke(egui::Stroke::new(1.0, p.border_soft))
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 4.0);
                            ui.horizontal(|ui| {
                                w::dot(ui, color, 3.0);
                                ui.label(crate::theme::mono_medium(&a.name, 12.0).color(p.text));
                                ui.label(
                                    egui::RichText::new(label)
                                        .monospace()
                                        .size(10.5)
                                        .color(color),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if a.state == AgentState::Running
                                            && w::chip(ui, "stop").clicked()
                                        {
                                            stop = Some(a.id.clone());
                                        }
                                        ui.label(crate::theme::meta(
                                            crate::protocol::short_id(&a.id),
                                        ));
                                    },
                                );
                            });
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&a.task).size(12.5).color(p.text_dim),
                                )
                                .wrap(),
                            );
                            if !a.last_message.is_empty() {
                                ui.add(
                                    egui::Label::new(crate::theme::meta(one_line(
                                        &a.last_message,
                                        160,
                                    )))
                                    .wrap(),
                                );
                            }
                        });
                    ui.add_space(4.0);
                }

                for (sid, plan) in &snap.plans {
                    ui.add_space(10.0);
                    ui.label(micro(&format!("plan {sid} · v{}", plan.version)));
                    ui.add_space(4.0);
                    for t in &plan.tasks {
                        let color = match t.status.as_str() {
                            "done" => p.ok,
                            "running" => p.accent,
                            "blocked" => p.error,
                            _ => p.muted,
                        };
                        ui.horizontal(|ui| {
                            w::dot(ui, color, 2.5);
                            ui.label(
                                egui::RichText::new(format!("{} {}", t.id, t.title))
                                    .monospace()
                                    .size(12.0)
                                    .color(p.text_dim),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !t.depends_on.is_empty() {
                                        ui.label(crate::theme::meta(format!(
                                            "dep {}",
                                            t.depends_on.join(",")
                                        )));
                                    }
                                    if let Some(who) = &t.assignee {
                                        ui.label(crate::theme::meta(who.clone()));
                                    }
                                },
                            );
                        });
                    }
                }

                if !snap.claims.is_empty() {
                    ui.add_space(10.0);
                    ui.label(micro("claimed files"));
                    ui.add_space(4.0);
                    for (path, owner) in &snap.claims {
                        ui.label(crate::theme::meta(format!("{path} → {owner}")));
                    }
                }

                if !snap.bus.is_empty() {
                    ui.add_space(10.0);
                    ui.label(micro("bus"));
                    ui.add_space(4.0);
                    for m in snap.bus.iter().rev().take(12) {
                        ui.add(
                            egui::Label::new(crate::theme::meta(format!(
                                "{}→{}: {}",
                                m.from,
                                m.to,
                                one_line(&m.body, 120)
                            )))
                            .wrap(),
                        );
                    }
                }
                ui.add_space(20.0);
            });

        if let Some(id) = stop {
            if let Some(client) = &self.daemon {
                let _ = client.swarm_stop(&id);
            }
            self.last_swarm_refresh =
                std::time::Instant::now() - std::time::Duration::from_secs(10);
        }
    }

    fn diag_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.horizontal(|ui| {
            ui.label(crate::theme::ui_medium("Diagnostics", 14.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w::chip(ui, "run").clicked() {
                    self.run_diagnostics();
                }
            });
        });
        ui.label(crate::theme::meta(&self.diagnostics.summary));
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for d in &self.diagnostics.items {
                    let color = match d.severity.as_str() {
                        "error" => p.error,
                        "warning" => egui::Color32::from_rgb(180, 120, 40),
                        _ => p.muted,
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{}:{}:{} [{}] {}",
                            d.path, d.line, d.col, d.source, d.message
                        ))
                        .monospace()
                        .size(12.0)
                        .color(color),
                    );
                }
            });
    }

    /// Web + Server no mesmo destino (era duas abas).
    fn webserver_view(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let st = webserver::status();
        ui.label(crate::theme::ui_medium("Web and server", 14.0).color(p.text));
        ui.label(crate::theme::meta(
            "web apps open in the harness WebView, not Safari/Chrome",
        ));
        ui.add_space(10.0);

        ui.label(micro("static server"));
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("path"));
            ui.add(egui::TextEdit::singleline(&mut self.server_path).desired_width(140.0));
            ui.label(crate::theme::meta("port"));
            ui.add(egui::TextEdit::singleline(&mut self.server_port).desired_width(60.0));
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!st.running, egui::Button::new("Start"))
                .clicked()
            {
                let port = self.server_port.parse().unwrap_or(self.cfg.web_server_port);
                let root = if PathBuf::from(&self.server_path).is_absolute() {
                    PathBuf::from(&self.server_path)
                } else {
                    self.cfg.workspace.join(&self.server_path)
                };
                match webserver::start(root, port) {
                    Ok(s) => {
                        self.browser_url = s.url.clone();
                        self.status = format!("server {}", s.url);
                        if let Err(e) = browser::open_in_app(&s.url) {
                            self.push_error(format!("webview: {e}"));
                        }
                    }
                    Err(e) => self.push_error(format!("server: {e}")),
                }
            }
            if ui
                .add_enabled(st.running, egui::Button::new("Stop"))
                .clicked()
            {
                webserver::stop();
                self.status = "server stopped".into();
            }
            ui.label(
                egui::RichText::new(if st.running {
                    format!("● {}", st.url)
                } else {
                    "○ stopped".into()
                })
                .monospace()
                .size(10.5)
                .color(if st.running { p.ok } else { p.muted }),
            );
        });
        ui.label(crate::theme::meta(format!("root: {}", st.root.display())));
        if !st.last_error.is_empty() {
            ui.label(
                egui::RichText::new(&st.last_error)
                    .size(12.0)
                    .color(p.error),
            );
        }

        ui.add_space(14.0);
        w::hairline(ui);
        ui.add_space(10.0);
        ui.label(micro("webview"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.browser_url)
                    .desired_width(260.0)
                    .hint_text("http://127.0.0.1:8765/"),
            );
            if ui.button("Open in app").clicked() {
                match browser::open_in_app(&self.browser_url) {
                    Ok(()) => self.status = "webview opened".into(),
                    Err(e) => self.push_error(format!("webview: {e}")),
                }
            }
            if w::chip(ui, "fetch text").clicked() {
                match browser::fetch_preview(&self.browser_url, self.cfg.web_markdown) {
                    Ok(s) => {
                        self.browser = s;
                        self.status = format!("HTTP {}", self.browser.status_code);
                    }
                    Err(e) => {
                        self.browser.last_error = e.to_string();
                        self.push_error(format!("fetch: {e}"));
                    }
                }
            }
            if w::chip(ui, "use server URL").clicked() {
                let s = webserver::status();
                if s.running && !s.url.is_empty() {
                    self.browser_url = s.url;
                }
            }
            if w::chip(ui, "open external").clicked() {
                let _ = browser::open_external(&self.browser_url);
            }
        });
        if self.browser.status_code > 0 {
            ui.label(crate::theme::meta(format!(
                "HTTP {} · {}",
                self.browser.status_code, self.browser.title
            )));
        }
        if !self.browser.last_error.is_empty() {
            ui.label(
                egui::RichText::new(&self.browser.last_error)
                    .size(12.0)
                    .color(p.error),
            );
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&self.browser.preview_text)
                            .monospace()
                            .size(12.0)
                            .color(p.text_dim),
                    )
                    .wrap(),
                );
            });
    }

    // -- ⌘K ------------------------------------------------------------------

    fn command_entries(&self) -> Vec<CmdEntry> {
        let mut v: Vec<CmdEntry> = Vec::new();
        let mut push = |label: String, hint: String, action: CmdAction| {
            v.push(CmdEntry {
                label,
                hint,
                action,
            })
        };

        push("New chat".into(), "⌘N".into(), CmdAction::NewChat);
        push(
            format!("Theme: {}", self.cfg.theme.toggled().label()),
            "⇧⌘D".into(),
            CmdAction::ToggleTheme,
        );
        push("Settings".into(), String::new(), CmdAction::OpenSettings);
        push(
            if self.session.meta.pinned {
                "Chat: unpin".into()
            } else {
                "Chat: pin to top".into()
            },
            "/fixar".into(),
            CmdAction::PinCurrent,
        );
        push(
            match &self.session.meta.project_dir {
                Some(_) => "Chat: clear project folder".into(),
                None => "Chat: set project folder…".into(),
            },
            self.session
                .meta
                .project_dir
                .clone()
                .unwrap_or_else(|| "/project".into()),
            CmdAction::Project,
        );
        push(
            "Chat: delete…".into(),
            "asks for confirmation".into(),
            CmdAction::DeleteCurrent,
        );
        push(
            "Reset: delete ALL chats…".into(),
            "asks for confirmation".into(),
            CmdAction::ResetAllChats,
        );
        push(
            "Chat: rename…".into(),
            "/rename <title>".into(),
            CmdAction::RenameCurrent,
        );
        push(
            if self.show_usage {
                "Usage panel: hide".into()
            } else {
                "Usage panel: show".into()
            },
            if self.cfg.usage_pinned {
                "pinned".into()
            } else {
                "/usage".into()
            },
            CmdAction::ToggleUsage,
        );
        push(
            if self.cfg.usage_pinned {
                "Usage panel: unpin".into()
            } else {
                "Usage panel: pin".into()
            },
            "keeps it open everywhere".into(),
            CmdAction::PinUsage,
        );
        push(
            "Graph: index workspace".into(),
            if self.graph_stats.files == 0 {
                "not indexed yet".into()
            } else {
                format!("{} files indexed", self.graph_stats.files)
            },
            CmdAction::GraphBuild,
        );
        for l in TokenLessLevel::ALL {
            let atual = self.token_less_level() == l;
            push(
                format!("Token Less Cost: {}", l.tag()),
                if atual {
                    format!("{} · active in this chat", l.label())
                } else {
                    l.label().to_string()
                },
                CmdAction::TokenLess(l),
            );
        }
        if self.open_tabs.len() > 1 {
            push(
                "Close current chat".into(),
                format!("{} open", self.open_tabs.len()),
                CmdAction::CloseTab,
            );
        }
        for (label, d) in [
            ("Go: Chat", Dest::Chat),
            ("Go: Files", Dest::Files),
            ("Go: Graph and savings", Dest::Graph),
            ("Go: Memory", Dest::Memory),
            ("Go: Swarm", Dest::Swarm),
            ("Go: Diagnostics", Dest::Diag),
            ("Go: Web and server", Dest::WebServer),
        ] {
            push(label.into(), String::new(), CmdAction::Go(d));
        }

        // Comandos slash (mesma fonte de verdade do composer)
        for line in slash::help_text().lines() {
            let line = line.trim();
            if !line.starts_with('/') {
                continue;
            }
            let (cmd, desc) = match line.split_once(" — ") {
                Some((a, b)) => (a.trim(), b.trim()),
                None => (line, ""),
            };
            let first = cmd.split(['·', ' ']).next().unwrap_or(cmd).trim();
            push(
                cmd.to_string(),
                desc.to_string(),
                CmdAction::Slash(first.to_string()),
            );
        }
        push(
            "Server: start".into(),
            "web/ · default port".into(),
            CmdAction::Slash("/serve web".into()),
        );
        push(
            "Server: stop".into(),
            String::new(),
            CmdAction::Slash("/stopserve".into()),
        );
        push(
            "Open in WebView".into(),
            self.browser_url.clone(),
            CmdAction::Slash(format!("/web {}", self.browser_url)),
        );

        for ep in self.cfg.llm_pool.iter().filter(|e| e.enabled) {
            push(
                format!("LLM: {}", ep.name),
                ep.model.clone(),
                CmdAction::UseLlm(ep.name.clone()),
            );
        }
        for s in &self.daemon_live {
            push(
                format!("Chat: {}", summary_title(s)),
                if s.busy {
                    "running".into()
                } else {
                    s.folder.clone()
                },
                CmdAction::LiveSession(s.id.clone()),
            );
        }
        let live: std::collections::HashSet<&str> =
            self.daemon_live.iter().map(|s| s.id.as_str()).collect();
        for m in self.session_list.iter().take(60) {
            if live.contains(m.id.as_str()) {
                continue;
            }
            push(
                format!("Chat: {}", meta_title(m)),
                m.chat_folder_name.clone(),
                CmdAction::SavedSession(m.id.clone()),
            );
        }
        for path in &self.artifacts {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            push(
                format!("File: {name}"),
                String::new(),
                CmdAction::OpenFile(path.clone()),
            );
        }
        v
    }

    fn run_command(&mut self, ctx: &egui::Context, action: CmdAction) {
        match action {
            CmdAction::Slash(cmd) => {
                let act = slash::parse(&cmd);
                if !matches!(act, SlashAction::NotSlash) {
                    self.handle_slash(act);
                }
            }
            CmdAction::Go(d) => self.dest = d,
            CmdAction::OpenSettings => self.show_settings = true,
            CmdAction::NewChat => self.new_chat(),
            CmdAction::TokenLess(l) => self.set_token_less(l),
            CmdAction::PinCurrent => {
                let id = self.active_session_key();
                let now = !self.session.meta.pinned;
                self.update_session_meta(&id, None, Some(now), None);
            }
            CmdAction::Project => {
                if self.session.meta.project_dir.is_some() {
                    self.clear_project_dir();
                } else {
                    self.pick_project_dir();
                }
            }
            CmdAction::DeleteCurrent => {
                let id = self.active_session_key();
                self.ask_delete(&id);
            }
            CmdAction::ResetAllChats => {
                self.confirm_reset = true;
            }
            CmdAction::RenameCurrent => {
                self.rename_buf = self.session.meta.title.clone();
                self.rename_id = Some(self.active_session_key());
            }
            CmdAction::ToggleUsage => self.show_usage = !self.show_usage,
            CmdAction::PinUsage => {
                self.cfg.usage_pinned = !self.cfg.usage_pinned;
                if self.cfg.usage_pinned {
                    self.show_usage = true;
                }
                let _ = self.cfg.save();
            }
            CmdAction::GraphBuild => {
                self.dest = Dest::Graph;
                self.run_graph_build(false);
            }
            CmdAction::CloseTab => {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            CmdAction::ToggleTheme => self.toggle_theme(ctx),
            CmdAction::LiveSession(id) => {
                self.dest = Dest::Chat;
                self.switch_to_daemon_session(&id);
            }
            CmdAction::SavedSession(id) => {
                self.dest = Dest::Chat;
                self.load_session_id(&id);
            }
            CmdAction::OpenFile(path) => {
                self.dest = Dest::Chat;
                self.open_preview(path);
            }
            CmdAction::UseLlm(name) => match crate::llm_pool::set_active(&mut self.cfg, &name) {
                Ok(msg) => {
                    self.draft_api_base = self.cfg.api_base.clone();
                    self.draft_api_key = self.cfg.api_key.clone();
                    self.draft_model = self.cfg.model.clone();
                    let _ = self.cfg.save();
                    self.messages.push(UiMessage {
                        role: "system".into(),
                        text: msg,
                    });
                }
                Err(e) => self.push_error(format!("llm: {e}")),
            },
        }
    }

    fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.cmdk {
            return;
        }
        let p = pal();
        let entries = self.command_entries();
        let q = self.cmdk_query.trim().to_lowercase();
        let filtered: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                q.is_empty()
                    || e.label.to_lowercase().contains(&q)
                    || e.hint.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .take(40)
            .collect();
        if self.cmdk_sel >= filtered.len() {
            self.cmdk_sel = filtered.len().saturating_sub(1);
        }

        let (up, down, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter) && !i.modifiers.shift,
            )
        });
        if down && !filtered.is_empty() {
            self.cmdk_sel = (self.cmdk_sel + 1).min(filtered.len() - 1);
        }
        if up {
            self.cmdk_sel = self.cmdk_sel.saturating_sub(1);
        }

        let mut chosen: Option<usize> = None;
        let mut close = false;

        egui::Window::new("cmdk")
            .title_bar(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 120.0])
            .fixed_size(egui::vec2(560.0, 404.0))
            .frame(crate::theme::card_frame().inner_margin(egui::Margin::ZERO))
            .show(ctx, |ui| {
                ui.set_width(560.0);
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(crate::theme::meta("›"));
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut self.cmdk_query)
                                    .desired_width(500.0)
                                    .frame(false)
                                    .hint_text("search chats, files, actions…"),
                            );
                            edit.request_focus();
                        });
                    });
                w::hairline(ui);
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_height(340.0);
                        egui::ScrollArea::vertical()
                            .max_height(340.0)
                            .auto_shrink([false, false])
                            .id_salt("cmdk_list")
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                if filtered.is_empty() {
                                    ui.add_space(8.0);
                                    ui.label(crate::theme::meta("nothing found"));
                                }
                                for (row, &idx) in filtered.iter().enumerate() {
                                    let e = &entries[idx];
                                    let on = row == self.cmdk_sel;
                                    let resp = egui::Frame::new()
                                        .fill(if on {
                                            p.raised
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        })
                                        .corner_radius(egui::CornerRadius::same(8))
                                        .inner_margin(egui::Margin::symmetric(10, 8))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.set_width(ui.available_width() - 4.0);
                                                ui.label(
                                                    egui::RichText::new(&e.label)
                                                        .monospace()
                                                        .size(12.0)
                                                        .color(p.text_dim),
                                                );
                                                if !e.hint.is_empty() {
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.label(crate::theme::meta(
                                                                one_line(&e.hint, 34),
                                                            ));
                                                        },
                                                    );
                                                }
                                            });
                                        })
                                        .response;
                                    if ui
                                        .interact(
                                            resp.rect,
                                            ui.make_persistent_id(("cmdk", idx)),
                                            egui::Sense::click(),
                                        )
                                        .clicked()
                                    {
                                        chosen = Some(idx);
                                    }
                                }
                            });
                    });
            });

        if enter {
            if let Some(&idx) = filtered.get(self.cmdk_sel) {
                chosen = Some(idx);
            }
        }
        if let Some(idx) = chosen {
            let mut entries = entries;
            let action = entries.swap_remove(idx).action;
            close = true;
            self.run_command(ctx, action);
        }
        if close {
            self.cmdk = false;
            self.cmdk_query.clear();
            self.cmdk_sel = 0;
        }
    }
}

impl eframe::App for HarnessApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events(ctx);
        self.poll_update_slot();
        self.refresh_mem_stats(false);
        self.sync_active_into_tabs();
        self.refresh_swarm(false);
        self.meter.tick();
        let any_busy = self.busy || self.open_tabs.iter().any(|t| t.busy);
        if matches!(self.dest, Dest::Swarm | Dest::WebServer) || any_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(400));
        }
        // o gráfico só "corre" se houver frame; enquanto o painel estiver
        // aberto pedimos repaint no ritmo da amostragem
        if self.show_usage {
            ctx.request_repaint_after(std::time::Duration::from_millis(
                METER_TICK_MS as u64,
            ));
        }

        self.handle_shortcuts(ctx);

        self.rail(ctx);
        self.sessions_panel(ctx);
        // antes das barras inferiores, para ocupar a altura inteira
        self.usage_panel(ctx);
        self.status_bar(ctx);
        if self.dest == Dest::Chat {
            self.composer(ctx);
            self.chat_top_bar(ctx);
            if self.ctx_panel {
                self.context_panel(ctx);
            }
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal().bg)
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| match self.dest {
                Dest::Chat => self.chat_view(ui),
                Dest::Files => self.files_view(ui),
                Dest::Graph => self.graph_view(ui),
                Dest::Memory => self.memory_view_ui(ui),
                Dest::Swarm => self.swarm_view(ui),
                Dest::Diag => self.diag_view(ui),
                Dest::WebServer => self.webserver_view(ui),
            });

        self.setup_window(ctx);
        self.settings_window(ctx);
        self.delete_window(ctx);
        self.reset_window(ctx);
        self.command_palette(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.busy {
            let sid = self.session.meta.daemon_session_id.clone();
            if let Some(client) = &self.daemon {
                if !sid.is_empty() {
                    let _ = client.cancel(&sid);
                }
            }
        }
        if let Ok(mut g) = swarm::global_swarm().lock() {
            g.stop_all();
        }
        webserver::stop();
        self.persist();
        // leave daemon running so reattach works
    }
}

// ---------------------------------------------------------------------------
// Setup (passo único) e Settings (seções)
// ---------------------------------------------------------------------------

/// Presets de provedor do wizard de setup.
const PROVIDERS: [(&str, &str, &str); 4] = [
    ("Grok", "https://api.x.ai/v1", "grok-4.5"),
    ("OpenAI", "https://api.openai.com/v1", "gpt-4.1-mini"),
    ("Meta", "https://api.meta.ai/v1", "muse-spark-1.2"),
    ("Other", "", ""),
];

impl HarnessApp {
    fn setup_window(&mut self, ctx: &egui::Context) {
        if !self.show_setup {
            return;
        }
        let p = pal();
        egui::Window::new("setup")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(560.0)
            .frame(crate::theme::card_frame().inner_margin(egui::Margin::same(20)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
                    crate::icon::paint_mark(ui.painter(), r);
                    ui.label(crate::theme::ui_medium("Let\u{2019}s set up harness", 18.0));
                });
                ui.add_space(12.0);

                ui.label(crate::theme::ui_medium("Workspace folder", 12.0));
                ui.label(
                    egui::RichText::new(
                        "Everything the agent generates lands here, under code/ docs/ sheets/ pdfs/ web/.",
                    )
                    .size(12.0)
                    .color(p.muted),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft_workspace)
                            .desired_width(360.0),
                    );
                    if ui.button("Choose…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Default folder for code and documents")
                            .pick_folder()
                        {
                            self.draft_workspace = dir.display().to_string();
                        }
                    }
                    if w::chip(ui, "Documents/Harness").clicked() {
                        self.draft_workspace =
                            config::suggested_workspace().display().to_string();
                    }
                });

                ui.add_space(12.0);
                w::hairline(ui);
                ui.add_space(12.0);

                ui.label(crate::theme::ui_medium("Provider", 12.0));
                ui.horizontal(|ui| {
                    for (name, base, model) in PROVIDERS {
                        let on = if base.is_empty() {
                            !PROVIDERS
                                .iter()
                                .any(|(_, b, _)| !b.is_empty() && *b == self.draft_api_base)
                        } else {
                            self.draft_api_base == base
                        };
                        let card = egui::Button::new(
                            crate::theme::ui_medium(name, 12.5)
                                .color(if on { p.text } else { p.text_dim }),
                        )
                        .fill(if on { p.raised } else { p.card })
                        .stroke(egui::Stroke::new(
                            if on { 1.5 } else { 1.0 },
                            if on { p.accent } else { p.border },
                        ))
                        .corner_radius(egui::CornerRadius::same(9))
                        .min_size(egui::vec2(150.0, 42.0));
                        if ui
                            .add(card)
                            .on_hover_text(if base.is_empty() { "manual base URL" } else { base })
                            .clicked()
                            && !base.is_empty()
                        {
                            self.draft_api_base = base.into();
                            if self.draft_model.trim().is_empty() {
                                self.draft_model = model.into();
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                labeled_edit(ui, "API key", &mut self.draft_api_key, true);
                labeled_edit(ui, "Base URL", &mut self.draft_api_base, false);
                labeled_edit(ui, "Model", &mut self.draft_model, false);

                ui.add_space(12.0);
                let can_save = !self.draft_workspace.trim().is_empty()
                    && !self.draft_api_key.trim().is_empty();
                ui.horizontal(|ui| {
                    let responses_api = crate::llm_pool::wire_of("", self.draft_api_base.trim())
                        == crate::llm_pool::Wire::Responses;
                    let btn = if responses_api { "Save and continue" } else { "Test and continue" };
                    if w::primary_button(ui, btn, can_save).clicked() {
                        if responses_api {
                            // A Responses API não expõe /models; não dá para
                            // testar a key sem gastar uma chamada de verdade.
                            self.status = "provider saved (key checked on first message)".into();
                            self.apply_settings();
                            if !self.cfg.needs_setup() {
                                self.show_setup = false;
                            }
                        } else {
                            match crate::llm_pool::fetch_remote_models(
                                self.draft_api_base.trim(),
                                self.draft_api_key.trim(),
                            ) {
                                Ok(models) => {
                                    self.status =
                                        format!("provider ok · {} models", models.len());
                                    self.apply_settings();
                                    if !self.cfg.needs_setup() {
                                        self.show_setup = false;
                                    }
                                }
                                Err(e) => self.push_error(format!("provider: {e}")),
                            }
                        }
                    }
                    ui.label(crate::theme::meta("the rest you can set later in ⌘K"));
                });
                if !can_save {
                    ui.label(
                        egui::RichText::new("Fill in the default folder + API key.")
                            .size(12.0)
                            .color(p.accent),
                    );
                }
            });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let p = pal();
        let mut close = false;
        egui::Window::new("settings")
            .title_bar(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size(egui::vec2(720.0, 604.0))
            .frame(crate::theme::card_frame().inner_margin(egui::Margin::ZERO))
            .show(ctx, |ui| {
                let body_h = 470.0_f32;
                ui.set_width(720.0);

                // cabeçalho
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(crate::theme::ui_medium("Settings", 16.0).color(p.text));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(crate::theme::meta("×"))
                                                .frame(false),
                                        )
                                        .clicked()
                                    {
                                        close = true;
                                    }
                                },
                            );
                        });
                    });
                w::hairline(ui);

                // corpo: lista à esquerda, uma seção por vez
                ui.allocate_ui_with_layout(
                    egui::vec2(720.0, body_h),
                    egui::Layout::left_to_right(egui::Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        egui::Frame::new()
                            .fill(p.bg_side)
                            .inner_margin(egui::Margin::symmetric(8, 10))
                            .show(ui, |ui| {
                                ui.set_width(150.0);
                                ui.set_min_height(body_h - 20.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    for s in SettingsSection::ALL {
                                        let on = s == self.settings_section;
                                        let btn = egui::Button::new(if on {
                                            crate::theme::ui_medium(s.label(), 12.0).color(p.text)
                                        } else {
                                            egui::RichText::new(s.label())
                                                .size(12.0)
                                                .color(p.muted)
                                        })
                                        .fill(if on { p.card } else { egui::Color32::TRANSPARENT })
                                        .stroke(if on {
                                            egui::Stroke::new(1.0, p.border)
                                        } else {
                                            egui::Stroke::NONE
                                        })
                                        .corner_radius(egui::CornerRadius::same(7))
                                        .min_size(egui::vec2(150.0, 28.0));
                                        if ui.add(btn).clicked() {
                                            self.settings_section = s;
                                        }
                                    }
                                });
                            });
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(14))
                            .show(ui, |ui| {
                                ui.set_width(720.0 - 166.0 - 28.0);
                                ui.set_min_height(body_h - 28.0);
                                ui.spacing_mut().item_spacing.y = 7.0;
                                egui::ScrollArea::vertical()
                                    .auto_shrink([false, false])
                                    .id_salt("settings_body")
                                    .show(ui, |ui| {
                                        // o corpo herda o layout horizontal do pai;
                                        // sem isto cada bloco vira uma coluna.
                                        ui.with_layout(
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| match self.settings_section {
                                                SettingsSection::Models => {
                                                    self.settings_models(ui)
                                                }
                                                SettingsSection::Workspace => {
                                                    self.settings_workspace(ui)
                                                }
                                                SettingsSection::Approvals => {
                                                    self.settings_approvals(ui)
                                                }
                                                SettingsSection::Memory => {
                                                    self.settings_memory(ui)
                                                }
                                                SettingsSection::Swarm => self.settings_swarm(ui),
                                                SettingsSection::Web => self.settings_web(ui),
                                                SettingsSection::Appearance => {
                                                    self.settings_appearance(ui, ctx)
                                                }
                                                SettingsSection::Updates => {
                                                    self.settings_updates(ui)
                                                }
                                            },
                                        );
                                    });
                            });
                    },
                );

                w::hairline(ui);
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(crate::theme::meta("⌘K opens these actions too"));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if w::primary_button(ui, "Save", true).clicked() {
                                        self.apply_settings();
                                    }
                                    if w::chip(ui, "Cancel").clicked() {
                                        self.draft_api_base = self.cfg.api_base.clone();
                                        self.draft_api_key = self.cfg.api_key.clone();
                                        self.draft_model = self.cfg.model.clone();
                                        self.draft_workspace =
                                            self.cfg.workspace.display().to_string();
                                        self.draft_auto_shell = self.cfg.auto_approve_shell;
                                        self.draft_stream = self.cfg.stream;
                                        self.draft_update_repo = self.cfg.update_repo.clone();
                                        close = true;
                                    }
                                },
                            );
                        });
                    });
            });
        if close {
            self.show_settings = false;
        }
    }

    fn settings_models(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.label(crate::theme::ui_medium("Active LLM", 13.0));
        labeled_edit(ui, "Base URL", &mut self.draft_api_base, false);
        labeled_edit(ui, "API key", &mut self.draft_api_key, true);
        labeled_edit(ui, "Model", &mut self.draft_model, false);
        ui.checkbox(
            &mut self.cfg.llm_auto_failover,
            "Automatic failover on rate limit / quota",
        );
        ui.checkbox(
            &mut self.cfg.llm_multi_worker,
            "Workers may use an LLM flagged for workers",
        );

        ui.add_space(10.0);
        ui.label(crate::theme::ui_medium("Weighted pool", 13.0));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.cfg.llm_rotate_enabled, "Rotate by weight");
            ui.label(crate::theme::meta("every"));
            let mut mins = self.cfg.llm_rotate_minutes.max(1);
            if ui
                .add(egui::DragValue::new(&mut mins).range(1..=10080).suffix(" min"))
                .changed()
            {
                self.cfg.llm_rotate_minutes = mins;
                self.cfg.llm_rotate_slot.clear();
            }
            for (label, m) in [("30m", 30u32), ("1h", 60), ("2h", 120), ("6h", 360), ("24h", 1440)]
            {
                if ui.small_button(label).clicked() {
                    self.cfg.llm_rotate_minutes = m;
                    self.cfg.llm_rotate_enabled = true;
                    self.cfg.llm_rotate_slot.clear();
                }
            }
        });
        ui.label(crate::theme::meta(
            "Weights split traffic (70+30 ≈ 70%/30%). Code/Office limit which pool is used.",
        ));
        ui.add_space(6.0);

        let total: u32 = self
            .cfg
            .llm_pool
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.weight.max(1))
            .sum::<u32>()
            .max(1);
        let mut fetch_idx: Option<usize> = None;
        let mut use_name: Option<String> = None;
        let row_w = (ui.available_width() - 24.0).max(240.0);
        for (i, ep) in self.cfg.llm_pool.iter_mut().enumerate() {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, p.border_soft))
                .corner_radius(egui::CornerRadius::same(9))
                .inner_margin(egui::Margin::symmetric(11, 9))
                .show(ui, |ui| {
                    ui.set_width(row_w);
                    ui.spacing_mut().item_spacing.y = 5.0;
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut ep.enabled, "");
                        ui.label(crate::theme::mono_medium(ep.name.clone(), 12.0));
                        if crate::llm_pool::wire_of(&ep.wire, &ep.api_base)
                            == crate::llm_pool::Wire::Responses
                        {
                            ui.label(crate::theme::meta("responses"))
                                .on_hover_text(
                                    "Speaks the Responses API (input[]/SSE events), \
                                     not Chat Completions",
                                );
                        }
                        let share = if ep.enabled {
                            ep.weight.max(1) as f32 / total as f32
                        } else {
                            0.0
                        };
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.small_button("Usar").clicked() {
                                    use_name = Some(ep.name.clone());
                                }
                                if ui.small_button("Modelos…").clicked() {
                                    fetch_idx = Some(i);
                                }
                                ui.add(
                                    egui::DragValue::new(&mut ep.weight)
                                        .range(1..=1000)
                                        .speed(1.0),
                                );
                                ui.label(crate::theme::meta(format!(
                                    "{}%",
                                    (share * 100.0).round() as i32
                                )));
                                let (bar, _) = ui.allocate_exact_size(
                                    egui::vec2(90.0, 5.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    egui::CornerRadius::same(3),
                                    p.border_soft,
                                );
                                let filled = egui::Rect::from_min_size(
                                    bar.min,
                                    egui::vec2(bar.width() * share, 5.0),
                                );
                                ui.painter().rect_filled(
                                    filled,
                                    egui::CornerRadius::same(3),
                                    p.accent,
                                );
                            },
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut ep.use_for_code, "Code");
                        ui.checkbox(&mut ep.use_for_office, "Office");
                        ui.checkbox(&mut ep.use_for_workers, "Worker");
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::theme::meta("model"));
                        ui.add(egui::TextEdit::singleline(&mut ep.model).desired_width(130.0));
                        ui.label(crate::theme::meta("key"));
                        let key_resp = ui.add(
                            egui::TextEdit::singleline(&mut ep.api_key)
                                .desired_width(110.0)
                                .password(true),
                        );
                        // colar a key já habilita o endpoint
                        // (ex.: o meta é semeado desligado sem env key)
                        if key_resp.changed() && !ep.api_key.trim().is_empty() {
                            ep.enabled = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::theme::meta("base"));
                        ui.add(
                            egui::TextEdit::singleline(&mut ep.api_base)
                                .desired_width(row_w - 60.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label(crate::theme::meta("wire"));
                        let sel = match ep.wire.as_str() {
                            "chat" => 1,
                            "responses" => 2,
                            _ => 0,
                        };
                        if let Some(i) = w::segmented(ui, &["auto", "chat", "responses"], sel) {
                            ep.wire = match i {
                                1 => "chat".into(),
                                2 => "responses".into(),
                                _ => String::new(),
                            };
                        }
                        ui.label(crate::theme::meta(
                            "auto = deduzido do host (meta.ai → responses)",
                        ));
                    });
                });
            ui.add_space(4.0);
        }

        if let Some(name) = use_name {
            if let Ok(msg) = crate::llm_pool::set_active(&mut self.cfg, &name) {
                self.draft_api_base = self.cfg.api_base.clone();
                self.draft_api_key = self.cfg.api_key.clone();
                self.draft_model = self.cfg.model.clone();
                let _ = self.cfg.save();
                self.messages.push(UiMessage {
                    role: "system".into(),
                    text: msg,
                });
            }
        }
        if let Some(i) = fetch_idx {
            if let Some(ep) = self.cfg.llm_pool.get(i).cloned() {
                match crate::llm_pool::fetch_remote_models(&ep.api_base, &ep.api_key) {
                    Ok(models) if !models.is_empty() => {
                        let preview: String =
                            models.iter().take(40).cloned().collect::<Vec<_>>().join("\n");
                        self.messages.push(UiMessage {
                            role: "system".into(),
                            text: format!(
                                "Models for {} ({} total):\n{}",
                                ep.name,
                                models.len(),
                                preview
                            ),
                        });
                    }
                    Ok(_) => self.push_error(format!("no models returned for {}", ep.name)),
                    Err(e) => self.push_error(format!("models {}: {e}", ep.name)),
                }
            }
        }
        ui.label(crate::theme::meta(crate::llm_pool::weights_text(
            &self.cfg, self.mode,
        )));
        ui.label(crate::theme::meta(
            "/llm weights · /llm every 60 · /llm rotate_on · /llm use grok",
        ));

        ui.add_space(12.0);
        w::hairline(ui);
        ui.add_space(8.0);
        ui.label(crate::theme::ui_medium("Token Less Cost — compressed replies", 13.0));
        ui.label(crate::theme::meta(
            "default for new chats; each chat switches in the composer chip or with /tokenless",
        ));
        let sel = TokenLessLevel::ALL
            .iter()
            .position(|l| *l == self.cfg.token_less)
            .unwrap_or(0);
        if let Some(i) = w::segmented(ui, &["off", "lite", "full", "ultra"], sel) {
            self.cfg.token_less = TokenLessLevel::ALL[i];
        }
        ui.label(crate::theme::meta(format!(
            "{} · this chat: {}",
            self.cfg.token_less.label(),
            self.token_less_level().tag()
        )));
        ui.label(crate::theme::meta(
            "shrinks output only — input, history and reasoning are unchanged",
        ));
    }

    fn settings_workspace(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::theme::ui_medium("Default output folder", 13.0));
        ui.label(crate::theme::meta(
            "code + docs + sheets + pdfs + web live here",
        ));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.draft_workspace).desired_width(320.0));
            if ui.button("Choose…").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Default folder for code and documents")
                    .pick_folder()
                {
                    self.draft_workspace = dir.display().to_string();
                }
            }
        });
        ui.label(crate::theme::meta(
            "Subfolders created: code/ docs/ sheets/ pdfs/ web/",
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("web server port"));
            let mut port = self.cfg.web_server_port;
            if ui
                .add(egui::DragValue::new(&mut port).range(1024..=65535))
                .changed()
            {
                self.cfg.web_server_port = port;
            }
        });
    }

    fn settings_approvals(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::theme::ui_medium("Approvals", 13.0));
        ui.checkbox(&mut self.cfg.auto_approve_safe, "Auto-approve safe tools");
        ui.checkbox(
            &mut self.draft_auto_shell,
            "Auto-approve shell commands",
        );
        ui.checkbox(&mut self.draft_stream, "Stream LLM tokens");
        ui.add_space(6.0);
        ui.label(crate::theme::meta(
            "Approval requests appear inline on the tool call row, in the chat.",
        ));
    }

    fn settings_memory(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::theme::ui_medium("Memory", 13.0));
        ui.checkbox(
            &mut self.cfg.memory_auto_recall,
            "Automatic recall before every reply",
        );
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("history sent to the LLM"));
            let mut cap = self.cfg.history_cap;
            if ui
                .add(egui::DragValue::new(&mut cap).range(4..=200).suffix(" msgs"))
                .changed()
            {
                self.cfg.history_cap = cap;
            }
        });
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("tool result cap"));
            let mut cap = self.cfg.tool_result_cap;
            if ui
                .add(
                    egui::DragValue::new(&mut cap)
                        .range(1000..=200_000)
                        .suffix(" chars"),
                )
                .changed()
            {
                self.cfg.tool_result_cap = cap;
            }
        });
        ui.add_space(6.0);
        ui.label(crate::theme::meta(crate::memory_graph::ambient_status()));
    }

    fn settings_swarm(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::theme::ui_medium("Swarm", 13.0));
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("max workers"));
            let mut n = self.cfg.swarm_max_workers;
            if ui.add(egui::DragValue::new(&mut n).range(1..=3)).changed() {
                self.cfg.swarm_max_workers = n;
            }
        });
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("live sessions in the daemon"));
            let mut n = self.cfg.max_sessions;
            if ui.add(egui::DragValue::new(&mut n).range(1..=256)).changed() {
                self.cfg.max_sessions = n;
            }
        });
        ui.add_space(6.0);
        ui.label(crate::theme::meta(format!(
            "{} workers right now (daemon)",
            self.swarm_running()
        )));
    }

    fn settings_web(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::theme::ui_medium("Web pages", 13.0));
        ui.checkbox(
            &mut self.cfg.web_markdown,
            "Read pages as markdown (headings, links, code)",
        );
        ui.label(crate::theme::meta(
            "Off = flat text, like before. On, nav/cookie/footer chrome is dropped.",
        ));
        ui.add_space(10.0);

        ui.label(crate::theme::ui_medium("Crawl limits", 13.0));
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("max pages"));
            let mut n = self.cfg.web_crawl_max_pages;
            if ui.add(egui::DragValue::new(&mut n).range(1..=200)).changed() {
                self.cfg.web_crawl_max_pages = n;
            }
            ui.add_space(12.0);
            ui.label(crate::theme::meta("max depth"));
            let mut d = self.cfg.web_crawl_max_depth;
            if ui.add(egui::DragValue::new(&mut d).range(0..=5)).changed() {
                self.cfg.web_crawl_max_depth = d;
            }
        });
        ui.checkbox(&mut self.cfg.web_crawl_same_domain, "Same domain only");
        ui.checkbox(&mut self.cfg.web_respect_robots, "Respect robots.txt");
        ui.label(crate::theme::meta(
            "The agent may ask for less, never for more than these.",
        ));
        ui.add_space(10.0);

        ui.label(crate::theme::ui_medium("Loop guards", 13.0));
        ui.checkbox(
            &mut self.cfg.stuck_detect,
            "Block a tool repeated with identical arguments",
        );
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("after"));
            let mut n = self.cfg.stuck_threshold;
            if ui.add(egui::DragValue::new(&mut n).range(2..=20)).changed() {
                self.cfg.stuck_threshold = n;
            }
            ui.label(crate::theme::meta("identical calls in one turn"));
        });
        ui.horizontal(|ui| {
            ui.label(crate::theme::meta("Gauntlet Loop ceiling"));
            let mut n = self.cfg.gauntlet_max_iterations;
            if ui.add(egui::DragValue::new(&mut n).range(1..=100)).changed() {
                self.cfg.gauntlet_max_iterations = n;
            }
            ui.label(crate::theme::meta("auto-continues"));
        });
        ui.add_space(6.0);
        ui.label(crate::theme::meta(
            "A looping turn also stops the Gauntlet Loop instead of burning what is left.",
        ));
    }

    fn settings_appearance(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(crate::theme::ui_medium("Appearance", 13.0));
        let sel = match self.cfg.theme {
            crate::theme::ThemeMode::Paper => 0,
            crate::theme::ThemeMode::Ember => 1,
        };
        if let Some(i) = w::segmented(ui, &["Paper (light)", "Ember (dark)"], sel) {
            let m = if i == 0 {
                crate::theme::ThemeMode::Paper
            } else {
                crate::theme::ThemeMode::Ember
            };
            if m != self.cfg.theme {
                self.cfg.theme = m;
                crate::theme::set_mode(ctx, m);
                let _ = self.cfg.save();
            }
        }
        ui.add_space(6.0);
        ui.label(crate::theme::meta("⇧⌘D toggles any time"));
        ui.add_space(10.0);
        ui.label(crate::theme::meta(
            "Font: IBM Plex Sans + Mono, embedded in the binary (OFL).",
        ));
    }

    fn settings_updates(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::theme::ui_medium("Updates", 13.0));
        labeled_edit(ui, "GitHub repo", &mut self.draft_update_repo, false);
        ui.label(crate::theme::meta(format!(
            "v{} · {}",
            self.update.current, self.update.message
        )));
        ui.checkbox(&mut self.cfg.check_updates_on_start, "Check on startup");
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.update_busy, egui::Button::new("Check"))
                .clicked()
            {
                self.cfg.update_repo = self.draft_update_repo.clone();
                self.spawn_update_check();
            }
            if ui
                .add_enabled(
                    self.update.download_url.is_some() && !self.update_busy,
                    egui::Button::new("Download"),
                )
                .clicked()
            {
                if let Some(url) = self.update.download_url.clone() {
                    self.update_busy = true;
                    thread::spawn(move || {
                        let st = match update::download_update(&url) {
                            Ok(path) => UpdateStatus {
                                current: update::CURRENT_VERSION.into(),
                                staged_path: Some(path.clone()),
                                message: format!("downloaded {}", path.display()),
                                ..Default::default()
                            },
                            Err(e) => UpdateStatus {
                                current: update::CURRENT_VERSION.into(),
                                message: format!("download failed: {e}"),
                                ..Default::default()
                            },
                        };
                        if let Ok(mut g) = UPDATE_SLOT.lock() {
                            *g = Some(st);
                        }
                    });
                }
            }
            if ui
                .add_enabled(
                    self.update.staged_path.is_some(),
                    egui::Button::new("Apply and restart"),
                )
                .clicked()
            {
                if let Some(path) = self.update.staged_path.clone() {
                    match update::apply_update(&path) {
                        Ok(msg) => {
                            self.update.message = msg;
                            let _ = update::relaunch();
                        }
                        Err(e) => self.update.message = format!("apply failed: {e}"),
                    }
                }
            }
        });
        if !self.update.notes.is_empty() {
            ui.add_space(8.0);
            ui.label(crate::theme::ui_medium("Release notes", 12.5));
            ui.add(
                egui::Label::new(egui::RichText::new(&self.update.notes).size(12.5)).wrap(),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Widgets locais
// ---------------------------------------------------------------------------

/// Linha da lista de sessões: título + subtítulo mono, ponto quando rodando.
fn session_row(
    ui: &mut egui::Ui,
    salt: (&str, &str),
    title: &str,
    subtitle: &str,
    busy: bool,
    selected: bool,
    pinned: bool,
) -> egui::Response {
    let p = pal();
    let mut frame = egui::Frame::new()
        .corner_radius(egui::CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(10, 8));
    if selected {
        frame = frame
            .fill(p.card)
            .stroke(egui::Stroke::new(1.0, p.border));
    }
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width() - 2.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if busy {
                    w::dot(ui, p.accent, 3.0);
                } else if pinned {
                    w::dot(ui, p.text_dim, 2.5);
                } else if selected {
                    w::dot(ui, p.muted, 3.0);
                } else {
                    ui.add_space(8.0);
                }
                ui.label(
                    egui::RichText::new(one_line(title, 34))
                        .size(12.5)
                        .color(if selected { p.text } else { p.text_dim }),
                );
            });
            if !subtitle.trim().is_empty() && subtitle != title {
                ui.horizontal(|ui| {
                    ui.add_space(13.0);
                    ui.label(crate::theme::meta(one_line(subtitle, 34)));
                });
            }
        });
    });
    let id = ui.make_persistent_id((salt.0, salt.1));
    let resp = ui.interact(inner.response.rect, id, egui::Sense::click());
    if resp.hovered() && !selected {
        ui.painter().rect_filled(
            inner.response.rect,
            egui::CornerRadius::same(9),
            p.raised.gamma_multiply(0.6),
        );
    }
    resp
}

/// Uma tool call: cabeçalho de uma linha, corpo expansível, aprovação inline.
fn tool_row(
    ui: &mut egui::Ui,
    idx: usize,
    call: &ToolCall,
    time: f64,
) -> Option<ApprovalDecision> {
    let p = pal();
    let id = ui.make_persistent_id(("toolcall", idx));
    let mut open = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let mut decision = None;

    let head_bg = if open { p.raised } else { egui::Color32::TRANSPARENT };
    let header = egui::Frame::new()
        .fill(head_bg)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.x = 9.0;
                w::chevron(ui, open, p.muted);
                ui.label(crate::theme::mono_medium(&call.name, 12.0).color(
                    if call.state == ToolState::Err {
                        p.error
                    } else {
                        p.text_dim
                    },
                ));
                ui.label(
                    egui::RichText::new(one_line(&call.target, 48))
                        .monospace()
                        .size(12.0)
                        .color(p.muted),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match call.state {
                        ToolState::Running => w::dot(ui, p.accent, 2.5),
                        ToolState::Ok => w::dot(ui, p.ok, 2.5),
                        ToolState::Err => w::dot(ui, p.error, 2.5),
                        ToolState::NeedsApproval => w::dot(ui, p.accent, 2.5),
                    }
                    if !call.metric.is_empty() {
                        ui.label(
                            egui::RichText::new(&call.metric)
                                .monospace()
                                .size(11.0)
                                .color(match call.state {
                                    ToolState::Err | ToolState::NeedsApproval => p.error,
                                    ToolState::Ok => p.muted,
                                    ToolState::Running => p.muted,
                                }),
                        );
                    }
                });
            });
        });

    let click = ui.interact(
        header.response.rect,
        id.with("hdr"),
        egui::Sense::click(),
    );
    if click.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }

    if call.state == ToolState::Running {
        w::indeterminate_bar(ui, time);
    }

    if call.state == ToolState::NeedsApproval {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(one_line(&call.body, 90))
                            .size(11.5)
                            .color(p.text_dim),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if w::primary_button(ui, "Allow", true).clicked() {
                        decision = Some(ApprovalDecision::Allow);
                    }
                    if w::chip(ui, "Always").clicked() {
                        decision = Some(ApprovalDecision::AllowAlwaysShell);
                    }
                    if w::chip(ui, "Deny").clicked() {
                        decision = Some(ApprovalDecision::Deny);
                    }
                });
            });
    } else if open && !call.body.trim().is_empty() {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .id_salt(("toolbody", idx))
                    .show(ui, |ui| {
                        for line in call.body.lines().take(400) {
                            let (fg, bg) = if line.starts_with('+') {
                                (p.diff_add_fg, Some(p.diff_add_bg))
                            } else if line.starts_with('-') {
                                (p.diff_del_fg, Some(p.diff_del_bg))
                            } else {
                                (p.text_dim, None)
                            };
                            let mut f = egui::Frame::new()
                                .inner_margin(egui::Margin::symmetric(0, 0));
                            if let Some(bg) = bg {
                                f = f.fill(bg);
                            }
                            f.show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(line)
                                            .monospace()
                                            .size(12.0)
                                            .color(fg),
                                    )
                                    .wrap(),
                                );
                            });
                        }
                    });
            });
    }
    decision
}

// ---------------------------------------------------------------------------
// Helpers de texto
// ---------------------------------------------------------------------------

/// Mesmo conteúdo do painel, em texto (usado pelo /swarm).
fn swarm_snapshot_text(snap: &crate::swarm::SwarmSnapshot) -> String {
    if snap.agents.is_empty() {
        return "swarm: nenhum worker no daemon".into();
    }
    let mut lines = vec![format!("{} agente(s)", snap.agents.len())];
    for a in &snap.agents {
        lines.push(format!(
            "- {} [{}] {:?}: {} | {}",
            a.name,
            crate::protocol::short_id(&a.id),
            a.state,
            one_line(&a.task, 60),
            one_line(&a.last_message, 80)
        ));
    }
    for (sid, plan) in &snap.plans {
        lines.push(format!("plano {sid} v{}", plan.version));
        for t in &plan.tasks {
            lines.push(format!("  [{}] {} {}", t.status, t.id, t.title));
        }
    }
    if !snap.claims.is_empty() {
        lines.push("claimed files:".into());
        for (path, owner) in &snap.claims {
            lines.push(format!("  {path} → {owner}"));
        }
    }
    if !snap.bus.is_empty() {
        lines.push("bus:".into());
        for m in snap.bus.iter().rev().take(8) {
            lines.push(format!("  {}→{}: {}", m.from, m.to, one_line(&m.body, 100)));
        }
    }
    lines.join("\n")
}

/// Card padrão dos painéis: fundo `card`, borda suave, raio 10.
fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let p = pal();
    egui::Frame::new()
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0, p.border_soft))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() - 4.0);
            add(ui);
        });
}

fn fmt_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn summary_title(s: &SessionSummary) -> String {
    if s.title.is_empty() || s.title == "New session" {
        if s.folder.is_empty() {
            s.short_id.clone()
        } else {
            s.folder.clone()
        }
    } else {
        s.title.clone()
    }
}

fn meta_title(m: &SessionMeta) -> String {
    // list_sessions() já aplica display_title (resumo de 5 palavras) no meta;
    // aqui é só o fallback para o carimbo da pasta quando veio vazio.
    if m.title.is_empty() || m.title == "New session" {
        if m.chat_folder_name.is_empty() {
            m.id.chars().take(8).collect()
        } else {
            m.chat_folder_name.clone()
        }
    } else {
        m.title.clone()
    }
}

/// "2026-08-04" a partir de um RFC3339 (ou string vazia).
fn day_key(rfc3339: &str) -> String {
    rfc3339.chars().take(10).collect()
}

fn today_key() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn shorten_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
        let h = home.display().to_string();
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{rest}");
        }
    }
    s
}

/// "GUI 84 MB · daemon 41 MB · tabs 3" → "84 MB · 41 MB"
fn short_mem(line: &str) -> String {
    line.split(" · ")
        .take(2)
        .map(|s| {
            s.replace("GUI ", "")
                .replace("daemon ", "")
                .trim()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" · ")
}



/// Devolve a URL que o usuário pediu para abrir, se pediu.
fn render_preview(ui: &mut egui::Ui, p: &PreviewContent) -> Option<String> {
    let mut open: Option<String> = None;
    match p {
        PreviewContent::Text { title, body } => {
            ui.heading(egui::RichText::new(title).size(18.0));
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(body).monospace().size(13.0)).wrap(),
                );
            });
        }
        PreviewContent::Table {
            title,
            sheet,
            headers,
            rows,
            note,
        } => {
            ui.heading(egui::RichText::new(format!("{title} · {sheet}")).size(16.0));
            ui.label(egui::RichText::new(note).size(12.0).weak());
            egui::ScrollArea::both().show(ui, |ui| {
                egui::Grid::new("sheet_preview")
                    .striped(true)
                    .show(ui, |ui| {
                        for h in headers {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for row in rows {
                            for cell in row {
                                ui.label(egui::RichText::new(cell).size(12.0));
                            }
                            for _ in row.len()..headers.len() {
                                ui.label("");
                            }
                            ui.end_row();
                        }
                    });
            });
        }
        PreviewContent::WebPage {
            title,
            path,
            url,
            source_preview,
        } => {
            ui.heading(egui::RichText::new(format!("🌐 {title}")).size(18.0));
            ui.label(
                egui::RichText::new("Served locally — Run opens it in the harness WebView")
                    .size(13.0)
                    .weak(),
            );
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("▶ Run  {title}")).size(15.0).strong(),
                    )
                    .min_size(egui::vec2(220.0, 34.0)),
                )
                .on_hover_text("Open the page in the internal WebView window")
                .clicked()
            {
                open = Some(url.clone());
            }
            ui.label(egui::RichText::new(path).size(12.0).monospace());
            ui.label(egui::RichText::new(url).size(12.0).monospace().strong());
            ui.separator();
            ui.label(egui::RichText::new("Source").size(14.0).strong());
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(source_preview).monospace().size(12.0),
                    )
                    .wrap(),
                );
            });
        }
        PreviewContent::Error { title, message } => {
            ui.colored_label(
                egui::Color32::from_rgb(240, 100, 100),
                format!("{title}: {message}"),
            );
        }
    }
    open
}

fn labeled_edit(ui: &mut egui::Ui, label: &str, value: &mut String, password: bool) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(13.0)
                .color(pal().muted),
        );
        let mut te = egui::TextEdit::singleline(value).desired_width(340.0);
        if password {
            te = te.password(true);
        }
        ui.add(te);
    });
}

/// HTML gerados neste chat, `web/index.html` primeiro — é o que o usuário
/// quer ver quando pediu "um jogo em html".
fn html_artifacts(artifacts: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = artifacts
        .iter()
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
                Some("html") | Some("htm")
            )
        })
        .cloned()
        .collect();
    v.sort_by_key(|p| {
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let idx = if name.eq_ignore_ascii_case("index.html") { 0 } else { 1 };
        (idx, p.components().count())
    });
    v
}

#[cfg(test)]
mod preview_pick {
    use super::*;

    #[test]
    fn index_html_vem_primeiro_e_o_resto_por_profundidade() {
        let a = vec![
            PathBuf::from("/c/deep/nested/page.html"),
            PathBuf::from("/c/main.rs"),
            PathBuf::from("/c/about.html"),
            PathBuf::from("/c/web/index.html"),
            PathBuf::from("/c/notes.md"),
        ];
        let got = html_artifacts(&a);
        assert_eq!(got[0], PathBuf::from("/c/web/index.html"), "index manda");
        assert_eq!(got.len(), 3, "só html: {got:?}");
        assert_eq!(got[1], PathBuf::from("/c/about.html"), "mais raso antes");
    }

    #[test]
    fn chat_sem_pagina_nao_inventa() {
        let a = vec![PathBuf::from("/c/main.rs"), PathBuf::from("/c/a.md")];
        assert!(html_artifacts(&a).is_empty());
    }
}

fn scan_artifacts(root: &std::path::Path, mode: AppMode) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if matches!(
                        name,
                        "target" | ".git" | "node_modules" | ".venv" | "venv"
                    ) {
                        continue;
                    }
                }
                stack.push(path);
            } else if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                let e = ext.to_ascii_lowercase();
                let keep = match mode {
                    AppMode::Code => matches!(
                        e.as_str(),
                        "rs" | "py" | "js" | "ts" | "tsx" | "go" | "java" | "c" | "cpp" | "h"
                            | "md" | "toml" | "json" | "yml" | "yaml" | "sh" | "txt" | "html"
                            | "css" | "swift" | "kt" | "docx" | "xlsx" | "pdf"
                    ),
                    AppMode::Office => matches!(
                        e.as_str(),
                        "docx" | "xlsx" | "pdf" | "rs" | "py" | "js" | "ts" | "md" | "toml"
                            | "json" | "txt" | "html" | "css"
                    ),
                };
                if keep {
                    out.push(path);
                }
            }
        }
        if out.len() > 150 {
            break;
        }
    }
    out.sort();
    out
}

fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.display().to_string()])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}
