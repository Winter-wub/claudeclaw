//! Pairing codes: 8 Crockford base32 characters, in memory only, valid for 10 minutes and burned
//! after 5 wrong attempts (management-api §2.12; architecture §4; #41 D3, #21 D15).
//!
//! A code is created only locally (`pairing.create` is `[local]`), never persisted, and consumed
//! by exactly one successful `POST /v1/pair`. This module owns the code lifecycle and its clock;
//! resolving the paired Device is the service's job.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Duration, Utc};
use claudeclaw_api::PairingCode;
use claudeclaw_api::pairing::PairingStatus;

use crate::random;
use crate::tokens::hex;

/// How long a Pairing code lives (#41 D3).
pub const PAIRING_TTL: Duration = Duration::minutes(10);

/// How many wrong attempts burn a code (#21 D15).
pub const MAX_ATTEMPTS: u8 = 5;

/// A wall clock, so expiry is testable on a paused one.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        // `chrono`'s `clock` feature is off in this workspace, so read the OS clock directly.
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        DateTime::from_timestamp(since_epoch.as_secs() as i64, since_epoch.subsec_nanos())
            .unwrap_or_default()
    }
}

/// One live or ended pairing.
#[derive(Debug, Clone)]
struct Pending {
    id: String,
    code: PairingCode,
    host: Option<String>,
    expires_at: DateTime<Utc>,
    attempts: u8,
    /// Set by the first [`PairingRegistry::claim`], so a code is single-use even under two
    /// simultaneous `pair` calls.
    claimed: bool,
    /// `Pending`, `Completed`, `Cancelled` or `Burned`; `Expired` is computed from the clock.
    status: PairingStatus,
    device_id: Option<String>,
}

/// A `pairing.get` view: the state with expiry applied, and the Device once complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingView {
    pub pairing_id: String,
    pub status: PairingStatus,
    pub expires_at: DateTime<Utc>,
    pub host: Option<String>,
    pub device_id: Option<String>,
}

/// What submitting a code to `pair` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submit {
    /// A live, unused pairing with this code.
    Matched { id: String, host: Option<String> },
    /// The code matched a pairing whose 10 minutes ran out: `410 pairing_expired`.
    Expired { id: String },
    /// Unknown, already used, cancelled or burned: `401 pairing_code_invalid`.
    NoMatch,
}

/// A new pairing code, as [`PairingRegistry::create`] returns it.
#[derive(Debug, Clone)]
pub struct Created {
    pub id: String,
    pub code: PairingCode,
    pub expires_at: DateTime<Utc>,
}

/// The in-memory pairing codes (architecture §4 `claudeclaw-server`).
pub struct PairingRegistry {
    clock: Arc<dyn Clock>,
    inner: Mutex<HashMap<String, Pending>>,
}

