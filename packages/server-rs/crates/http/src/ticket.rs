use opencode_contract::generated::PtyTicketConnectToken;

use crate::Clock;
use crate::expiry::ExpiringMap;

pub const TICKET_TTL_SECONDS: u64 = 60;

const CAPACITY: usize = 10_000;
const TTL_MS: u64 = TICKET_TTL_SECONDS * 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketScope {
    pub target: String,
    pub directory: String,
    pub workspace_id: Option<String>,
}

pub struct TicketStore {
    tickets: ExpiringMap<TicketScope>,
}

impl TicketStore {
    pub fn new() -> Self {
        Self::with_clock(crate::clock())
    }

    pub fn with_clock(clock: Clock) -> Self {
        Self { tickets: ExpiringMap::new(CAPACITY, TTL_MS, clock) }
    }

    pub fn issue(&self, scope: TicketScope) -> PtyTicketConnectToken {
        let ticket = opencode_local::id::v4();
        self.tickets.insert(ticket.clone(), scope);
        PtyTicketConnectToken { ticket, expires_in: TICKET_TTL_SECONDS as f64 }
    }

    pub fn consume(&self, scope: &TicketScope, ticket: &str) -> bool {
        self.tickets.take(ticket, |stored| stored == scope)
    }
}

impl Default for TicketStore {
    fn default() -> Self {
        Self::new()
    }
}
