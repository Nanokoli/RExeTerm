use super::{select, Args, Page};
use anyhow::Result;
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use rexeterm::{admin_view, client::management::ManagementClient};
use std::{
    io::{stdout, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
struct Terminal {
    events: Arc<AtomicBool>,
    refresh: tokio::task::JoinHandle<()>,
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.events.store(false, Ordering::Relaxed);
        self.refresh.abort();
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), Show, LeaveAlternateScreen);
    }
}
fn truncate(line: &str, width: u16) -> String {
    let mut used = 0;
    line.chars()
        .take_while(|c| {
            used += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            used <= usize::from(width)
        })
        .collect()
}
pub(super) async fn run(mut client: ManagementClient, args: Args) -> Result<()> {
    let (tx, mut snapshots) = tokio::sync::mpsc::channel(1);
    let delay = Duration::from_secs(args.interval);
    let refresh = tokio::spawn(async move {
        loop {
            let result = client.snapshot().await.map_err(|e| e.to_string());
            let failed = result.is_err();
            if tx.send(result).await.is_err() || failed {
                break;
            }
            tokio::time::sleep(delay).await;
        }
    });
    let running = Arc::new(AtomicBool::new(true));
    let _guard = Terminal {
        events: running.clone(),
        refresh,
    };
    terminal::enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, Hide)?;
    let (event_tx, mut events) = tokio::sync::mpsc::channel(32);
    tokio::task::spawn_blocking(move || {
        while running.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(e) => {
                        let _ = event_tx.try_send(e);
                    }
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
    let mut page = args.page.unwrap_or(Page::Clients);
    let mut offset = 0usize;
    let mut latest = None;
    let mut error = None;
    loop {
        let mut output = stdout();
        let (width, height) = terminal::size()?;
        queue!(output, MoveTo(0, 0), Clear(ClearType::All))?;
        let text = latest
            .as_ref()
            .map(|s| admin_view::render(s, page))
            .unwrap_or_else(|| "Connecting to rexed...\n".into());
        let lines: Vec<_> = text.lines().collect();
        let available = usize::from(height.saturating_sub(2));
        for (row, line) in lines
            .iter()
            .take(3)
            .chain(lines.iter().skip(3 + offset))
            .take(available)
            .enumerate()
        {
            queue!(output, MoveTo(0, row as u16))?;
            write!(output, "{}", truncate(line, width))?;
        }
        queue!(output, MoveTo(0, height.saturating_sub(1)))?;
        let footer = error.as_deref().unwrap_or(
            "1-5 / Tab / Left-Right: page | Up-Down / PgUp-PgDn: scroll | q / Ctrl-C: quit",
        );
        write!(output, "{}", truncate(footer, width))?;
        output.flush()?;
        tokio::select! {
            Some(next)=snapshots.recv()=>match next {Ok(mut s)=>{select(&mut s,&args);latest=Some(s);error=None},Err(e)=>error=Some(e)},
            Some(e)=events.recv()=>if let Event::Key(key)=e {if key.kind==KeyEventKind::Release{continue}match key.code {
                KeyCode::Char('q')|KeyCode::Esc=>break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL)=>break,
                KeyCode::Char(c @ '1'..='5')=>{page=Page::ALL[(c as u8-b'1')as usize];offset=0;},
                KeyCode::Tab|KeyCode::Right=>{page=page.next(false);offset=0;},KeyCode::BackTab|KeyCode::Left=>{page=page.next(true);offset=0;},
                KeyCode::Up=>offset=offset.saturating_sub(1),KeyCode::Down=>offset=(offset+1).min(lines.len().saturating_sub(3)),
                KeyCode::PageUp=>offset=offset.saturating_sub(available),KeyCode::PageDown=>offset=(offset+available).min(lines.len().saturating_sub(3)),_=>{},
            }},
            else=>break,
        }
    }
    Ok(())
}
