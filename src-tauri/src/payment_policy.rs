//! Payment safety rules: amount and cadence limits, terms fingerprints, and a
//! minimal cron evaluator (6 fields: sec min hour dom month dow, local time).
//!
//! Every rule here is enforced by the backend, so a compromised or buggy frontend
//! cannot bypass it.

use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike, Utc};
use std::collections::HashMap;
use std::path::Path;

/// Below this, network fees become a large share of the payment.
pub const MIN_AMOUNT_XMR: f64 = 0.00021;
/// From this amount, every payment needs an explicit confirmation, even in auto mode.
pub const HIGH_AMOUNT_XMR: f64 = 1.0;
/// Above this amount, payments are refused unless large payments are unlocked in Settings.
pub const MAX_AMOUNT_XMR: f64 = 5.0;
/// Cadences shorter than this need "short cadences" unlocked in Settings.
pub const MIN_CADENCE_MINUTES: i64 = 7 * 24 * 60;
/// Hard floor, even when short cadences are unlocked. "Every 25 min" ("0 */25 * * * *")
/// has a 10-min gap at the top of the hour; the scheduler's global 25-min cooldown
/// between sends still applies.
pub const ABSOLUTE_MIN_CADENCE_MINUTES: i64 = 10;
/// Projected monthly total above which adding a subscription needs an explicit confirmation.
pub const MONTHLY_CONFIRM_XMR: f64 = 1.0;
/// Upper bound of a standard transfer fee, used for "enough balance" checks.
pub const FEE_MARGIN_XMR: f64 = 0.0001;

const LIMITS_FILE: &str = "payment_limits.json";
const WALLET_CAP_FILE: &str = "wallet_cap.json";
const APPROVED_TERMS_FILE: &str = "approved_terms.json";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaymentLimits {
    /// Allow amounts above MAX_AMOUNT_XMR.
    pub allow_large_amounts: bool,
    /// Allow cadences between ABSOLUTE_MIN_CADENCE_MINUTES and MIN_CADENCE_MINUTES.
    pub allow_short_cadence: bool,
}

impl PaymentLimits {
    /// Plain file of earlier versions, read once to migrate into the policy vault.
    pub fn load_legacy(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(LIMITS_FILE)).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapMode {
    /// Share of the funds available at the start of the period.
    Percent,
    /// Fixed amount in XMR.
    Fixed,
}

/// Wallet-wide guards on automatic payments, whatever the subscriptions say: a ceiling over a
/// rolling period, a balance floor, a daily number of sends and a waiting time for new
/// addresses. Checked against the wallet's own outgoing transfers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WalletCap {
    pub enabled: bool,
    pub mode: CapMode,
    pub percent: u32,
    pub fixed_xmr: f64,
    pub period_days: u32,
    /// Balance that automatic payments never go below (0 = off).
    pub floor_xmr: f64,
    /// Sends allowed over the last 24 h (0 = off).
    pub max_sends_per_day: u32,
    /// Hours between approving a never-paid address and the first payment to it (0 = off).
    pub new_address_delay_hours: u32,
    /// Network fee above which a prepared payment is dropped instead of relayed (0 = off).
    pub max_fee_xmr: f64,
}

impl Default for WalletCap {
    fn default() -> Self {
        Self {
            enabled: true, mode: CapMode::Percent, percent: 33, fixed_xmr: 1.0, period_days: 30,
            floor_xmr: 0.0, max_sends_per_day: 0, new_address_delay_hours: 0,
            max_fee_xmr: DEFAULT_MAX_FEE_XMR,
        }
    }
}

/// Normal fees are around 0.00003 XMR; 0.001 only trips on an abnormal fee market.
pub const DEFAULT_MAX_FEE_XMR: f64 = 0.001;

/// Days covered by a per-subscription cap.
pub const SUB_CAP_DAYS: i64 = 30;

