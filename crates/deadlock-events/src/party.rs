//! Turning successive party snapshots into join and leave events.

use std::collections::HashMap;

use valveprotos::deadlock::CsoCitadelParty as Party;

/// Something that changed about the party.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PartyEvent {
    /// A party appeared, or replaced a different one.
    Formed {
        /// Party identity.
        party_id: u64,
        /// Everyone in it at the moment it was first seen.
        members: Vec<u32>,
    },
    /// The party went away.
    ///
    /// Either it was genuinely disbanded, or the object stopped being resident. Those are
    /// not distinguishable from the client's heap, so do not read this as proof that
    /// anyone did anything.
    Disbanded {
        /// The party that is no longer visible.
        party_id: u64,
    },
    /// Someone joined.
    MemberJoined {
        /// Steam account id.
        account_id: u32,
        /// Their persona name, if the record carried one.
        persona_name: Option<String>,
    },
    /// Someone left.
    ///
    /// This fires whether or not they are still in a match with you, which is the case
    /// the entity system cannot answer at all.
    MemberLeft {
        /// Steam account id.
        account_id: u32,
        /// Their persona name, from the roster before they left.
        persona_name: Option<String>,
    },
    /// Someone readied up or un-readied.
    ReadyChanged {
        /// Steam account id.
        account_id: u32,
        /// Whether they are ready now.
        is_ready: bool,
    },
}

/// One member's state, reduced to what the differ compares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MemberState {
    persona_name: Option<String>,
    is_ready: Option<bool>,
}

/// Diffs consecutive party readings.
///
/// Feed it every reading, including `None` for "no party found". It holds one tick of
/// history.
#[derive(Debug, Default)]
pub struct PartyTracker {
    party_id: Option<u64>,
    members: HashMap<u32, MemberState>,
}

impl PartyTracker {
    /// A tracker with no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the previous reading.
    pub fn reset(&mut self) {
        self.party_id = None;
        self.members.clear();
    }

    /// The party id currently being tracked.
    pub fn party_id(&self) -> Option<u64> {
        self.party_id
    }

