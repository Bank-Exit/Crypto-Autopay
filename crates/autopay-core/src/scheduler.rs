/// Subscription scheduler — fires payments at the right time.
/// Uses tokio-cron-scheduler for cron-expression support.
/// Network-agnostic: delegates actual sending to a PaymentAdapter.
use std::sync::Arc;
use chrono::Utc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use uuid::Uuid;

use autopay_rpc::{PaymentAdapter, CanaryChecker, CanaryStatus};

use super::db::Database;
use super::models::{PaymentRecord, PaymentStatus, Subscription, SubscriptionStatus};

pub struct SubscriptionScheduler {
    db: Arc<Mutex<Database>>,
    adapters: Vec<Arc<dyn PaymentAdapter>>,
    canary_checker: Arc<CanaryChecker>,
}

impl SubscriptionScheduler {
    pub fn new(db: Arc<Mutex<Database>>, adapters: Vec<Arc<dyn PaymentAdapter>>) -> Self {
        Self {
            db,
            adapters,
            canary_checker: Arc::new(CanaryChecker::new()),
        }
    }

    /// Find the adapter for a given network_id.
    fn adapter_for(&self, network_id: &str) -> Option<Arc<dyn PaymentAdapter>> {
        self.adapters
            .iter()
            .find(|a| a.network_id() == network_id)
            .cloned()
    }

    /// Run all due subscriptions. Called periodically by the Tauri app.
    /// Also checks canaries for all active subscriptions.
    pub async fn tick(&self) -> Vec<TickResult> {
        let now = Utc::now();
        let subs = {
            let db = self.db.lock().await;
            db.list_subscriptions().unwrap_or_default()
        };

        let mut results = Vec::new();

        for sub in &subs {
            // Canary check for all active subscriptions that have a canary_url
            if sub.is_active() {
                if let Some(canary_url) = &sub.canary_url {
                    let status = self
                        .canary_checker
                        .check(
                            canary_url,
                            &sub.destination_address,
                            sub.merchant_pubkey.as_deref(),
                        )
                        .await;

                    let status_str = format!("{:?}", status);
                    let last_ok = if status == CanaryStatus::Ok {
                        Some(now)
                    } else {
                        sub.canary_last_ok
                    };

                    {
                        let db = self.db.lock().await;
                        let _ = db.update_canary_status(&sub.id, &status_str, last_ok);
                    }

                    match status {
                        CanaryStatus::Ok | CanaryStatus::NotConfigured => {}
                        CanaryStatus::SignatureInvalid | CanaryStatus::AddressMismatch => {
                            warn!("Canary security alert for '{}': {:?}", sub.label, status);
                            let db = self.db.lock().await;
                            let _ = db.update_subscription_status(
                                &sub.id,
                                &SubscriptionStatus::Suspended,
                            );
                            results.push(TickResult::CanaryAlert {
                                subscription_id: sub.id.clone(),
                                status: format!("{:?}", status),
                            });
                            continue;
                        }
                        CanaryStatus::Expired | CanaryStatus::InactiveByCreator => {
                            warn!("Canary expired/inactive for '{}': {:?}", sub.label, status);
                            let db = self.db.lock().await;
                            let _ = db.update_subscription_status(
                                &sub.id,
                                &SubscriptionStatus::Suspended,
                            );
                            results.push(TickResult::CanaryAlert {
                                subscription_id: sub.id.clone(),
                                status: format!("{:?}", status),
                            });
                            continue;
                        }
                        CanaryStatus::Unreachable => {
                            // Non-blocking: warn but don't suspend (network may be down)
                            warn!("Canary unreachable for '{}' — skipping payment this cycle", sub.label);
                            results.push(TickResult::CanaryWarning {
                                subscription_id: sub.id.clone(),
                            });
                            continue;
                        }
                    }
                }
            }

            // Payment due?
            let due = sub
                .next_payment_at
                .map(|t| t <= now)
                .unwrap_or(false);

            if !due || !sub.is_active() {
                continue;
            }

            info!("Payment due for '{}' ({})", sub.label, sub.id);
            let result = self.process_payment(sub).await;
            results.push(result);
        }

        results
    }