/// What the wallet reports about its recent outgoing transfers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct OutStats {
    /// Amounts + fees within the wallet cap period.
    pub sent_period: u64,
    pub sends_last_day: u32,
    /// Amounts paid to the destination over the last SUB_CAP_DAYS.
    pub to_dest_recent: u64,
    pub dest_paid_before: bool,
}

/// Everything needed to decide whether one automatic payment may leave.
pub struct SendContext {
    pub amount_atomic: u64,
    pub balance_atomic: u64,
    pub stats: OutStats,
    /// Per-subscription ceiling over SUB_CAP_DAYS, in XMR.
    pub sub_cap_xmr: Option<f64>,
    /// When the destination address was first approved (None: approved before this was tracked).
    pub dest_first_seen: Option<i64>,
    pub now: i64,
}

impl WalletCap {
    /// Plain file of earlier versions, read once to migrate into the policy vault.
    pub fn load_legacy(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(WALLET_CAP_FILE)).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1..=100).contains(&self.percent) { return Err("Percent must be between 1 and 100.".into()); }
        if !(self.fixed_xmr.is_finite() && self.fixed_xmr > 0.0) { return Err("Amount must be positive.".into()); }
        if !(1..=365).contains(&self.period_days) { return Err("Period must be between 1 and 365 days.".into()); }
        if !(self.floor_xmr.is_finite() && self.floor_xmr >= 0.0) { return Err("Floor must be zero or positive.".into()); }
        if self.max_sends_per_day > 1000 { return Err("At most 1000 sends per day.".into()); }
        if self.new_address_delay_hours > 168 { return Err("Delay must be at most 168 hours.".into()); }
        if !(self.max_fee_xmr.is_finite() && (0.0..=0.1).contains(&self.max_fee_xmr)) {
            return Err("Fee cap must be between 0 and 0.1 XMR.".into());
        }
        Ok(())
    }

    /// Ceiling in piconero for the current period (None when disabled). In percent mode the base
    /// is the balance at the start of the period: current balance + what was sent since.
    pub fn cap_atomic(&self, balance_atomic: u64, sent_atomic: u64) -> Option<u64> {
        if !self.enabled { return None; }
        Some(match self.mode {
            CapMode::Percent => ((balance_atomic as u128 + sent_atomic as u128) * self.percent as u128 / 100) as u64,
            CapMode::Fixed => (self.fixed_xmr * 1e12) as u64,
        })
    }

    pub fn check(&self, amount_atomic: u64, balance_atomic: u64, sent_atomic: u64) -> Result<(), PolicyError> {
        match self.cap_atomic(balance_atomic, sent_atomic) {
            Some(cap) if sent_atomic.saturating_add(amount_atomic) > cap => {
                Err(PolicyError::WalletCap(cap.saturating_sub(sent_atomic) as f64 / 1e12))
            }
            _ => Ok(()),
        }
    }

    /// All wallet guards plus the per-subscription ones, cheapest to explain first.
    pub fn check_send(&self, c: &SendContext) -> Result<(), PolicyError> {
        if self.max_sends_per_day > 0 && c.stats.sends_last_day >= self.max_sends_per_day {
            return Err(PolicyError::DailySends(self.max_sends_per_day));
        }
        if self.new_address_delay_hours > 0 && !c.stats.dest_paid_before {
            if let Some(seen) = c.dest_first_seen {
                let ready = seen + self.new_address_delay_hours as i64 * 3600;
                if c.now < ready {
                    return Err(PolicyError::NewAddressDelay(((ready - c.now + 3599) / 3600) as u32));
                }
            }
        }
        if let Some(cap) = c.sub_cap_xmr {
            let cap = (cap * 1e12) as u64;
            if c.stats.to_dest_recent.saturating_add(c.amount_atomic) > cap {
                return Err(PolicyError::SubCap(cap.saturating_sub(c.stats.to_dest_recent) as f64 / 1e12));
            }
        }
        let floor = (self.floor_xmr * 1e12) as u64;
        let fee = (FEE_MARGIN_XMR * 1e12) as u64;
        if floor > 0 && c.balance_atomic < c.amount_atomic.saturating_add(fee).saturating_add(floor) {
            return Err(PolicyError::BalanceFloor(self.floor_xmr));
        }
        self.check(c.amount_atomic, c.balance_atomic, c.stats.sent_period)
    }

    /// Err when a prepared transaction's fee is above the cap.
    pub fn check_fee(&self, fee_atomic: u64) -> Result<(), PolicyError> {
        if self.max_fee_xmr > 0.0 && fee_atomic > (self.max_fee_xmr * 1e12) as u64 {
            return Err(PolicyError::FeeCap(self.max_fee_xmr));
        }
        Ok(())
    }

    /// True when `self` allows more than `current` (needs the master password).
    pub fn is_looser_than(&self, current: &WalletCap) -> bool {
        let cap_looser = current.enabled && (
            !self.enabled || self.mode != current.mode || self.period_days < current.period_days
                || match self.mode {
                    CapMode::Percent => self.percent > current.percent,
                    CapMode::Fixed => self.fixed_xmr > current.fixed_xmr,
                });
        let sends_looser = current.max_sends_per_day > 0
            && (self.max_sends_per_day == 0 || self.max_sends_per_day > current.max_sends_per_day);
        let fee_looser = current.max_fee_xmr > 0.0
            && (self.max_fee_xmr == 0.0 || self.max_fee_xmr > current.max_fee_xmr);
        cap_looser
            || fee_looser
            || self.floor_xmr < current.floor_xmr
            || sends_looser
            || self.new_address_delay_hours < current.new_address_delay_hours
    }
}

