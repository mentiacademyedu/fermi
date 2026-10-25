//! The time the engine waits on between attempts. Real by default; a test
//! swaps in a manual clock, whose sleeps return at once, advance its time
//! and are recorded.

use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub enum Clock {
    #[default]
    Real,
    Manual(Arc<Mutex<Manual>>),
}

#[derive(Debug, Default)]
pub struct Manual {
    /// Seconds since the clock was made.
    pub now: u64,
    /// Every sleep, in seconds, in order.
    pub sleeps: Vec<u64>,
}

impl Clock {
    pub fn manual() -> Clock {
        Clock::Manual(Arc::default())
    }

    pub async fn sleep(&self, secs: u64) {
        match self {
            Clock::Real => tokio::time::sleep(std::time::Duration::from_secs(secs)).await,
            Clock::Manual(m) => {
                let mut m = m.lock().unwrap();
                m.now += secs;
                m.sleeps.push(secs);
            }
        }
    }

    /// Unix seconds, or a manual clock's own time.
    pub fn now(&self) -> u64 {
        match self {
            Clock::Real => crate::util::now(),
            Clock::Manual(m) => m.lock().unwrap().now,
        }
    }

    pub fn sleeps(&self) -> Vec<u64> {
        match self {
            Clock::Real => vec![],
            Clock::Manual(m) => m.lock().unwrap().sleeps.clone(),
        }
    }
}
