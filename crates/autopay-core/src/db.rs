/// Local SQLite database for subscriptions and payment history.
/// Uses rusqlite (bundled, no external dependency).
/// Future: replace with sqlcipher for encrypted-at-rest DB.
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use thiserror::Error;
use super::models::{PaymentRecord, PaymentStatus, Subscription, SubscriptionStatus};

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Data serialization error: {0}")]
    Serialization(String),
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), DbError> {
        self.conn.execute_batch(include_str!("schema.sql"))?;
        // Columns added after the first release: schema.sql only creates missing tables.
        let has_website: bool = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('subscriptions') WHERE name = 'website_url'",
            [], |r| r.get::<_, i64>(0),
        )? > 0;
        if !has_website {
            self.conn.execute_batch("ALTER TABLE subscriptions ADD COLUMN website_url TEXT;")?;
        }
        self.complete_exhausted()?;
        Ok(())
    }

    // --- Subscriptions ---

    pub fn insert_subscription(&self, sub: &Subscription) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO subscriptions (
                id, wallet_id, label, description, destination_address,
                payment_network, amount, currency, schedule, number_of_payments,
                payments_sent, next_payment_at, status, grace_period_hours,
                canary_url, merchant_pubkey, last_canary_status, canary_last_ok,
                webhook_url, created_at, updated_at, raw_code, notes, website_url
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)",
            params![
                sub.id,
                sub.wallet_id,
                sub.label,
                sub.description,
                sub.destination_address,
                sub.payment_network,
                sub.amount,
                sub.currency,
                sub.schedule,
                sub.number_of_payments as i64,
                sub.payments_sent as i64,
                sub.next_payment_at.map(|t| t.to_rfc3339()),
                sub.status.to_string(),
                sub.grace_period_hours as i64,
                sub.canary_url,
                sub.merchant_pubkey,
                sub.last_canary_status,
                sub.canary_last_ok.map(|t| t.to_rfc3339()),
                sub.webhook_url,
                sub.created_at.to_rfc3339(),
                sub.updated_at.to_rfc3339(),
                sub.raw_code,
                sub.notes,
                sub.website_url,
            ],
        )?;
        Ok(())
    }

    pub fn get_subscription(&self, id: &str) -> Result<Option<Subscription>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM subscriptions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_subscription(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM subscriptions ORDER BY next_payment_at ASC NULLS LAST")?;
        let subs = stmt
            .query_map([], |row| Ok(row_to_subscription(row)))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(subs)
    }

    /// Mark subscriptions whose planned payments have all been sent as completed.
    /// Not a user change: `updated_at` is left alone.
    pub fn complete_exhausted(&self) -> Result<usize, DbError> {
        Ok(self.conn.execute(
            "UPDATE subscriptions SET status = 'completed', next_payment_at = NULL
             WHERE status IN ('active', 'paused', 'past_due', 'pending')
               AND number_of_payments > 0 AND payments_sent >= number_of_payments",
            [],
        )?)
    }

    /// Terms and details edited by the user. `updated_at` only moves on user changes
    /// (edit, pause / resume), never on payments, postpones or canary checks.
    pub fn update_subscription_terms(&self, sub: &Subscription) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE subscriptions SET label = ?1, description = ?2, website_url = ?3,
                destination_address = ?4, amount = ?5, currency = ?6, schedule = ?7,
                number_of_payments = ?8, next_payment_at = ?9, canary_url = ?10, updated_at = ?11,
                status = ?12
             WHERE id = ?13",
            params![
                sub.label,
                sub.description,
                sub.website_url,
                sub.destination_address,
                sub.amount,
                sub.currency,
                sub.schedule,
                sub.number_of_payments as i64,
                sub.next_payment_at.map(|t| t.to_rfc3339()),
                sub.canary_url,
                sub.updated_at.to_rfc3339(),
                sub.status.to_string(),
                sub.id,
            ],
        )?;
        Ok(())
    }

    pub fn update_subscription_status(
        &self,
        id: &str,
        status: &SubscriptionStatus,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE subscriptions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.to_string(), Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn update_next_payment(
        &self,
        id: &str,
        next: Option<DateTime<Utc>>,
        payments_sent: u64,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE subscriptions SET next_payment_at = ?1, payments_sent = ?2 WHERE id = ?3",
            params![
                next.map(|t| t.to_rfc3339()),
                payments_sent as i64,
                id
            ],
        )?;
        Ok(())
    }

    /// Update only the next_payment_at field by row id (used by frontend scheduler).
    pub fn update_subscription_next_payment(&self, id: &str, next_payment_at: &str) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE subscriptions SET next_payment_at = ?1 WHERE id = ?2",
            rusqlite::params![next_payment_at, id],
        )?;
        Ok(())
    }

    pub fn update_canary_status(
        &self,
        id: &str,
        status_str: &str,
        last_ok: Option<DateTime<Utc>>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE subscriptions SET last_canary_status = ?1, canary_last_ok = ?2 WHERE id = ?3",
            params![
                status_str,
                last_ok.map(|t| t.to_rfc3339()),
                id
            ],
        )?;
        Ok(())
    }

    pub fn delete_subscription(&self, id: &str) -> Result<(), DbError> {
        self.conn.execute("DELETE FROM subscriptions WHERE id = ?1", params![id])?;
        Ok(())
    }

    // --- Payment records ---

    pub fn insert_payment(&self, p: &PaymentRecord) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO payments (id, subscription_id, wallet_id, destination_address, amount_atomic, currency, tx_id, status, error_message, initiated_at, confirmed_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                p.id,
                p.subscription_id,
                p.wallet_id,
                p.destination_address,
                p.amount_atomic as i64,
                p.currency,
                p.tx_id,
                serde_json::to_string(&p.status).unwrap_or_default(),
                p.error_message,
                p.initiated_at.to_rfc3339(),
                p.confirmed_at.map(|t| t.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn list_payments_for_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<Vec<PaymentRecord>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM payments WHERE subscription_id = ?1 ORDER BY initiated_at DESC",
        )?;
        let records = stmt
            .query_map(params![subscription_id], |row| Ok(row_to_payment(row)))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }
}

