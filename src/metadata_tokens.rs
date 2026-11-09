use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_TRANSFER_TIME_OFFSET: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);
const MAX_TRANSFER_CLOCK_SAMPLE_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TransferTimestamp {
    seconds: u64,
    nanoseconds: u32,
}

impl TransferTimestamp {
    fn from_wall(wall: SystemTime) -> Result<Self, String> {
        let elapsed = wall
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "transfer timestamp precedes Unix epoch".to_string())?;
        Ok(Self {
            seconds: elapsed.as_secs(),
            nanoseconds: elapsed.subsec_nanos(),
        })
    }

    fn to_wall(self) -> Result<SystemTime, String> {
        if self.nanoseconds >= 1_000_000_000 {
            return Err("invalid transfer timestamp nanoseconds".into());
        }
        UNIX_EPOCH
            .checked_add(Duration::new(self.seconds, self.nanoseconds))
            .ok_or_else(|| "transfer timestamp is out of range".into())
    }
}

pub(crate) struct TransferClock {
    instant: Instant,
    wall: SystemTime,
}

impl TransferClock {
    pub(crate) fn now() -> Result<Self, String> {
        for _ in 0..3 {
            let before = Instant::now();
            let wall = SystemTime::now();
            let after = Instant::now();
            if let Ok(clock) = Self::from_sample(before, wall, after) {
                return Ok(clock);
            }
        }
        Err("unable to sample transfer wall clock within 1 millisecond".into())
    }

    fn from_sample(before: Instant, wall: SystemTime, after: Instant) -> Result<Self, String> {
        let interval = after
            .checked_duration_since(before)
            .filter(|interval| *interval <= MAX_TRANSFER_CLOCK_SAMPLE_INTERVAL)
            .ok_or_else(|| "transfer wall clock sampling interval is out of range".to_string())?;
        let instant = before
            .checked_add(interval / 2)
            .ok_or_else(|| "transfer clock sampling exceeds monotonic clock range".to_string())?;
        Ok(Self { instant, wall })
    }

    #[cfg(test)]
    pub(crate) fn at(instant: Instant, wall: SystemTime) -> Self {
        Self { instant, wall }
    }

    pub(crate) fn captured_at(&self) -> Result<TransferTimestamp, String> {
        TransferTimestamp::from_wall(self.wall)
    }

    pub(crate) fn validate_capture(&self, captured_at: TransferTimestamp) -> Result<(), String> {
        let elapsed = self
            .wall
            .duration_since(captured_at.to_wall()?)
            .map_err(|_| "wall clock moved backwards since transfer capture".to_string())?;
        if elapsed > MAX_TRANSFER_TIME_OFFSET {
            return Err("transfer capture time is out of range".into());
        }
        Ok(())
    }

    pub(crate) fn encode(&self, instant: Instant) -> Result<TransferTimestamp, String> {
        let wall = if let Some(offset) = instant.checked_duration_since(self.instant) {
            Self::validate_offset(offset)?;
            self.wall.checked_add(offset)
        } else {
            let offset = self.instant.duration_since(instant);
            Self::validate_offset(offset)?;
            self.wall.checked_sub(offset)
        }
        .ok_or_else(|| "transfer timestamp is out of range".to_string())?;
        TransferTimestamp::from_wall(wall)
    }

    pub(crate) fn decode(&self, timestamp: TransferTimestamp) -> Result<Instant, String> {
        let wall = timestamp.to_wall()?;
        let instant = if let Ok(offset) = wall.duration_since(self.wall) {
            Self::validate_offset(offset)?;
            self.instant.checked_add(offset)
        } else {
            let offset = self
                .wall
                .duration_since(wall)
                .map_err(|_| "transfer timestamp is out of range".to_string())?;
            Self::validate_offset(offset)?;
            self.instant.checked_sub(offset)
        };
        instant.ok_or_else(|| "transfer timestamp exceeds monotonic clock range".into())
    }

