use serde::{Deserialize, Serialize};

/// Amounts are integer minor units: 1 DEAL = 100 units. No floats on-chain.
pub type Amount = u64;

pub const DECIMALS: u32 = 2;
/// Balance granted to every newly registered account (demo faucet): 10,000.00 DEAL.
pub const REGISTER_GRANT: Amount = 1_000_000;
/// Protocol treasury receiving deal fees.
pub const TREASURY_ADDRESS: &str = "0x7265617375727900000000000000000000000000";
/// Proof-of-work difficulty: number of leading hex zeros in a block hash.
pub const DEFAULT_DIFFICULTY: u32 = 4;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PartyKind {
    Consumer,
    Business,
}

/// Deal category, derived from the seller and buyer kinds (seller first).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DealType {
    C2C,
    C2B,
    B2C,
    B2B,
}

impl DealType {
    pub fn classify(seller: PartyKind, buyer: PartyKind) -> Self {
        use PartyKind::*;
        match (seller, buyer) {
            (Consumer, Consumer) => DealType::C2C,
            (Consumer, Business) => DealType::C2B,
            (Business, Consumer) => DealType::B2C,
            (Business, Business) => DealType::B2B,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DealStatus {
    Proposed,
    Accepted,
    Funded,
    Shipped,
    Completed,
    Disputed,
    Resolved,
    Cancelled,
    /// Counterparty refused the proposal.
    Declined,
    /// A deadline passed before the deal was accepted or funded (nothing ever moved,
    /// or the buyer defaulted on funding).
    Expired,
    /// Seller missed the delivery deadline on a funded deal: buyer refunded, bond slashed.
    Failed,
}

impl DealStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            DealStatus::Completed | DealStatus::Resolved | DealStatus::Cancelled
                | DealStatus::Declined | DealStatus::Expired | DealStatus::Failed
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Seller,
    Buyer,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LineItem {
    pub name: String,
    pub qty: u32,
    pub unit_price: Amount,
}

/// Everything a transaction can do. Serialized as `{"type": "...", ...}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Register {
        name: String,
        kind: PartyKind,
    },
    Transfer {
        to: String,
        amount: Amount,
        memo: String,
    },
    CreateDeal {
        role: Role,
        counterparty: String,
        title: String,
        description: String,
        items: Vec<LineItem>,
        arbiter: Option<String>,
    },
    AcceptDeal {
        deal_id: String,
    },
    FundDeal {
        deal_id: String,
    },
    MarkShipped {
        deal_id: String,
        tracking: String,
    },
    ConfirmReceipt {
        deal_id: String,
        rating: u8,
    },
    ClaimRelease {
        deal_id: String,
    },
    OpenDispute {
        deal_id: String,
        reason: String,
    },
    ResolveDispute {
        deal_id: String,
        buyer_refund_bps: u16,
        note: String,
    },
    CancelDeal {
        deal_id: String,
        reason: String,
    },
    /// Counterparty rejects a proposal.
    DeclineDeal {
        deal_id: String,
        reason: String,
    },
    /// Enforces a missed deadline (callable by either party or the arbiter).
    ExpireDeal {
        deal_id: String,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Action::Register { .. } => "register",
            Action::Transfer { .. } => "transfer",
            Action::CreateDeal { .. } => "create_deal",
            Action::AcceptDeal { .. } => "accept_deal",
            Action::FundDeal { .. } => "fund_deal",
            Action::MarkShipped { .. } => "mark_shipped",
            Action::ConfirmReceipt { .. } => "confirm_receipt",
            Action::ClaimRelease { .. } => "claim_release",
            Action::OpenDispute { .. } => "open_dispute",
            Action::ResolveDispute { .. } => "resolve_dispute",
            Action::CancelDeal { .. } => "cancel_deal",
            Action::DeclineDeal { .. } => "decline_deal",
            Action::ExpireDeal { .. } => "expire_deal",
        }
    }