fn row_to_subscription(row: &rusqlite::Row) -> Result<Subscription, rusqlite::Error> {
    let status_str: String = row.get("status")?;
    let status = match status_str.as_str() {
        "active" => SubscriptionStatus::Active,
        "paused" => SubscriptionStatus::Paused,
        "past_due" => SubscriptionStatus::PastDue,
        "cancelled" => SubscriptionStatus::Cancelled,
        "expired" => SubscriptionStatus::Expired,
        "pending" => SubscriptionStatus::Pending,
        "suspended" => SubscriptionStatus::Suspended,
        "completed" => SubscriptionStatus::Completed,
        _ => SubscriptionStatus::Active,
    };

    let parse_dt = |s: Option<String>| -> Option<DateTime<Utc>> {
        s.and_then(|s| s.parse().ok())
    };

    Ok(Subscription {
        id: row.get("id")?,
        wallet_id: row.get("wallet_id")?,
        label: row.get("label")?,
        description: row.get("description")?,
        destination_address: row.get("destination_address")?,
        payment_network: row.get("payment_network")?,
        amount: row.get("amount")?,
        currency: row.get("currency")?,
        schedule: row.get("schedule")?,
        number_of_payments: row.get::<_, i64>("number_of_payments")? as u64,
        payments_sent: row.get::<_, i64>("payments_sent")? as u64,
        next_payment_at: parse_dt(row.get("next_payment_at")?),
        status,
        grace_period_hours: row.get::<_, i64>("grace_period_hours")? as u32,
        canary_url: row.get("canary_url")?,
        merchant_pubkey: row.get("merchant_pubkey")?,
        last_canary_status: row.get("last_canary_status")?,
        canary_last_ok: parse_dt(row.get("canary_last_ok")?),
        webhook_url: row.get("webhook_url")?,
        created_at: row.get::<_, String>("created_at")?.parse().unwrap_or_else(|_| Utc::now()),
        updated_at: row.get::<_, String>("updated_at")?.parse().unwrap_or_else(|_| Utc::now()),
        raw_code: row.get("raw_code")?,
        notes: row.get("notes")?,
        website_url: row.get("website_url")?,
    })
}

fn row_to_payment(row: &rusqlite::Row) -> Result<PaymentRecord, rusqlite::Error> {
    let status_str: String = row.get("status")?;
    let status: PaymentStatus = serde_json::from_str(&format!("\"{}\"", status_str))
        .unwrap_or(PaymentStatus::Failed);
    let parse_dt = |s: Option<String>| -> Option<DateTime<Utc>> {
        s.and_then(|s| s.parse().ok())
    };
    Ok(PaymentRecord {
        id: row.get("id")?,
        subscription_id: row.get("subscription_id")?,
        wallet_id: row.get("wallet_id")?,
        destination_address: row.get("destination_address")?,
        amount_atomic: row.get::<_, i64>("amount_atomic")? as u64,
        currency: row.get("currency")?,
        tx_id: row.get("tx_id")?,
        status,
        error_message: row.get("error_message")?,
        initiated_at: row.get::<_, String>("initiated_at")?.parse().unwrap_or_else(|_| Utc::now()),
        confirmed_at: parse_dt(row.get("confirmed_at")?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(id: &str, planned: u64, sent: u64) -> Subscription {
        let now = Utc::now();
        Subscription {
            id: id.into(), wallet_id: "default".into(), label: "t".into(), description: None,
            website_url: None, destination_address: "4x".into(), payment_network: "monero".into(),
            amount: "0.01".into(), currency: "XMR".into(), schedule: "0 0 9 1 * *".into(),
            number_of_payments: planned, payments_sent: sent, next_payment_at: Some(now),
            status: SubscriptionStatus::Active, grace_period_hours: 24, canary_url: None,
            merchant_pubkey: None, last_canary_status: None, canary_last_ok: None, webhook_url: None,
            created_at: now, updated_at: now, raw_code: None, notes: None,
        }
    }

    #[test]
    fn exhausted_subscriptions_complete() {
        let db = Database::open_in_memory().unwrap();
        db.insert_subscription(&sub("done", 3, 3)).unwrap();
        db.insert_subscription(&sub("over", 3, 20)).unwrap();
        db.insert_subscription(&sub("left", 3, 2)).unwrap();
        db.insert_subscription(&sub("unlimited", 0, 50)).unwrap();
        assert_eq!(db.complete_exhausted().unwrap(), 2);
        let subs = db.list_subscriptions().unwrap();
        let get = |id: &str| subs.iter().find(|s| s.id == id).unwrap().clone();
        assert_eq!(get("done").status, SubscriptionStatus::Completed);
        assert!(get("over").next_payment_at.is_none());
        assert_eq!(get("left").status, SubscriptionStatus::Active);
        assert_eq!(get("unlimited").status, SubscriptionStatus::Active);
    }
}