    async fn process_payment(&self, sub: &Subscription) -> TickResult {
        let adapter = match self.adapter_for(&sub.payment_network) {
            Some(a) => a,
            None => {
                error!("No adapter for network '{}' on subscription '{}'", sub.payment_network, sub.id);
                return TickResult::Failed {
                    subscription_id: sub.id.clone(),
                    error: format!("No adapter for network '{}'", sub.payment_network),
                };
            }
        };

        if !adapter.is_ready().await {
            warn!("Adapter not ready for '{}' — will retry next tick", sub.label);
            return TickResult::Deferred {
                subscription_id: sub.id.clone(),
            };
        }

        // Convert amount to atomic units
        let amount_atomic = match adapter.to_atomic(&sub.amount) {
            Ok(a) => a,
            Err(e) => {
                return TickResult::Failed {
                    subscription_id: sub.id.clone(),
                    error: format!("Amount conversion failed: {e}"),
                };
            }
        };

        let payment_id = Uuid::new_v4().to_string();
        let now = Utc::now();

        let mut record = PaymentRecord {
            id: payment_id.clone(),
            subscription_id: sub.id.clone(),
            wallet_id: sub.wallet_id.clone(),
            destination_address: sub.destination_address.clone(),
            amount_atomic,
            currency: sub.currency.clone(),
            tx_id: None,
            status: PaymentStatus::Pending,
            error_message: None,
            initiated_at: now,
            confirmed_at: None,
        };

        // First attempt
        let send_result = adapter
            .send(&sub.destination_address, amount_atomic, Some(&sub.label))
            .await;

        match send_result {
            Ok(tx_id) => {
                info!("Payment sent: tx={} for '{}'", tx_id.0, sub.label);
                record.tx_id = Some(tx_id.0.clone());
                record.status = PaymentStatus::Sent;

                let new_payments_sent = sub.payments_sent + 1;

                // Compute next payment time
                let next = if sub.is_infinite()
                    || new_payments_sent < sub.number_of_payments
                {
                    compute_next_occurrence(&sub.schedule)
                } else {
                    None // Final payment sent
                };

                {
                    let db = self.db.lock().await;
                    let _ = db.insert_payment(&record);
                    let _ = db.update_next_payment(&sub.id, next, new_payments_sent);
                    if next.is_none() && !sub.is_infinite() {
                        let _ = db.update_subscription_status(
                            &sub.id,
                            &SubscriptionStatus::Cancelled,
                        );
                    }
                }

                TickResult::Sent {
                    subscription_id: sub.id.clone(),
                    tx_id: tx_id.0,
                }
            }
            Err(e) => {
                warn!("Payment failed for '{}': {e}", sub.label);
                // Retry once
                let retry_result = adapter
                    .send(&sub.destination_address, amount_atomic, Some(&sub.label))
                    .await;

                match retry_result {
                    Ok(tx_id) => {
                        record.tx_id = Some(tx_id.0.clone());
                        record.status = PaymentStatus::Sent;

                        let new_payments_sent = sub.payments_sent + 1;
                        let next = if sub.is_infinite()
                            || new_payments_sent < sub.number_of_payments
                        {
                            compute_next_occurrence(&sub.schedule)
                        } else {
                            None
                        };

                        let db = self.db.lock().await;
                        let _ = db.insert_payment(&record);
                        let _ = db.update_next_payment(&sub.id, next, new_payments_sent);

                        TickResult::Sent {
                            subscription_id: sub.id.clone(),
                            tx_id: tx_id.0,
                        }
                    }
                    Err(retry_err) => {
                        record.status = PaymentStatus::Failed;
                        record.error_message = Some(retry_err.to_string());

                        {
                            let db = self.db.lock().await;
                            let _ = db.insert_payment(&record);
                            let _ = db.update_subscription_status(
                                &sub.id,
                                &SubscriptionStatus::PastDue,
                            );
                        }

                        TickResult::Failed {
                            subscription_id: sub.id.clone(),
                            error: retry_err.to_string(),
                        }
                    }
                }
            }
        }
    }
}

/// Parse a cron expression and return the next occurrence after now.
/// Uses a simple approximation — will be replaced with a proper cron parser.
fn compute_next_occurrence(cron_expr: &str) -> Option<chrono::DateTime<Utc>> {
    // Minimal implementation: supports common cases
    // "0 0 1 * *" = monthly, "0 0 * * *" = daily, "0 0 * * 0" = weekly
    // TODO: replace with tokio-cron-scheduler's cron::Schedule parser
    use chrono::Datelike;
    let now = Utc::now();
    let parts: Vec<&str> = cron_expr.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }

    // Detect monthly: day-of-month is a number, month and dow are *
    if parts[2] != "*" && parts[3] == "*" && parts[4] == "*" {
        // Monthly on day X
        let day: u32 = parts[2].parse().ok()?;
        let next_month = if now.day() >= day {
            if now.month() == 12 {
                now.with_year(now.year() + 1)?.with_month(1)?.with_day(day)?
            } else {
                now.with_month(now.month() + 1)?.with_day(day)?
            }
        } else {
            now.with_day(day)?
        };
        return Some(next_month.date_naive().and_hms_opt(0, 0, 0)?.and_utc());
    }

    // Default: next day
    Some(now + chrono::Duration::days(1))
}

/// Result of a single scheduler tick for a subscription.
#[derive(Debug)]
pub enum TickResult {
    Sent { subscription_id: String, tx_id: String },
    Failed { subscription_id: String, error: String },
    Deferred { subscription_id: String },
    CanaryAlert { subscription_id: String, status: String },
    CanaryWarning { subscription_id: String },
}