/// Machine-readable policy violation. Serialized as "POLICY:<CODE>[:<detail>]"
/// so the frontend can show a friendly message.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyError {
    UnsupportedCurrency,
    UnsupportedRepeat,
    InvalidAddress,
    InvalidAmount,
    AmountTooLow,
    AmountAboveMax,
    InvalidSchedule,
    CadenceBelowFloor,
    CadenceTooShort,
    MonthlyTotal(f64),
    /// Remaining allowance (XMR) of the wallet cap for the current period.
    WalletCap(f64),
    /// Balance floor (XMR) that the payment would cross.
    BalanceFloor(f64),
    /// Daily number of sends reached.
    DailySends(u32),
    /// Remaining allowance (XMR) of the subscription cap over SUB_CAP_DAYS.
    SubCap(f64),
    /// Hours left before a newly approved address may be paid.
    NewAddressDelay(u32),
    /// Fee cap (XMR) exceeded by the prepared transaction.
    FeeCap(f64),
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedCurrency => write!(f, "POLICY:UNSUPPORTED_CURRENCY"),
            Self::UnsupportedRepeat => write!(f, "POLICY:UNSUPPORTED_REPEAT"),
            Self::InvalidAddress => write!(f, "POLICY:INVALID_ADDRESS"),
            Self::InvalidAmount => write!(f, "POLICY:INVALID_AMOUNT"),
            Self::AmountTooLow => write!(f, "POLICY:AMOUNT_TOO_LOW"),
            Self::AmountAboveMax => write!(f, "POLICY:AMOUNT_ABOVE_MAX"),
            Self::InvalidSchedule => write!(f, "POLICY:INVALID_SCHEDULE"),
            Self::CadenceBelowFloor => write!(f, "POLICY:CADENCE_BELOW_FLOOR"),
            Self::CadenceTooShort => write!(f, "POLICY:CADENCE_TOO_SHORT"),
            Self::MonthlyTotal(x) => write!(f, "POLICY:MONTHLY_TOTAL:{:.4}", x),
            Self::WalletCap(x) => write!(f, "POLICY:WALLET_CAP:{:.4}", x),
            Self::BalanceFloor(x) => write!(f, "POLICY:BALANCE_FLOOR:{:.4}", x),
            Self::DailySends(n) => write!(f, "POLICY:DAILY_SENDS:{}", n),
            Self::SubCap(x) => write!(f, "POLICY:SUB_CAP:{:.4}", x),
            Self::NewAddressDelay(h) => write!(f, "POLICY:NEW_ADDRESS_DELAY:{}", h),
            Self::FeeCap(x) => write!(f, "POLICY:FEE_CAP:{:.4}", x),
        }
    }
}

