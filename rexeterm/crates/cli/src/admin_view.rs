//! Five independent operations pages. Rendering consumes one server snapshot.
use crate::protocol::management::{ChannelKind, ManagementSnapshot, SessionOwner, TrafficSummary};
use std::{collections::BTreeMap, fmt::Write};
#[derive(Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum Page {
    #[value(alias = "status")]
    Clients,
    Programs,
    Pty,
    Files,
    Forwards,
}
impl Page {
    pub const ALL: [Self; 5] = [
        Self::Clients,
        Self::Programs,
        Self::Pty,
        Self::Files,
        Self::Forwards,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Clients => "Clients",
            Self::Programs => "Programs",
            Self::Pty => "PTY",
            Self::Files => "Files",
            Self::Forwards => "Forwards",
        }
    }
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap()
    }
    pub fn next(self, reverse: bool) -> Self {
        Self::ALL[(self.index() + if reverse { 4 } else { 1 }) % 5]
    }
}
fn owner(o: Option<&SessionOwner>) -> String {
    o.map(|o| {
        format!(
            "user={} client={} clientId={} session={}",
            o.username, o.client_name, o.client_id, o.session_id
        )
    })
    .unwrap_or_else(|| "owner=unassigned".into())
}
fn rate(t: &TrafficSummary) -> String {
    format!(
        "RX {:.3} MB/s  TX {:.3} MB/s  TOTAL {:.3} MB/s",
        t.receive_bytes_per_second as f64 / 1_000_000.0,
        t.send_bytes_per_second as f64 / 1_000_000.0,
        (t.receive_bytes_per_second
            .saturating_add(t.send_bytes_per_second)) as f64
            / 1_000_000.0
    )
}
/// UTC civil date conversion, with milliseconds kept in the protocol.
fn timestamp(ms: u64) -> String {
    let seconds = (ms / 1000) as i64;
    let z = seconds / 86400 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let rem = seconds % 86400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
pub fn render(snapshot: &ManagementSnapshot, page: Page) -> String {
    let mut out = format!(
        "rexctl | {} | {}\n",
        snapshot.executor_id,
        timestamp(snapshot.observed_at_ms)
    );
    for (i, p) in Page::ALL.iter().enumerate() {
        let _ = write!(
            out,
            "{}{} {}{}  ",
            if *p == page { "[" } else { " " },
            i + 1,
            p.title(),
            if *p == page { "]" } else { " " }
        );
    }
    out.push_str("\n\n");
    match page {
        Page::Clients => {
            let mut users: BTreeMap<&str, Vec<_>> = BTreeMap::new();
            for c in &snapshot.clients {
                users.entry(&c.owner.username).or_default().push(c);
            }
            let _ = writeln!(
                out,
                "{} clients / {} users | server RX/TX across all associated links",
                snapshot.clients.len(),
                users.len()
            );
            for (username, mut clients) in users {
                clients.sort_by(|a, b| {
                    (&a.owner.client_name, &a.owner.session_id)
                        .cmp(&(&b.owner.client_name, &b.owner.session_id))
                });
                let mut traffic = TrafficSummary::default();
                for c in &clients {
                    traffic.receive_bytes_per_second = traffic
                        .receive_bytes_per_second
                        .saturating_add(c.traffic.receive_bytes_per_second);
                    traffic.send_bytes_per_second = traffic
                        .send_bytes_per_second
                        .saturating_add(c.traffic.send_bytes_per_second);
                }
                let _ = writeln!(out, "{username}  {}", rate(&traffic));
                let len = clients.len();
                for (i, c) in clients.into_iter().enumerate() {
                    let _ = writeln!(
                        out,
                        "{} {}  {}  links={}{}",
                        if i + 1 == len { "└─" } else { "├─" },
                        c.owner.client_name,
                        rate(&c.traffic),
                        c.active_links,
                        if c.traffic_complete {
                            ""
                        } else {
                            " (partial counters)"
                        }
                    );
                    let _ = writeln!(
                        out,
                        "   clientId={}  session={}  peer={}  created={}",
                        c.owner.client_id,
                        c.owner.session_id,
                        c.peer.escape_debug(),
                        timestamp(c.created_at_ms)
                    );
                }
            }
        }
        Page::Programs => {
            let _ = writeln!(
                out,
                "{} programs maintained by rexed (running and retained exits)",
                snapshot.programs.len()
            );
            let mut programs: Vec<_> = snapshot.programs.iter().collect();
            programs.sort_by_key(|p| (p.state != "running", std::cmp::Reverse(p.created_at_ms)));
            for p in programs {
                let _ = writeln!(
                    out,
                    "{}  title={}  pid={:?}  {}  command={:?}",
                    p.program_id,
                    p.title.escape_debug(),
                    p.pid,
                    p.state,
                    p.command
                );
                let _ = writeln!(out, "  description={}", p.description.escape_debug());
                let _ = writeln!(
                    out,
                    "  created={}  {}",
                    timestamp(p.created_at_ms),
                    owner(p.created_by.as_ref())
                );
                let _ = writeln!(
                    out,
                    "  master={:?}  {}  exit={:?}",
                    p.controller,
                    owner(p.controller_session.as_ref()),
                    p.exit_code
                );
            }
        }
        page => {
            let kind = match page {
                Page::Pty => ChannelKind::Pty,
                Page::Files => ChannelKind::File,
                _ => ChannelKind::Forward,
            };
            let channels: Vec<_> = snapshot
                .channels
                .iter()
                .filter(|c| c.kind == kind)
                .collect();
            let _ = writeln!(
                out,
                "{} active {} channels maintained by rexed",
                channels.len(),
                page.title()
            );
            for c in channels {
                if kind != ChannelKind::Pty {
                    let _ = writeln!(
                        out,
                        "  title={}  description={}",
                        c.metadata.title.escape_debug(),
                        c.metadata.description.escape_debug()
                    );
                }
                let _ = writeln!(
                    out,
                    "{}  {}  target={}",
                    c.channel_id,
                    c.transport,
                    c.target.escape_debug()
                );
                let _ = writeln!(
                    out,
                    "  {}  peer={}  created={}",
                    owner(c.owner.as_ref()),
                    c.peer.escape_debug(),
                    timestamp(c.created_at_ms)
                );
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_dates_cover_epoch_leap_day_and_year_boundary() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(timestamp(1709164800000), "2024-02-29 00:00:00 UTC");
        assert_eq!(timestamp(1640995200000), "2022-01-01 00:00:00 UTC");
    }
}
