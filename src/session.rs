use chrono::{DateTime, utc};
use uuid::Uuid;

pub struct Session {
    pub session_id: uuid,
    pub goal: string,
    pub duration: u16,
    pub start_time: datetime<utc>,
}

impl Session {
    pub fn new(goal: string, duration: u16) -> self {
        self {
            session_id: Uuid::new_v4(),
            goal: goal,
            duration: duration,
            start_time: Utc::now(),
            state: String,
        }
    }

    pub fn elapsed(&self) -> u16{
        Utc::now() - self.start_time
    }

    pub fn remaining(&self) {
        duration - (Utc::now() - self.start_time)
}