pub fn parse_amount(amount: &str) -> Option<f64> {
    amount.trim().parse::<f64>().ok().filter(|a| a.is_finite() && *a > 0.0)
}

/// Only XMR is sent by the scheduler: any other currency would be paid as the same number of XMR.
pub fn validate_currency(currency: &str) -> Result<(), PolicyError> {
    if currency.trim().eq_ignore_ascii_case("XMR") { Ok(()) } else { Err(PolicyError::UnsupportedCurrency) }
}

/// Check the amount and cadence of a subscription against the limits.
pub fn validate_terms(amount: &str, schedule: &str, limits: &PaymentLimits) -> Result<(), PolicyError> {
    let a = parse_amount(amount).ok_or(PolicyError::InvalidAmount)?;
    if a < MIN_AMOUNT_XMR { return Err(PolicyError::AmountTooLow); }
    if a > MAX_AMOUNT_XMR && !limits.allow_large_amounts { return Err(PolicyError::AmountAboveMax); }

    let period = min_interval_minutes(schedule).ok_or(PolicyError::InvalidSchedule)?;
    if period < ABSOLUTE_MIN_CADENCE_MINUTES { return Err(PolicyError::CadenceBelowFloor); }
    if period < MIN_CADENCE_MINUTES && !limits.allow_short_cadence { return Err(PolicyError::CadenceTooShort); }
    Ok(())
}

/// Monero mainnet address: base58, 95 chars (standard "4", subaddress "8")
/// or 106 chars (integrated "4").
pub fn validate_address(address: &str) -> Result<(), PolicyError> {
    const B58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let a = address.trim();
    let shape_ok = match a.len() {
        95 => a.starts_with('4') || a.starts_with('8'),
        106 => a.starts_with('4'),
        _ => false,
    };
    if shape_ok && a.chars().all(|c| B58.contains(c)) { Ok(()) } else { Err(PolicyError::InvalidAddress) }
}

/// Free text from a subscription code: drop control characters and cap the length.
pub fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max_chars).collect::<String>().trim().to_string()
}

/// Average XMR per 30 days for one subscription.
pub fn monthly_projection(amount: &str, schedule: &str) -> f64 {
    match (parse_amount(amount), average_interval_minutes(schedule)) {
        (Some(a), Some(p)) if p > 0 => a * (30.0 * 24.0 * 60.0) / p as f64,
        _ => 0.0,
    }
}

// ---- Approved terms (detects a changed amount / address / cadence) ----

/// Hash of the terms the user agreed to. Stored instead of the terms themselves
/// so no address or amount is written in clear in this file.
pub fn terms_fingerprint(address: &str, amount: &str, currency: &str, schedule: &str) -> String {
    let data = format!("{}|{}|{}|{}", address.trim(), amount.trim(), currency.trim(), schedule.trim());
    blake3::hash(data.as_bytes()).to_hex().to_string()
}