impl PairingRegistry {
    /// A registry on the real clock.
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// A registry on an injected clock.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        PairingRegistry {
            clock,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// The registry's clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// How many pairings the registry holds, for tests.
    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        lock(&self.inner).len()
    }

    /// Creates a code for a new pairing: 40 random bits as 8 Crockford base32 characters. Fails,
    /// and stores nothing, when the OS random source fails (#41).
    pub fn create(&self, host: Option<String>) -> Result<Created, random::Unavailable> {
        let mut bytes = [0u8; 5];
        random::fill(&mut bytes)?;
        let code = PairingCode::from_bytes(bytes);
        let now = self.clock.now();
        let id = format!("pair_{}", random_hex()?);
        let expires_at = now + PAIRING_TTL;
        let mut map = lock(&self.inner);
        map.insert(
            id.clone(),
            Pending {
                id: id.clone(),
                code,
                host,
                expires_at,
                attempts: 0,
                claimed: false,
                status: PairingStatus::Pending,
                device_id: None,
            },
        );
        Ok(Created {
            id,
            code,
            expires_at,
        })
    }

    /// The state of a pairing, with expiry applied; `None` when there is no such pairing.
    pub fn get(&self, id: &str) -> Option<PairingView> {
        let now = self.clock.now();
        let map = lock(&self.inner);
        map.get(id).map(|pending| pending.view(now))
    }

    /// Cancels a live pairing. `true` when it was still pending.
    pub fn cancel(&self, id: &str) -> bool {
        let now = self.clock.now();
        let mut map = lock(&self.inner);
        match map.get_mut(id) {
            Some(pending) => match pending.effective(now) {
                PairingStatus::Pending => {
                    pending.status = PairingStatus::Cancelled;
                    true
                }
                _ => false,
            },
            None => false,
        }
    }

    /// Marks a live pairing complete with the new Device. Returns its host, when it matched.
    pub fn complete(&self, id: &str, device_id: String) -> bool {
        let now = self.clock.now();
        let mut map = lock(&self.inner);
        match map.get_mut(id) {
            Some(pending) if pending.effective(now) == PairingStatus::Pending => {
                pending.status = PairingStatus::Completed;
                pending.device_id = Some(device_id);
                true
            }
            _ => false,
        }
    }

    /// Finds the pairing a submitted code belongs to (management-api §2.12).
    pub fn submit(&self, code: PairingCode) -> Submit {
        let now = self.clock.now();
        let mut map = lock(&self.inner);
        let found = map
            .values_mut()
            .find(|pending| constant_time_eq(pending.code, code));
        match found {
            Some(pending) => match pending.effective(now) {
                PairingStatus::Pending => Submit::Matched {
                    id: pending.id.clone(),
                    host: pending.host.clone(),
                },
                PairingStatus::Expired => Submit::Expired {
                    id: pending.id.clone(),
                },
                // Completed (single use), cancelled or burned all read as an invalid code.
                _ => Submit::NoMatch,
            },
            None => Submit::NoMatch,
        }
    }

    /// Atomically claims a live, unclaimed pairing for a submission: only the first caller gets
    /// [`Submit::Matched`], so a Pairing code is single-use even under two simultaneous `pair`s.
    pub fn claim(&self, code: PairingCode) -> Submit {
        let now = self.clock.now();
        let mut map = lock(&self.inner);
        let found = map
            .values_mut()
            .find(|pending| constant_time_eq(pending.code, code));
        match found {
            Some(pending) => match pending.effective(now) {
                PairingStatus::Pending if !pending.claimed => {
                    pending.claimed = true;
                    Submit::Matched {
                        id: pending.id.clone(),
                        host: pending.host.clone(),
                    }
                }
                PairingStatus::Pending => Submit::NoMatch,
                PairingStatus::Expired => Submit::Expired {
                    id: pending.id.clone(),
                },
                // Completed (single use), cancelled or burned all read as an invalid code.
                _ => Submit::NoMatch,
            },
            None => Submit::NoMatch,
        }
    }

    /// Counts one wrong code against every live pairing, burning any that reaches
    /// [`MAX_ATTEMPTS`] (#21 D15). Returns the ids that burned.
    pub fn record_wrong_attempt(&self) -> Vec<String> {
        let now = self.clock.now();
        let mut map = lock(&self.inner);
        let mut burned = Vec::new();
        for pending in map.values_mut() {
            // A claimed pairing is already being completed by a successful `pair`; a wrong
            // attempt from another caller must not burn it out from under that call.
            if pending.claimed || pending.effective(now) != PairingStatus::Pending {
                continue;
            }
            pending.attempts = pending.attempts.saturating_add(1);
            if pending.attempts >= MAX_ATTEMPTS {
                pending.status = PairingStatus::Burned;
                burned.push(pending.id.clone());
            }
        }
        burned
    }

    /// Whether any pairing is still pending (used to short-circuit an idle long-poll).
    pub fn has_pending(&self) -> bool {
        let now = self.clock.now();
        lock(&self.inner)
            .values()
            .any(|pending| pending.effective(now) == PairingStatus::Pending)
    }
}

impl Default for PairingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Pending {
    /// The status with `expires_at` applied to a still-pending pairing.
    fn effective(&self, now: DateTime<Utc>) -> PairingStatus {
        if self.status == PairingStatus::Pending && now >= self.expires_at {
            PairingStatus::Expired
        } else {
            self.status
        }
    }

    fn view(&self, now: DateTime<Utc>) -> PairingView {
        PairingView {
            pairing_id: self.id.clone(),
            status: self.effective(now),
            expires_at: self.expires_at,
            host: self.host.clone(),
            device_id: self.device_id.clone(),
        }
    }
}