    /// Deal referenced by this action, if it targets an existing deal.
    pub fn deal_id(&self) -> Option<&str> {
        match self {
            Action::AcceptDeal { deal_id }
            | Action::FundDeal { deal_id }
            | Action::MarkShipped { deal_id, .. }
            | Action::ConfirmReceipt { deal_id, .. }
            | Action::ClaimRelease { deal_id }
            | Action::OpenDispute { deal_id, .. }
            | Action::ResolveDispute { deal_id, .. }
            | Action::CancelDeal { deal_id, .. }
            | Action::DeclineDeal { deal_id, .. }
            | Action::ExpireDeal { deal_id } => Some(deal_id),
            _ => None,
        }
    }

    /// Addresses (other than the sender) this action directly references.
    pub fn referenced_addresses(&self) -> Vec<&str> {
        match self {
            Action::Transfer { to, .. } => vec![to],
            Action::CreateDeal {
                counterparty,
                arbiter,
                ..
            } => {
                let mut v = vec![counterparty.as_str()];
                if let Some(a) = arbiter {
                    v.push(a);
                }
                v
            }
            _ => vec![],
        }
    }
}

/// The signed part of a transaction. Field order is the canonical encoding.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TxBody {
    pub from: String,
    /// Hex-encoded ed25519 public key; `from` must be its derived address.
    pub pubkey: String,
    /// Random client nonce; together with the timestamp makes every hash unique.
    pub nonce: u64,
    /// Client time in milliseconds since the Unix epoch.
    pub timestamp: u64,
    pub action: Action,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SignedTx {
    pub hash: String,
    pub body: TxBody,
    pub signature: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TxStatus {
    Pending,
    Confirmed,
    Rejected,
}

/// A transaction as persisted, with its outcome.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TxRecord {
    pub hash: String,
    pub body: TxBody,
    pub signature: String,
    pub status: TxStatus,
    pub error: Option<String>,
    pub block_height: Option<u64>,
    pub deal_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub address: String,
    pub pubkey: String,
    pub name: String,
    pub kind: PartyKind,
    pub balance: Amount,
    /// Funds locked in escrow (as buyer) or as a performance bond (as seller).
    pub escrowed: Amount,
    pub created_at: u64,
    pub deals_completed: u32,
    pub disputes: u32,
    pub rating_sum: u32,
    pub rating_count: u32,
    /// Deadlines this account missed (unfunded accepted deals, undelivered funded deals).
    #[serde(default)]
    pub defaults: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DealEvent {
    pub status: DealStatus,
    pub by: String,
    pub tx_hash: String,
    pub at: u64,
    pub note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Deal {
    pub id: String,
    pub title: String,
    pub description: String,
    pub deal_type: DealType,
    pub seller: String,
    pub buyer: String,
    pub arbiter: Option<String>,
    pub proposer: String,
    pub items: Vec<LineItem>,
    pub amount: Amount,
    pub fee: Amount,
    pub bond: Amount,
    pub bond_locked: bool,
    pub status: DealStatus,
    pub tracking: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub shipped_at: Option<u64>,
    pub release_after: Option<u64>,
    pub buyer_refund_bps: Option<u16>,
    pub dispute_reason: Option<String>,
    pub history: Vec<DealEvent>,
    /// When the current step lapses (accept-by, fund-by or deliver-by); None when no deadline runs.
    #[serde(default)]
    pub deadline: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub height: u64,
    pub prev_hash: String,
    /// Seconds since the Unix epoch; the contract clock for this block.
    pub timestamp: u64,
    pub merkle_root: String,
    pub tx_count: u32,
    pub difficulty: u32,
    pub nonce: u64,
    pub producer: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub header: BlockHeader,
    pub hash: String,
    pub tx_hashes: Vec<String>,
}

/// Formats minor units as a decimal string, e.g. `123456` -> `"1234.56"`.
pub fn format_amount(a: Amount) -> String {
    let d = 10u64.pow(DECIMALS);
    format!("{}.{:02}", a / d, a % d)
}