/// Plain file of earlier versions, read once to migrate into the policy vault.
pub fn load_legacy_approved_terms(data_dir: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(data_dir.join(APPROVED_TERMS_FILE)).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Plain policy files of earlier versions, removed once migrated into the policy vault.
pub fn remove_legacy_files(data_dir: &Path) {
    for f in [LIMITS_FILE, WALLET_CAP_FILE, APPROVED_TERMS_FILE] {
        let _ = std::fs::remove_file(data_dir.join(f));
    }
}

/// Why a payment must be confirmed by the user even in automatic mode.
pub fn confirmation_reasons(amount: &str, fingerprint: &str, approved: Option<&String>) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    match approved {
        None => reasons.push("first_payment"),
        Some(fp) if fp != fingerprint => reasons.push("terms_changed"),
        _ => {}
    }
    if parse_amount(amount).map(|a| a >= HIGH_AMOUNT_XMR).unwrap_or(false) {
        reasons.push("high_amount");
    }
    reasons
}

// ---- Minimal cron evaluator ----

struct CronField { allowed: Vec<bool>, any: bool }

impl CronField {
    fn parse(expr: &str, min: u32, max: u32) -> Option<Self> {
        let mut allowed = vec![false; (max + 1) as usize];
        let any = expr == "*";
        for part in expr.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0)?),
                None => (part, 1),
            };
            let (lo, hi) = if range == "*" {
                (min, max)
            } else if let Some((a, b)) = range.split_once('-') {
                (a.parse().ok()?, b.parse().ok()?)
            } else {
                let v: u32 = range.parse().ok()?;
                (v, if part.contains('/') { max } else { v })
            };
            if lo < min || hi > max || lo > hi { return None; }
            let mut v = lo;
            while v <= hi { allowed[v as usize] = true; v += step; }
        }
        Some(Self { allowed, any })
    }
    fn has(&self, v: u32) -> bool { self.allowed.get(v as usize).copied().unwrap_or(false) }
}

struct Cron { minute: CronField, hour: CronField, dom: CronField, month: CronField, dow: CronField, year: CronField }

const YEAR_MIN: u32 = 1970;
const YEAR_MAX: u32 = 2199;
/// Longest supported gap between two payments ("every N years").
pub const MAX_REPEAT_YEARS: u32 = 10;

impl Cron {
    fn parse(expr: &str) -> Option<Self> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        // 5 classic fields, 6 with seconds (ignored), or 7 with a Quartz year field ("2026/2").
        let (f, year) = match parts.len() {
            7 => (&parts[1..6], parts[6]),
            6 => (&parts[1..], "*"),
            5 => (&parts[..], "*"),
            _ => return None,
        };
        let mut dow = CronField::parse(f[4], 0, 7)?;
        if dow.has(7) { dow.allowed[0] = true; }
        Some(Self {
            minute: CronField::parse(f[0], 0, 59)?,
            hour: CronField::parse(f[1], 0, 23)?,
            dom: CronField::parse(f[2], 1, 31)?,
            month: CronField::parse(f[3], 1, 12)?,
            dow,
            year: CronField::parse(year, YEAR_MIN, YEAR_MAX)?,
        })
    }

    fn day_matches(&self, d: &DateTime<Local>) -> bool {
        if !self.year.has(d.year() as u32) || !self.month.has(d.month()) { return false; }
        let dom_ok = self.dom.has(d.day());
        let dow_ok = self.dow.has(d.weekday().num_days_from_sunday());
        // Vixie cron: when both day fields are restricted, either one matching is enough.
        match (self.dom.any, self.dow.any) {
            (true, true) => true,
            (true, false) => dow_ok,
            (false, true) => dom_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }
}

/// First occurrence strictly after `after`. None if the expression is invalid
/// or never matches within the supported horizon.
pub fn next_after(schedule: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let cron = Cron::parse(schedule)?;
    let start = after.with_timezone(&Local) + Duration::minutes(1);
    let mut t = start.with_second(0)?.with_nanosecond(0)?;
    let limit = start + Duration::days((MAX_REPEAT_YEARS as i64 + 1) * 366);
    while t < limit {
        if !cron.year.has(t.year() as u32) {
            let next_year = chrono::NaiveDate::from_ymd_opt(t.year() + 1, 1, 1)?.and_hms_opt(0, 0, 0)?;
            t = Local.from_local_datetime(&next_year).earliest()?;
            continue;
        }
        if !cron.day_matches(&t) {
            let next_day = t.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
            t = Local.from_local_datetime(&next_day).earliest()?;
            continue;
        }
        if cron.hour.has(t.hour()) && cron.minute.has(t.minute()) {
            return Some(t.with_timezone(&Utc));
        }
        t += Duration::minutes(1);
    }
    None
}