    /// Compare a reading against the previous one.
    ///
    /// A first sighting reports [`PartyEvent::Formed`] listing who is present, and does
    /// not manufacture a join for each of them: they did not join while anyone was
    /// watching, and reporting otherwise would produce a burst of events every time the
    /// process starts.
    pub fn update(&mut self, party: Option<&Party>) -> Vec<PartyEvent> {
        let mut out = Vec::new();

        let Some(party) = party else {
            if let Some(old) = self.party_id.take() {
                out.push(PartyEvent::Disbanded { party_id: old });
            }
            self.members.clear();
            return out;
        };

        // A party with no id is not one this can track; the id is what makes successive
        // readings comparable.
        let Some(id) = party.party_id else {
            return out;
        };

        let next: HashMap<u32, MemberState> = party
            .members
            .iter()
            .filter_map(|m| {
                Some((
                    m.account_id?,
                    MemberState {
                        persona_name: m.persona_name.clone(),
                        is_ready: m.is_ready,
                    },
                ))
            })
            .collect();

        match self.party_id {
            Some(prev) if prev == id => {
                let mut joined: Vec<u32> = next
                    .keys()
                    .copied()
                    .filter(|a| !self.members.contains_key(a))
                    .collect();
                let mut left: Vec<u32> = self
                    .members
                    .keys()
                    .copied()
                    .filter(|a| !next.contains_key(a))
                    .collect();
                // HashMap order is not stable; sort so the stream is reproducible.
                joined.sort_unstable();
                left.sort_unstable();

                for a in left {
                    out.push(PartyEvent::MemberLeft {
                        account_id: a,
                        persona_name: self.members[&a].persona_name.clone(),
                    });
                }
                for a in joined {
                    out.push(PartyEvent::MemberJoined {
                        account_id: a,
                        persona_name: next[&a].persona_name.clone(),
                    });
                }

                let mut ready: Vec<(u32, bool)> = next
                    .iter()
                    .filter_map(|(a, m)| {
                        let was = self.members.get(a)?.is_ready;
                        let now = m.is_ready?;
                        (was != Some(now)).then_some((*a, now))
                    })
                    .collect();
                ready.sort_unstable();
                for (account_id, is_ready) in ready {
                    out.push(PartyEvent::ReadyChanged {
                        account_id,
                        is_ready,
                    });
                }
            }
            prev => {
                if let Some(old) = prev {
                    out.push(PartyEvent::Disbanded { party_id: old });
                }
                let mut members: Vec<u32> = next.keys().copied().collect();
                members.sort_unstable();
                out.push(PartyEvent::Formed {
                    party_id: id,
                    members,
                });
            }
        }

        self.party_id = Some(id);
        self.members = next;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valveprotos::deadlock::cso_citadel_party::Member;

    fn member(account_id: u32, name: &str, ready: Option<bool>) -> Member {
        Member {
            account_id: Some(account_id),
            persona_name: Some(name.to_string()),
            is_ready: ready,
            ..Default::default()
        }
    }

    fn party(id: u64, members: Vec<Member>) -> Party {
        Party {
            party_id: Some(id),
            members,
            ..Default::default()
        }
    }

    #[test]
    fn a_first_sighting_reports_the_roster_without_synthetic_joins() {
        let mut t = PartyTracker::new();
        let p = party(
            7,
            vec![member(111, "alice", None), member(222, "bob", None)],
        );
        assert_eq!(
            t.update(Some(&p)),
            vec![PartyEvent::Formed {
                party_id: 7,
                members: vec![111, 222],
            }]
        );
    }

    #[test]
    fn a_steady_party_produces_nothing() {
        let mut t = PartyTracker::new();
        let p = party(7, vec![member(111, "alice", None)]);
        t.update(Some(&p));
        assert!(t.update(Some(&p)).is_empty());
    }

    #[test]
    fn someone_joining_is_reported() {
        let mut t = PartyTracker::new();
        t.update(Some(&party(7, vec![member(111, "alice", None)])));
        let ev = t.update(Some(&party(
            7,
            vec![member(111, "alice", None), member(222, "bob", None)],
        )));
        assert_eq!(
            ev,
            vec![PartyEvent::MemberJoined {
                account_id: 222,
                persona_name: Some("bob".into()),
            }]
        );
    }

    /// The case that motivated the whole GC detour: someone leaves the party while
    /// everyone is still in the same match, so a match roster cannot show it.
    #[test]
    fn someone_leaving_is_reported_with_the_name_they_had() {
        let mut t = PartyTracker::new();
        t.update(Some(&party(
            7,
            vec![member(111, "alice", None), member(222, "bob", None)],
        )));
        let ev = t.update(Some(&party(7, vec![member(111, "alice", None)])));
        assert_eq!(
            ev,
            vec![PartyEvent::MemberLeft {
                account_id: 222,
                persona_name: Some("bob".into()),
            }],
            "the name has to come from the roster before they left"
        );
    }

    #[test]
    fn ready_state_changes_are_reported_once() {
        let mut t = PartyTracker::new();
        t.update(Some(&party(7, vec![member(111, "alice", Some(false))])));
        let ev = t.update(Some(&party(7, vec![member(111, "alice", Some(true))])));
        assert_eq!(
            ev,
            vec![PartyEvent::ReadyChanged {
                account_id: 111,
                is_ready: true,
            }]
        );
        assert!(
            t.update(Some(&party(7, vec![member(111, "alice", Some(true))])))
                .is_empty()
        );
    }

    #[test]
    fn a_new_party_disbands_the_old_one() {
        let mut t = PartyTracker::new();
        t.update(Some(&party(7, vec![member(111, "alice", None)])));
        let ev = t.update(Some(&party(8, vec![member(111, "alice", None)])));
        assert_eq!(
            ev,
            vec![
                PartyEvent::Disbanded { party_id: 7 },
                PartyEvent::Formed {
                    party_id: 8,
                    members: vec![111],
                },
            ]
        );
    }

    #[test]
    fn losing_the_party_reports_disbanded_once() {
        let mut t = PartyTracker::new();
        t.update(Some(&party(7, vec![member(111, "alice", None)])));
        assert_eq!(t.update(None), vec![PartyEvent::Disbanded { party_id: 7 }]);
        assert!(t.update(None).is_empty(), "and not again every tick");
    }

    /// Solo play means no party at all, which must be silent rather than a stream of
    /// disband events.
    #[test]
    fn never_having_a_party_is_silent() {
        let mut t = PartyTracker::new();
        assert!(t.update(None).is_empty());
        assert!(t.update(None).is_empty());
        assert_eq!(t.party_id(), None);
    }

    /// A party with no id cannot be compared against the next reading, so it is ignored
    /// rather than treated as a new party every tick.
    #[test]
    fn an_id_less_party_is_ignored() {
        let mut t = PartyTracker::new();
        let p = Party {
            party_id: None,
            members: vec![member(111, "alice", None)],
            ..Default::default()
        };
        assert!(t.update(Some(&p)).is_empty());
        assert_eq!(t.party_id(), None);
    }

    #[test]
    fn simultaneous_join_and_leave_are_both_reported() {
        let mut t = PartyTracker::new();
        t.update(Some(&party(
            7,
            vec![member(111, "alice", None), member(222, "bob", None)],
        )));
        let ev = t.update(Some(&party(
            7,
            vec![member(111, "alice", None), member(333, "carol", None)],
        )));
        assert_eq!(ev.len(), 2);
        assert!(ev.contains(&PartyEvent::MemberLeft {
            account_id: 222,
            persona_name: Some("bob".into()),
        }));
        assert!(ev.contains(&PartyEvent::MemberJoined {
            account_id: 333,
            persona_name: Some("carol".into()),
        }));
    }
}
