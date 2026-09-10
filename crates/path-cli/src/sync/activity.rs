//! The two-hour inactivity policy, judged from per-session source stamps.
//!
//! Activity is what the source did, not what was uploaded: a late tool
//! result that changes the session file counts even when the derived
//! document does not, and repeated reads of an unchanged file never
//! restart the clock.

use super::sources::Stamp;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub(crate) const IDLE_THRESHOLD_SECS: i64 = 2 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Activity {
    pub(crate) last_activity_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modified: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<u64>,
}

impl Activity {
    /// First observation. The provider's own timestamp is trusted when
    /// it is present and not in the future; otherwise the clock starts
    /// now and the session waits the full threshold.
    pub(crate) fn first(now: DateTime<Utc>, stamp: Stamp) -> Self {
        let last_activity_at = match stamp.0 {
            Some(ts) if ts <= now => ts,
            _ => now,
        };
        Self {
            last_activity_at,
            modified: stamp.0,
            size: stamp.1,
        }
    }

    /// Fold in a later observation. A changed stamp is activity dated
    /// no earlier than now, even if the timestamp regressed. Returns
    /// whether the stamp changed.
    pub(crate) fn observe(&mut self, now: DateTime<Utc>, stamp: Stamp) -> bool {
        if (self.modified, self.size) == stamp {
            return false;
        }
        self.modified = stamp.0;
        self.size = stamp.1;
        self.last_activity_at = self.last_activity_at.max(now);
        true
    }

    /// Idle for at least the threshold. Unknown freshness (no stamp at
    /// all) and a future activity time never qualify.
    pub(crate) fn idle_at(&self, now: DateTime<Utc>) -> bool {
        if self.modified.is_none() && self.size.is_none() {
            return false;
        }
        now.signed_duration_since(self.last_activity_at) >= Duration::seconds(IDLE_THRESHOLD_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }
    const H: i64 = 3600;

    #[test]
    fn first_observation_trusts_a_past_timestamp_and_not_a_future_one() {
        let a = Activity::first(t(0), (Some(t(-3 * H)), Some(10)));
        assert_eq!(a.last_activity_at, t(-3 * H));
        assert!(a.idle_at(t(0)));
        let a = Activity::first(t(0), (Some(t(H)), Some(10)));
        assert_eq!(a.last_activity_at, t(0));
        assert!(!a.idle_at(t(2 * H - 1)));
        assert!(a.idle_at(t(2 * H)));
        let a = Activity::first(t(0), (None, Some(10)));
        assert_eq!(a.last_activity_at, t(0));
    }

    #[test]
    fn unknown_freshness_never_qualifies() {
        let a = Activity::first(t(0), (None, None));
        assert!(!a.idle_at(t(10 * H)));
    }

    #[test]
    fn unchanged_reads_do_not_reset_and_changes_advance_to_now() {
        let mut a = Activity::first(t(0), (Some(t(0)), Some(10)));
        assert!(!a.observe(t(H), (Some(t(0)), Some(10))));
        assert_eq!(a.last_activity_at, t(0));
        // A regressed timestamp still counts as activity at observation time.
        assert!(a.observe(t(H), (Some(t(-5 * H)), Some(11))));
        assert_eq!(a.last_activity_at, t(H));
        assert!(!a.idle_at(t(3 * H - 1)));
        assert!(a.idle_at(t(3 * H)));
        // A future activity time never qualifies.
        a.last_activity_at = t(10 * H);
        assert!(!a.idle_at(t(3 * H)));
    }

    #[test]
    fn survives_persistence() {
        let a = Activity::first(t(0), (Some(t(-H)), None));
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Activity>(&json).unwrap(), a);
    }
}