/// Turn a yearly schedule into "every `n` years" starting at `first_year`,
/// by appending a Quartz year field: "0 0 9 1 1 *" -> "0 0 9 1 1 * 2026/2".
pub fn with_year_step(schedule: &str, n: u32, first_year: i32) -> Result<String, PolicyError> {
    if n <= 1 { return Ok(schedule.to_string()); }
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    let base = match parts.len() { 6 => parts.clone(), 5 => [&["0"][..], &parts[..]].concat(), _ => return Err(PolicyError::UnsupportedRepeat) };
    let yearly = base[4].parse::<u32>().is_ok() && base[3].parse::<u32>().is_ok();
    if !yearly || n > MAX_REPEAT_YEARS || first_year < YEAR_MIN as i32 {
        return Err(PolicyError::UnsupportedRepeat);
    }
    Ok(format!("{} {}/{}", base.join(" "), first_year, n))
}

fn intervals(schedule: &str, count: usize) -> Option<Vec<i64>> {
    let mut t = next_after(schedule, Utc::now())?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let n = next_after(schedule, t)?;
        out.push((n - t).num_minutes());
        t = n;
    }
    Some(out)
}

/// Shortest gap between two consecutive occurrences (used for cadence limits).
pub fn min_interval_minutes(schedule: &str) -> Option<i64> {
    intervals(schedule, 6)?.into_iter().min()
}

