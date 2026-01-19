//! TUI - Terminal User Interface for Evil Validator
//!
//! Real-time visualization of the 51% attack in progress

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use node_lib::{BLOCKCHAIN, NODES};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Sparkline},
};
use tokio::sync::RwLock;

use crate::attack_coordinator::AttackCoordinator;
use crate::cli::AttackMode;
use crate::evil_proposer::{EvilProposer, EvilStats};
use crate::shadow_chain::ShadowChain;

/// Application state for the TUI
pub struct TuiApp {
    pub proposer: Arc<EvilProposer>,
    pub coordinator: Option<Arc<AttackCoordinator>>,
    pub attack_mode: AttackMode,
    pub evil_id: u8,
    pub logs: Arc<RwLock<Vec<String>>>,
    pub running: bool,
}

impl TuiApp {
    pub fn new(
        proposer: Arc<EvilProposer>,
        coordinator: Option<Arc<AttackCoordinator>>,
        attack_mode: AttackMode,
        evil_id: u8,
    ) -> Self {
        Self {
            proposer,
            coordinator,
            attack_mode,
            evil_id,
            logs: Arc::new(RwLock::new(Vec::new())),
            running: true,
        }
    }

    pub async fn add_log(&self, msg: String) {
        let mut logs = self.logs.write().await;
        logs.push(format!(
            "[{}] {}",
            chrono::Utc::now().format("%H:%M:%S"),
            msg
        ));
        if logs.len() > 100 {
            logs.remove(0);
        }
    }
}

/// Initialize and run the TUI
pub async fn run_tui(app: Arc<RwLock<TuiApp>>) -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Main loop
    loop {
        let app_guard = app.read().await;
        if !app_guard.running {
            drop(app_guard);
            break;
        }
        drop(app_guard);

        // Draw UI
        {
            let app_guard = app.read().await;
            terminal.draw(|f| draw_ui(f, &app_guard))?;
        }

        // Handle input (non-blocking)
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => {
                            let mut app_guard = app.write().await;
                            app_guard.running = false;
                        }
                        KeyCode::Char('r') => {
                            // Manual release trigger
                            let app_guard = app.read().await;
                            let _ = app_guard.proposer.release_attack().await;
                            app_guard
                                .add_log("Manual release triggered!".to_string())
                                .await;
                        }
                        KeyCode::Char('i') => {
                            // Reinitialize shadow chain
                            let app_guard = app.read().await;
                            app_guard.proposer.init_shadow_chain().await;
                            app_guard
                                .add_log("Shadow chain reinitialized".to_string())
                                .await;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

/// Draw the main UI
fn draw_ui(f: &mut Frame, app: &TuiApp) {
    // Create main layout
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main content
            Constraint::Length(8), // Logs
            Constraint::Length(3), // Status bar
        ])
        .split(f.area());

    // Draw header
    draw_header(f, chunks[0], app);

    // Draw main content (split into left and right)
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    draw_public_chain(f, main_chunks[0]);
    draw_shadow_chain(f, main_chunks[1], app);

    // Draw logs
    draw_logs(f, chunks[2], app);

    // Draw status bar
    draw_status_bar(f, chunks[3], app);
}

/// Draw the header with attack mode and evil ID
fn draw_header(f: &mut Frame, area: Rect, app: &TuiApp) {
    let mode_str = match app.attack_mode {
        AttackMode::PrivateChain => "PRIVATE CHAIN",
        AttackMode::DoubleSpend => "DOUBLE SPEND",
        AttackMode::SelfishMining => "SELFISH MINING",
        AttackMode::Observer => "OBSERVER",
    };

    let header_text = format!(
        "👿 EVIL VALIDATOR #{} | Mode: {} | Press 'r' to release, 'i' to reinit, 'q' to quit",
        app.evil_id, mode_str
    );

    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("51% Attack Simulator"),
        );

    f.render_widget(header, area);
}

/// Draw the public blockchain view
fn draw_public_chain(f: &mut Frame, area: Rect) {
    // We need to use futures::executor::block_on or similar since we're in a sync context
    // For simplicity, we'll show placeholder - in real impl would need async handling

    let block = Block::default()
        .title("📦 Public Blockchain")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Green));

    // Get blockchain info synchronously (this is a limitation of ratatui's sync draw)
    let content = "Loading blockchain state...\n\nBlocks are shown in green.\nEvil blocks would be shown in red.";

    let paragraph = Paragraph::new(content)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(paragraph, area);
}

/// Draw the shadow chain (private blocks)
fn draw_shadow_chain(f: &mut Frame, area: Rect, app: &TuiApp) {
    let block = Block::default()
        .title("🌑 Shadow Chain (Private)")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Rgb(255, 165, 0))); // Orange

    let content = format!(
        "Secret blocks ready to release:\n\n\
         Press 'r' to manually release attack\n\
         Press 'i' to reinitialize shadow chain\n\n\
         Attack mode: {:?}\n\
         Evil ID: #{}",
        app.attack_mode, app.evil_id
    );

    let paragraph = Paragraph::new(content)
        .block(block)
        .style(Style::default().fg(Color::Yellow));

    f.render_widget(paragraph, area);
}

/// Draw the logs panel
fn draw_logs(f: &mut Frame, area: Rect, app: &TuiApp) {
    let block = Block::default()
        .title("📜 Attack Logs")
        .borders(Borders::ALL);

    // We can't easily access async data here, show placeholder
    let items: Vec<ListItem> = vec![ListItem::new("Waiting for events...")];

    let list = List::new(items)
        .block(block)
        .style(Style::default().fg(Color::Cyan));

    f.render_widget(list, area);
}

/// Draw the status bar
fn draw_status_bar(f: &mut Frame, area: Rect, app: &TuiApp) {
    let connected = if app.coordinator.is_some() {
        "🔗 Partner: Configured"
    } else {
        "⚡ Solo Mode"
    };

    let status = format!(
        "Evil #{} | {} | Nodes: checking... | [q]uit [r]elease [i]nit",
        app.evil_id, connected
    );

    let status_bar = Paragraph::new(status)
        .style(Style::default().fg(Color::White).bg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(status_bar, area);
}

/// Simple non-TUI output mode
pub async fn run_simple_output(app: Arc<RwLock<TuiApp>>) {
    println!("═══════════════════════════════════════════════");
    println!("   👿 Evil Validator - Simple Output Mode      ");
    println!("═══════════════════════════════════════════════");

    loop {
        let app_guard = app.read().await;
        if !app_guard.running {
            break;
        }
        drop(app_guard);

        // Print status periodically
        tokio::time::sleep(Duration::from_secs(5)).await;

        let app_guard = app.read().await;
        let stats = app_guard.proposer.get_stats().await;
        let shadow = app_guard.proposer.shadow_chain.read().await;

        println!("\n--- Evil Status ---");
        println!("Shadow chain length: {}", shadow.len());
        println!("Blocks mined secretly: {}", stats.blocks_mined_secretly);
        println!("Blocks released: {}", stats.blocks_released);
        println!("Attack releases: {}", stats.attack_releases);
        println!("-------------------\n");
    }
}
