//! Session status and the discard policy (FR-20, FR-30, FR-31).

use std::time::{Duration, Instant};

/// Live activity of a session, from hook events. Not persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Activity {
    #[default]
    Idle,
    Working,
    NeedsYou,
}

/// What the sidebar shows for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Working,
    NeedsYou,
    Unread,
    Idle,
    Discarded,
}

pub fn status(activity: Activity, unread: bool, live: bool) -> Status {
    match (live, activity) {
        (true, Activity::NeedsYou) => Status::NeedsYou,
        (true, Activity::Working) => Status::Working,
        _ if unread => Status::Unread,
        (false, _) => Status::Discarded,
        _ => Status::Idle,
    }
}

pub const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub fn glyph(s: Status, frame: usize) -> &'static str {
    match s {
        Status::Working => SPINNER[frame % SPINNER.len()],
        Status::NeedsYou => "!",
        Status::Unread => "●",
        Status::Discarded => "◌",
        Status::Idle => " ",
    }
}

/// Inputs for the discard decision, one per live thread.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub open: bool,
    pub activity: Activity,
    /// Last time the thread was viewed (opened/focused).
    pub last_viewed: Instant,
    /// Last time anything happened (viewed, status change, output burst).
    pub last_active: Instant,
}

/// Which live threads to discard now. Never the open thread, never Working or
/// Needs-you. First anything idle for `discard_after`; then, while more than
/// `max_live` remain, the least recently viewed.
pub fn choose_discards(live: &[Candidate], now: Instant, discard_after: Duration, max_live: usize) -> Vec<String> {
    let eligible = |c: &&Candidate| !c.open && c.activity == Activity::Idle;
    let mut out: Vec<String> = live
        .iter()
        .filter(eligible)
        .filter(|c| now.saturating_duration_since(c.last_active) >= discard_after)
        .map(|c| c.id.clone())
        .collect();
    let mut remaining = live.len() - out.len();
    if remaining > max_live {
        let mut rest: Vec<&Candidate> = live.iter().filter(eligible).filter(|c| !out.contains(&c.id)).collect();
        rest.sort_by_key(|c| c.last_viewed);
        for c in rest {
            if remaining <= max_live {
                break;
            }
            out.push(c.id.clone());
            remaining -= 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, open: bool, a: Activity, viewed_ago: u64, active_ago: u64, now: Instant) -> Candidate {
        Candidate {
            id: id.into(),
            open,
            activity: a,
            last_viewed: now - Duration::from_secs(viewed_ago),
            last_active: now - Duration::from_secs(active_ago),
        }
    }

    const TEN_MIN: Duration = Duration::from_secs(600);

    #[test]
    fn idle_timeout() {
        let now = Instant::now() + Duration::from_secs(10_000);
        let live = vec![
            c("old", false, Activity::Idle, 900, 700, now),
            c("fresh", false, Activity::Idle, 900, 60, now),
            c("busy", false, Activity::Working, 900, 900, now),
            c("asks", false, Activity::NeedsYou, 900, 900, now),
            c("open", true, Activity::Idle, 900, 900, now),
        ];
        assert_eq!(choose_discards(&live, now, TEN_MIN, 5), vec!["old"]);
    }

    #[test]
    fn max_live_lru() {
        let now = Instant::now() + Duration::from_secs(10_000);
        let live = vec![
            c("a", false, Activity::Idle, 50, 10, now),
            c("b", false, Activity::Idle, 500, 10, now),
            c("c", false, Activity::Idle, 300, 10, now),
            c("d", true, Activity::Idle, 900, 10, now),
            c("e", false, Activity::Working, 999, 10, now),
        ];
        // 5 live, max 3: discard the two least recently viewed eligible ones
        assert_eq!(choose_discards(&live, now, TEN_MIN, 3), vec!["b", "c"]);
        // can't go below what's protected
        assert_eq!(choose_discards(&live, now, TEN_MIN, 1), vec!["b", "c", "a"]);
        assert!(choose_discards(&live, now, TEN_MIN, 5).is_empty());
    }

    #[test]
    fn statuses() {
        assert_eq!(status(Activity::Working, true, true), Status::Working);
        assert_eq!(status(Activity::NeedsYou, false, true), Status::NeedsYou);
        assert_eq!(status(Activity::Idle, true, true), Status::Unread);
        assert_eq!(status(Activity::Idle, true, false), Status::Unread);
        assert_eq!(status(Activity::Working, false, false), Status::Discarded);
        assert_eq!(status(Activity::Idle, false, true), Status::Idle);
        assert_eq!(glyph(Status::NeedsYou, 0), "!");
    }
}
