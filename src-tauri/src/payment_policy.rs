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
const APPROVED_TERMS_FILE: &str = "approved_terms.json";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PaymentLimits {
    /// Allow amounts above MAX_AMOUNT_XMR.
    pub allow_large_amounts: bool,
    /// Allow cadences between ABSOLUTE_MIN_CADENCE_MINUTES and MIN_CADENCE_MINUTES.
    pub allow_short_cadence: bool,
}

impl PaymentLimits {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join(LIMITS_FILE)).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(data_dir.join(LIMITS_FILE), json).map_err(|e| e.to_string())
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

pub fn load_approved_terms(data_dir: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(data_dir.join(APPROVED_TERMS_FILE)).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_approved_terms(data_dir: &Path, terms: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string(terms) {
        if let Err(e) = std::fs::write(data_dir.join(APPROVED_TERMS_FILE), json) {
            log::error!("[Policy] Could not save approved terms: {}", e);
        }
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

struct Cron { minute: CronField, hour: CronField, dom: CronField, month: CronField, dow: CronField }

impl Cron {
    fn parse(expr: &str) -> Option<Self> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        // Accept 6 fields (with seconds, ignored) or classic 5 fields.
        let f = match parts.len() { 6 => &parts[1..], 5 => &parts[..], _ => return None };
        let mut dow = CronField::parse(f[4], 0, 7)?;
        if dow.has(7) { dow.allowed[0] = true; }
        Some(Self {
            minute: CronField::parse(f[0], 0, 59)?,
            hour: CronField::parse(f[1], 0, 23)?,
            dom: CronField::parse(f[2], 1, 31)?,
            month: CronField::parse(f[3], 1, 12)?,
            dow,
        })
    }

    fn day_matches(&self, d: &DateTime<Local>) -> bool {
        if !self.month.has(d.month()) { return false; }
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
/// or never matches within 5 years.
pub fn next_after(schedule: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let cron = Cron::parse(schedule)?;
    let start = after.with_timezone(&Local) + Duration::minutes(1);
    let mut t = start.with_second(0)?.with_nanosecond(0)?;
    let limit = start + Duration::days(5 * 366);
    while t < limit {
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