/// Mean gap between occurrences (used for monthly projections).
pub fn average_interval_minutes(schedule: &str) -> Option<i64> {
    let v = intervals(schedule, 6)?;
    Some(v.iter().sum::<i64>() / v.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_cap_percent_of_starting_balance() {
        let xmr = |x: f64| (x * 1e12) as u64;
        let cap = WalletCap::default(); // 33 % over 30 days
        // 3 XMR at the start of the period, nothing sent yet: up to 0.99 XMR.
        assert!(cap.check(xmr(0.99), xmr(3.0), 0).is_ok());
        assert!(cap.check(xmr(1.0), xmr(3.0), 0).is_err());
        // 0.5 already sent (balance now 2.5): the base stays 3 XMR, 0.49 left.
        assert!(cap.check(xmr(0.49), xmr(2.5), xmr(0.5)).is_ok());
        assert_eq!(cap.check(xmr(0.5), xmr(2.5), xmr(0.5)), Err(PolicyError::WalletCap(0.49)));
        let off = WalletCap { enabled: false, ..WalletCap::default() };
        assert!(off.check(xmr(100.0), xmr(1.0), 0).is_ok());
        let fixed = WalletCap { mode: CapMode::Fixed, fixed_xmr: 0.2, ..WalletCap::default() };
        assert!(fixed.check(xmr(0.1), xmr(50.0), xmr(0.1)).is_ok());
        assert!(fixed.check(xmr(0.11), xmr(50.0), xmr(0.1)).is_err());
    }

    #[test]
    fn send_guards() {
        let xmr = |x: f64| (x * 1e12) as u64;
        let guards = WalletCap {
            enabled: false, floor_xmr: 1.0, max_sends_per_day: 3, new_address_delay_hours: 24,
            ..WalletCap::default()
        };
        let ctx = |amount: f64, balance: f64, stats: OutStats| SendContext {
            amount_atomic: xmr(amount), balance_atomic: xmr(balance), stats,
            sub_cap_xmr: Some(0.5), dest_first_seen: None, now: 100_000,
        };
        let paid = OutStats { dest_paid_before: true, ..OutStats::default() };
        assert!(guards.check_send(&ctx(0.1, 2.0, paid)).is_ok());
        // Floor: 1.3 - 0.4 - fee leaves less than 1.0.
        assert_eq!(guards.check_send(&ctx(0.4, 1.3, paid)), Err(PolicyError::BalanceFloor(1.0)));
        let busy = OutStats { sends_last_day: 3, ..paid };
        assert_eq!(guards.check_send(&ctx(0.1, 5.0, busy)), Err(PolicyError::DailySends(3)));
        let sub_used = OutStats { to_dest_recent: xmr(0.45), ..paid };
        assert_eq!(guards.check_send(&ctx(0.1, 5.0, sub_used)), Err(PolicyError::SubCap(0.05)));
        // New address approved 2 h ago: 22 h left; once paid or untracked, no wait.
        let mut c = ctx(0.1, 5.0, OutStats::default());
        c.dest_first_seen = Some(100_000 - 7200);
        assert_eq!(guards.check_send(&c), Err(PolicyError::NewAddressDelay(22)));
        c.stats.dest_paid_before = true;
        assert!(guards.check_send(&c).is_ok());
    }

    #[test]
    fn wallet_cap_loosening() {
        let cur = WalletCap::default();
        assert!(WalletCap { percent: 50, ..cur.clone() }.is_looser_than(&cur));
        assert!(WalletCap { enabled: false, ..cur.clone() }.is_looser_than(&cur));
        assert!(WalletCap { period_days: 7, ..cur.clone() }.is_looser_than(&cur));
        assert!(!WalletCap { percent: 20, ..cur.clone() }.is_looser_than(&cur));
        assert!(!WalletCap { period_days: 60, ..cur.clone() }.is_looser_than(&cur));
        assert!(!cur.is_looser_than(&WalletCap { enabled: false, ..cur.clone() }));
        let strict = WalletCap { floor_xmr: 0.5, max_sends_per_day: 5, new_address_delay_hours: 24, ..cur.clone() };
        assert!(WalletCap { floor_xmr: 0.2, ..strict.clone() }.is_looser_than(&strict));
        assert!(WalletCap { max_sends_per_day: 0, ..strict.clone() }.is_looser_than(&strict));
        assert!(WalletCap { new_address_delay_hours: 0, ..strict.clone() }.is_looser_than(&strict));
        assert!(!WalletCap { max_sends_per_day: 2, floor_xmr: 1.0, ..strict.clone() }.is_looser_than(&strict));
        assert!(WalletCap { max_fee_xmr: 0.0, ..cur.clone() }.is_looser_than(&cur));
        assert!(WalletCap { max_fee_xmr: 0.01, ..cur.clone() }.is_looser_than(&cur));
        assert!(!WalletCap { max_fee_xmr: 0.0005, ..cur.clone() }.is_looser_than(&cur));
        assert_eq!(cur.check_fee(30_000_000), Ok(()));
        assert_eq!(cur.check_fee(2_000_000_000), Err(PolicyError::FeeCap(0.001)));
        assert_eq!(WalletCap { max_fee_xmr: 0.0, ..cur.clone() }.check_fee(u64::MAX), Ok(()));
    }

    #[test]
    fn cadences_of_app_presets() {
        let day = 24 * 60;
        assert_eq!(min_interval_minutes("0 0 * * * *"), Some(60));
        assert_eq!(min_interval_minutes("0 0 9 * * *"), Some(day));
        assert_eq!(min_interval_minutes("0 0 9 * * 1"), Some(7 * day));
        assert!(min_interval_minutes("0 0 9 1 * *").unwrap() >= 28 * day);
        assert!(min_interval_minutes("0 0 9 1 */3 *").unwrap() >= 89 * day);
        assert!(min_interval_minutes("0 0 9 1 1 *").unwrap() >= 365 * day);
        assert_eq!(min_interval_minutes("0 */25 * * * *"), Some(10)); // 0, 25, 50 then 0
        assert_eq!(min_interval_minutes("nonsense"), None);
    }

    #[test]
    fn multi_year() {
        let day = 24 * 60;
        let s = with_year_step("0 0 9 1 1 *", 3, 2026).unwrap();
        assert_eq!(s, "0 0 9 1 1 * 2026/3");
        let gap = min_interval_minutes(&s).unwrap();
        assert!(gap >= 3 * 365 * day && gap <= 3 * 366 * day);
        let after = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        assert_eq!(next_after(&s, after).unwrap().with_timezone(&Local).year(), 2029);
        assert!(min_interval_minutes("0 0 9 1 1 * 2026/10").unwrap() >= 3650 * day);
        assert_eq!(with_year_step("0 0 9 1 * *", 2, 2026), Err(PolicyError::UnsupportedRepeat));
        assert_eq!(with_year_step("0 0 9 1 1 *", 11, 2026), Err(PolicyError::UnsupportedRepeat));
        assert_eq!(with_year_step("0 0 9 1 1 *", 1, 2026).unwrap(), "0 0 9 1 1 *");
        let off = PaymentLimits::default();
        assert!(validate_terms("0.5", "0 0 9 1 1 * 2026/2", &off).is_ok());
    }

    #[test]
    fn limits() {
        let off = PaymentLimits::default();
        let on = PaymentLimits { allow_large_amounts: true, allow_short_cadence: true };
        assert_eq!(validate_terms("0.0002", "0 0 9 1 * *", &off), Err(PolicyError::AmountTooLow));
        assert!(validate_terms("0.00021", "0 0 9 1 * *", &off).is_ok());
        assert_eq!(validate_terms("6", "0 0 9 1 * *", &off), Err(PolicyError::AmountAboveMax));
        assert!(validate_terms("6", "0 0 9 1 * *", &on).is_ok());
        assert_eq!(validate_terms("0.01", "0 0 9 * * *", &off), Err(PolicyError::CadenceTooShort));
        assert!(validate_terms("0.01", "0 0 9 * * *", &on).is_ok());
        assert!(validate_terms("0.01", "0 0 9 * * 1", &off).is_ok());
        assert_eq!(validate_terms("abc", "0 0 9 1 * *", &off), Err(PolicyError::InvalidAmount));
    }

    #[test]
    fn addresses_and_text() {
        let std95 = format!("4{}", "A".repeat(94));
        let sub95 = format!("8{}", "b".repeat(94));
        let int106 = format!("4{}", "c".repeat(105));
        assert!(validate_address(&std95).is_ok());
        assert!(validate_address(&sub95).is_ok());
        assert!(validate_address(&int106).is_ok());
        assert!(validate_address(&format!("8{}", "c".repeat(105))).is_err());
        assert!(validate_address(&format!("4{}0", "A".repeat(93))).is_err()); // '0' is not base58
        assert!(validate_address("bc1qxyz").is_err());
        assert_eq!(clean_text("Hi\u{0}<b>\nthere", 50), "Hi<b>there");
        assert_eq!(clean_text("abcdef", 3), "abc");
    }

    #[test]
    fn confirmation() {
        let fp = terms_fingerprint("4abc", "0.1", "XMR", "0 0 9 1 * *");
        assert_eq!(confirmation_reasons("0.1", &fp, None), vec!["first_payment"]);
        assert!(confirmation_reasons("0.1", &fp, Some(&fp)).is_empty());
        assert_eq!(confirmation_reasons("0.1", &fp, Some(&"x".into())), vec!["terms_changed"]);
        assert_eq!(confirmation_reasons("1", &fp, Some(&fp)), vec!["high_amount"]);
    }
}