    fn validate_offset(offset: Duration) -> Result<(), String> {
        if offset > MAX_TRANSFER_TIME_OFFSET {
            return Err("transfer timestamp exceeds supported 100-year clock range".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetadataTokenTransferState {
    value: String,
    expires_at: Option<TransferTimestamp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MetadataTokensTransferState {
    captured_at: TransferTimestamp,
    entries: HashMap<String, MetadataTokenTransferState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataToken {
    value: String,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MetadataTokens {
    entries: HashMap<String, MetadataToken>,
}

pub(crate) const MAX_SEQUENCE_SOURCES: usize = 32;

pub(crate) fn sequence_is_fresh(
    sequences: &HashMap<String, u64>,
    source: &str,
    seq: Option<u64>,
) -> bool {
    seq.is_none_or(|seq| sequences.get(source).is_none_or(|last| seq > *last))
}

pub(crate) fn accept_sequence(
    sequences: &mut HashMap<String, u64>,
    source: &str,
    seq: Option<u64>,
) -> Result<bool, ()> {
    let Some(seq) = seq else {
        return Ok(true);
    };
    if !sequence_is_fresh(sequences, source, Some(seq)) {
        return Ok(false);
    }
    if !sequences.contains_key(source) && sequences.len() >= MAX_SEQUENCE_SOURCES {
        return Err(());
    }
    sequences.insert(source.to_string(), seq);
    Ok(true)
}

impl Serialize for MetadataTokens {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let clock = TransferClock::now().map_err(serde::ser::Error::custom)?;
        self.transfer_state(&clock)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MetadataTokens {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let snapshot = MetadataTokensTransferState::deserialize(deserializer)?;
        let clock = TransferClock::now().map_err(serde::de::Error::custom)?;
        Self::from_transfer_state(snapshot, &clock).map_err(serde::de::Error::custom)
    }
}

impl MetadataTokens {
    pub(crate) fn transfer_state(
        &self,
        clock: &TransferClock,
    ) -> Result<MetadataTokensTransferState, String> {
        Ok(MetadataTokensTransferState {
            captured_at: clock.captured_at()?,
            entries: self
                .entries
                .iter()
                .map(|(key, token)| {
                    Ok((
                        key.clone(),
                        MetadataTokenTransferState {
                            value: token.value.clone(),
                            expires_at: token.expires_at.map(|at| clock.encode(at)).transpose()?,
                        },
                    ))
                })
                .collect::<Result<_, String>>()?,
        })
    }

    pub(crate) fn from_transfer_state(
        snapshot: MetadataTokensTransferState,
        clock: &TransferClock,
    ) -> Result<Self, String> {
        clock.validate_capture(snapshot.captured_at)?;
        Ok(Self {
            entries: snapshot
                .entries
                .into_iter()
                .map(|(key, token)| {
                    Ok((
                        key,
                        MetadataToken {
                            value: token.value,
                            expires_at: token.expires_at.map(|at| clock.decode(at)).transpose()?,
                        },
                    ))
                })
                .collect::<Result<_, String>>()?,
        })
    }

    pub(crate) fn patch(
        &mut self,
        patch: HashMap<String, Option<String>>,
        ttl: Option<Duration>,
        now: Instant,
    ) -> bool {
        let expires_at = ttl.and_then(|ttl| now.checked_add(ttl));
        let mut changed = false;
        for (key, value) in patch {
            match value {
                Some(value) => {
                    let token = MetadataToken { value, expires_at };
                    if self.entries.get(&key) != Some(&token) {
                        self.entries.insert(key, token);
                        changed = true;
                    }
                }
                None => {
                    changed |= self.entries.remove(&key).is_some();
                }
            }
        }
        changed
    }

    pub(crate) fn key_count_after_patch(&self, patch: &HashMap<String, Option<String>>) -> usize {
        let mut keys = self
            .values()
            .into_keys()
            .collect::<std::collections::HashSet<_>>();
        for (key, value) in patch {
            if value.is_some() {
                keys.insert(key.clone());
            } else {
                keys.remove(key);
            }
        }
        keys.len()
    }

    pub(crate) fn values(&self) -> HashMap<String, String> {
        self.entries
            .iter()
            .map(|(key, token)| (key.clone(), token.value.clone()))
            .collect()
    }

    pub(crate) fn next_expiry(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|token| token.expires_at)
            .min()
    }

    pub(crate) fn expire_at(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, token| token.expires_at.is_none_or(|deadline| deadline > now));
        self.entries.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(items: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        items
            .iter()
            .map(|(key, value)| ((*key).into(), value.map(str::to_string)))
            .collect()
    }

    #[test]
    fn workspace_transfer_clock_sampling_rejects_excessive_skew() {
        let before = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let after = before + MAX_TRANSFER_CLOCK_SAMPLE_INTERVAL;
        let clock = TransferClock::from_sample(before, wall, after).unwrap();
        assert_eq!(
            clock.instant,
            before + MAX_TRANSFER_CLOCK_SAMPLE_INTERVAL / 2
        );
        assert!(TransferClock::from_sample(before, wall, after + Duration::from_nanos(1)).is_err());
        assert!(TransferClock::from_sample(after, wall, before).is_err());
        let boundary = TransferTimestamp {
            seconds: 1_800_000_000 + MAX_TRANSFER_TIME_OFFSET.as_secs(),
            nanoseconds: 0,
        };
        let instant = clock.decode(boundary).unwrap();
        assert_eq!(
            instant.duration_since(clock.instant),
            MAX_TRANSFER_TIME_OFFSET
        );
        assert_eq!(clock.encode(instant).unwrap(), boundary);
        assert!(clock
            .decode(TransferTimestamp {
                seconds: 1_800_000_000 + MAX_TRANSFER_TIME_OFFSET.as_secs(),
                nanoseconds: 1,
            })
            .is_err());
    }

    #[test]
    fn workspace_transfer_tokens_roundtrip_absolute_deadlines_without_refresh() {
        let now = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let source_clock = TransferClock::at(now, wall);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("short", Some("temporary"))]),
            Some(Duration::from_secs(10)),
            now - Duration::from_secs(7),
        );
        tokens.patch(
            patch(&[("expired", Some("pending sweep"))]),
            Some(Duration::ZERO),
            now - Duration::from_secs(2),
        );
        tokens.patch(patch(&[("permanent", Some("keep"))]), None, now);
        let snapshot = tokens.transfer_state(&source_clock).unwrap();
        let json = serde_json::to_vec(&snapshot).unwrap();
        let decoded = serde_json::from_slice(&json).unwrap();
        let arrival = now + Duration::from_secs(700);
        let destination_clock = TransferClock::at(arrival, wall + Duration::from_secs(2));
        let mut restored =
            MetadataTokens::from_transfer_state(decoded, &destination_clock).unwrap();

        assert_eq!(restored.values(), tokens.values());
        assert_eq!(
            restored.next_expiry(),
            Some(arrival - Duration::from_secs(4))
        );
        assert!(restored.expire_at(arrival));
        assert_eq!(
            restored.next_expiry(),
            Some(arrival + Duration::from_secs(1))
        );
        assert!(!restored.expire_at(arrival + Duration::from_secs(1) - Duration::from_nanos(1)));
        assert!(restored.expire_at(arrival + Duration::from_secs(1)));
        assert_eq!(
            restored.values(),
            HashMap::from([("permanent".into(), "keep".into())])
        );
    }

    #[test]
    fn workspace_transfer_tokens_support_direct_serde_with_deadlines() {
        let now = Instant::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("short", Some("temporary"))]),
            Some(Duration::from_secs(30)),
            now - Duration::from_secs(10),
        );
        tokens.patch(patch(&[("permanent", Some("keep"))]), None, now);
        let json = serde_json::to_vec(&tokens).unwrap();
        let restored: MetadataTokens = serde_json::from_slice(&json).unwrap();
        assert_eq!(restored.values(), tokens.values());
        assert!(restored.next_expiry().unwrap() < now + Duration::from_secs(21));
        assert!(restored.next_expiry().unwrap() > now + Duration::from_secs(19));
        let encoded = bincode::serde::encode_to_vec(&tokens, bincode::config::standard()).unwrap();
        let (restored, consumed): (MetadataTokens, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(restored.values(), tokens.values());
        assert!(restored.next_expiry().unwrap() < now + Duration::from_secs(21));
    }

    #[test]
    fn workspace_transfer_tokens_reject_invalid_deadlines_and_clock_rollback() {
        let now = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let clock = TransferClock::at(now, wall);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("short", Some("temporary"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        let snapshot = tokens.transfer_state(&clock).unwrap();
        let backwards = TransferClock::at(now, wall - Duration::from_nanos(1));
        assert!(
            MetadataTokens::from_transfer_state(snapshot.clone(), &backwards)
                .unwrap_err()
                .contains("backwards")
        );
        let mut invalid = snapshot;
        invalid.entries.get_mut("short").unwrap().expires_at = Some(TransferTimestamp {
            seconds: u64::MAX,
            nanoseconds: 0,
        });
        assert!(MetadataTokens::from_transfer_state(invalid, &clock).is_err());
        let before_epoch = TransferClock::at(now, UNIX_EPOCH);
        assert!(before_epoch.encode(now - Duration::from_secs(1)).is_err());
        assert!(clock
            .decode(TransferTimestamp {
                seconds: 1_800_000_000,
                nanoseconds: 1_000_000_000
            })
            .is_err());
    }

    #[test]
    fn sequence_sources_are_bounded() {
        let mut sequences = HashMap::new();
        for index in 0..MAX_SEQUENCE_SOURCES {
            assert_eq!(
                accept_sequence(&mut sequences, &format!("source-{index}"), Some(1)),
                Ok(true)
            );
        }
        assert_eq!(
            accept_sequence(&mut sequences, "one-too-many", Some(1)),
            Err(())
        );
        assert_eq!(
            accept_sequence(&mut sequences, "source-0", Some(1)),
            Ok(false)
        );
        assert_eq!(
            accept_sequence(&mut sequences, "source-0", Some(2)),
            Ok(true)
        );
    }

    #[test]
    fn patches_and_clears_individual_keys() {
        let now = Instant::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("one")), ("model", Some("opus"))]),
            None,
            now,
        );
        tokens.patch(
            patch(&[("summary", Some("two")), ("model", None)]),
            None,
            now,
        );

        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "two".into())])
        );
    }

    #[test]
    fn ttl_only_changes_keys_in_the_patch() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("short", Some("one"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(patch(&[("persistent", Some("two"))]), None, now);

        assert!(tokens.expire_at(deadline));
        assert_eq!(
            tokens.values(),
            HashMap::from([("persistent".into(), "two".into())])
        );
    }

    #[test]
    fn values_remain_stable_until_expiry_mutates_state() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("temporary"))]),
            Some(Duration::from_secs(1)),
            now,
        );

        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "temporary".into())])
        );
        assert!(tokens.expire_at(deadline));
        assert!(tokens.values().is_empty());
    }

    #[test]
    fn delayed_expiry_sweep_removes_every_token_due_at_now() {
        let now = Instant::now();
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("first", Some("one"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(
            patch(&[("second", Some("two"))]),
            Some(Duration::from_secs(2)),
            now,
        );

        assert!(tokens.expire_at(now + Duration::from_secs(10)));
        assert!(tokens.values().is_empty());
    }

    #[test]
    fn stale_expiry_does_not_clear_replacement() {
        let now = Instant::now();
        let first_deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("old"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(
            patch(&[("summary", Some("new"))]),
            Some(Duration::from_secs(5)),
            now,
        );

        assert!(!tokens.expire_at(first_deadline));
        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "new".into())])
        );
    }

    #[test]
    fn update_without_ttl_cancels_previous_expiry() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let mut tokens = MetadataTokens::default();
        tokens.patch(
            patch(&[("summary", Some("temporary"))]),
            Some(Duration::from_secs(1)),
            now,
        );
        tokens.patch(patch(&[("summary", Some("persistent"))]), None, now);

        assert!(!tokens.expire_at(deadline));
        assert_eq!(
            tokens.values(),
            HashMap::from([("summary".into(), "persistent".into())])
        );
    }
}
