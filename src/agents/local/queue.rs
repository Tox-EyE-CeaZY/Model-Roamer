use anyhow::{bail, Result};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};

#[derive(Default)]
pub(crate) struct Queue {
    state: Mutex<State>,
    change: Notify,
}

#[derive(Default)]
struct State {
    next: u64,
    active: bool,
    last_user: String,
    waiting: VecDeque<(String, VecDeque<u64>)>,
    recent: HashMap<String, VecDeque<Instant>>,
}

const MAX_RATE_WINDOW: Duration = Duration::from_secs(86_400);

impl Queue {
    pub(super) fn enter(
        self: &Arc<Self>,
        user: &str,
        rate: Option<(&str, usize, Duration)>,
    ) -> Result<Ticket> {
        let mut state = self.state.lock().unwrap();
        if state
            .waiting
            .iter()
            .map(|(_, tickets)| tickets.len())
            .sum::<usize>()
            >= 128
        {
            bail!("The local model queue is full. Try again shortly.");
        }
        if let Some((membership, limit, window)) = rate {
            let now = Instant::now();
            state.recent.retain(|_, turns| {
                turns.retain(|time| now.duration_since(*time) < MAX_RATE_WINDOW);
                !turns.is_empty()
            });
            let turns = state.recent.entry(membership.into()).or_default();
            let used = turns
                .iter()
                .rev()
                .take_while(|time| now.duration_since(**time) < window)
                .count();
            if limit == 0 {
                bail!("The owner has disabled shared local-model turns.");
            }
            if used >= limit {
                let oldest = turns[turns.len() - limit];
                let seconds = window.saturating_sub(now.duration_since(oldest)).as_secs() + 1;
                bail!("Local model rate limit reached. Try again in {seconds} seconds.");
            }
            turns.push_back(now);
        }
        let id = state.next;
        state.next += 1;
        let queued = state.active || !state.waiting.is_empty();
        if let Some((_, tickets)) = state.waiting.iter_mut().find(|(u, _)| u == user) {
            tickets.push_back(id);
        } else {
            state.waiting.push_back((user.into(), VecDeque::from([id])));
        }
        self.change.notify_waiters();
        Ok(Ticket {
            queue: self.clone(),
            id,
            queued,
        })
    }
}

pub(super) struct Ticket {
    queue: Arc<Queue>,
    id: u64,
    pub(super) queued: bool,
}

impl Ticket {
    pub(super) async fn acquire(self) -> Permit {
        loop {
            let notified = self.queue.change.notified();
            {
                let mut state = self.queue.state.lock().unwrap();
                if !state.active {
                    // A user arriving during someone else's turn gets a chance before that
                    // same person's backlog, even if it was the only queue when they started.
                    let next = state
                        .waiting
                        .iter()
                        .position(|(user, _)| *user != state.last_user)
                        .unwrap_or(0);
                    if state
                        .waiting
                        .get(next)
                        .and_then(|(_, tickets)| tickets.front())
                        == Some(&self.id)
                    {
                        state.waiting.rotate_left(next);
                        let (user, mut tickets) = state.waiting.pop_front().unwrap();
                        tickets.pop_front();
                        state.active = true;
                        state.last_user = user.clone();
                        if !tickets.is_empty() {
                            state.waiting.push_back((user, tickets));
                        }
                        return Permit(self.queue.clone());
                    }
                }
            }
            notified.await;
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock().unwrap();
        state.waiting.retain_mut(|(_, tickets)| {
            tickets.retain(|id| *id != self.id);
            !tickets.is_empty()
        });
        self.queue.change.notify_waiters();
    }
}

pub(crate) struct Permit(Arc<Queue>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active = false;
        self.0.change.notify_waiters();
    }
}