fn random_hex() -> Result<String, random::Unavailable> {
    let mut bytes = [0u8; 8];
    random::fill(&mut bytes)?;
    Ok(hex(&bytes))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn constant_time_eq(a: PairingCode, b: PairingCode) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(a.compact().as_bytes().ct_eq(b.compact().as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A clock a test moves by hand.
    #[derive(Default)]
    struct ManualClock(Mutex<DateTime<Utc>>);

    impl Clock for ManualClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn clock_start() -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, 16, 9, 0, 0).unwrap()
    }

    fn registry() -> (Arc<ManualClock>, PairingRegistry) {
        let clock = Arc::new(ManualClock(Mutex::new(clock_start())));
        let registry = PairingRegistry::with_clock(clock.clone());
        (clock, registry)
    }

    fn code_of(registry: &PairingRegistry, id: &str) -> PairingCode {
        let map = lock(&registry.inner);
        map[id].code
    }

    #[test]
    fn a_code_is_8_crockford_characters_shown_once() {
        let (_clock, registry) = registry();
        let created = registry.create(None).unwrap();
        assert_eq!(created.code.compact().len(), 8);
        assert_eq!(created.code.to_string().len(), 9); // XXXX-XXXX
        assert!(
            created
                .code
                .compact()
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        );
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Pending
        );
    }

    #[test]
    fn a_code_expires_after_ten_minutes_on_a_paused_clock() {
        let (clock, registry) = registry();
        let created = registry.create(None).unwrap();
        assert_eq!(created.expires_at, clock_start() + Duration::minutes(10));
        *clock.0.lock().unwrap() = clock_start() + Duration::minutes(9);
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Pending
        );
        assert_eq!(
            registry.submit(created.code),
            Submit::Matched {
                id: created.id.clone(),
                host: None,
            }
        );
        *clock.0.lock().unwrap() = clock_start() + Duration::minutes(10);
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Expired
        );
        assert_eq!(
            registry.submit(created.code),
            Submit::Expired {
                id: created.id.clone(),
            }
        );
    }

    #[test]
    fn a_failing_random_source_creates_no_code() {
        let registry = PairingRegistry::new();
        {
            let _fail = random::failing::enable();
            assert_eq!(registry.create(None).unwrap_err(), random::Unavailable);
        }
        assert!(lock(&registry.inner).is_empty());
        // The source recovers: the next code is created normally.
        assert!(registry.create(None).is_ok());
    }

    #[test]
    fn five_wrong_attempts_burn_the_code() {
        let (_clock, registry) = registry();
        let created = registry.create(None).unwrap();
        let wrong: PairingCode = "ZZZZ-ZZZZ".parse().unwrap();
        for attempt in 1..MAX_ATTEMPTS {
            assert!(
                registry.record_wrong_attempt().is_empty(),
                "attempt {attempt}"
            );
            assert_eq!(
                registry.get(&created.id).unwrap().status,
                PairingStatus::Pending
            );
        }
        assert_eq!(registry.record_wrong_attempt(), vec![created.id.clone()]);
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Burned
        );
        assert_eq!(registry.submit(created.code), Submit::NoMatch);
        assert_eq!(registry.submit(wrong), Submit::NoMatch);
    }

    #[test]
    fn a_wrong_code_does_not_match_and_counts() {
        let (_clock, registry) = registry();
        let created = registry.create(None).unwrap();
        let wrong: PairingCode = "0000-0001".parse().unwrap();
        assert_eq!(registry.submit(wrong), Submit::NoMatch);
        assert_eq!(registry.record_wrong_attempt(), Vec::<String>::new());
        assert_eq!(
            registry.submit(created.code),
            Submit::Matched {
                id: created.id.clone(),
                host: None,
            }
        );
    }

    #[test]
    fn a_code_is_single_use() {
        let (_clock, registry) = registry();
        let created = registry.create(Some("build-box".into())).unwrap();
        assert!(registry.complete(&created.id, "dev-1".into()));
        let view = registry.get(&created.id).unwrap();
        assert_eq!(view.status, PairingStatus::Completed);
        assert_eq!(view.device_id.as_deref(), Some("dev-1"));
        assert_eq!(view.host.as_deref(), Some("build-box"));
        assert_eq!(registry.submit(created.code), Submit::NoMatch);
        assert!(!registry.complete(&created.id, "dev-2".into()));
    }

    #[test]
    fn claim_hands_a_code_to_exactly_one_caller() {
        let (_clock, registry) = registry();
        let created = registry.create(None).unwrap();
        assert_eq!(
            registry.claim(created.code),
            Submit::Matched {
                id: created.id.clone(),
                host: None,
            }
        );
        // A second claim of the same code loses, so a second `pair` never pairs a second Device.
        assert_eq!(registry.claim(created.code), Submit::NoMatch);
        // A wrong attempt must not burn a claimed pairing out from under its `pair`.
        assert!(registry.record_wrong_attempt().is_empty());
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Pending
        );
        assert!(registry.complete(&created.id, "dev-1".into()));
    }

    #[test]
    fn cancel_ends_a_pending_pairing() {
        let (_clock, registry) = registry();
        let created = registry.create(None).unwrap();
        assert!(registry.cancel(&created.id));
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Cancelled
        );
        assert!(!registry.cancel(&created.id));
        assert_eq!(registry.submit(created.code), Submit::NoMatch);
        assert!(!registry.has_pending());
    }

    #[test]
    fn expired_codes_cannot_be_cancelled_or_completed() {
        let (clock, registry) = registry();
        let created = registry.create(None).unwrap();
        *clock.0.lock().unwrap() = clock_start() + Duration::minutes(11);
        assert!(!registry.cancel(&created.id));
        assert!(!registry.complete(&created.id, "dev-1".into()));
        assert_eq!(
            registry.get(&created.id).unwrap().status,
            PairingStatus::Expired
        );
    }

    #[test]
    fn unknown_ids_are_none() {
        let (_clock, registry) = registry();
        assert!(registry.get("pair_nope").is_none());
        assert!(!registry.cancel("pair_nope"));
        assert!(!registry.complete("pair_nope", "d".into()));
    }

    #[test]
    fn codes_are_unique() {
        let (_clock, registry) = registry();
        let codes: Vec<_> = (0..20)
            .map(|_| registry.create(None).unwrap().code)
            .collect();
        let unique: std::collections::HashSet<_> = codes.iter().map(PairingCode::compact).collect();
        assert_eq!(unique.len(), codes.len());
        let _ = code_of(&registry, &registry.create(None).unwrap().id);
    }
}
