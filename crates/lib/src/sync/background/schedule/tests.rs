use rand::SeedableRng;

use super::*;
use crate::{
    PrivateKey,
    sync::background::{MAX_CONCURRENT_PEER_SYNCS, TreeReport},
};

fn peer() -> PeerId {
    PeerId::new(PrivateKey::generate().public_key())
}

fn tree(name: &str) -> ID {
    ID::from_bytes(name)
}

fn started(now: Instant) -> Schedule {
    let mut schedule = Schedule::new(now, StdRng::seed_from_u64(42));
    schedule.refresh(HashMap::new(), now);
    schedule
}

fn success(trees: &[&ID]) -> RoundReport {
    RoundReport {
        trees: trees
            .iter()
            .map(|&tree| TreeReport {
                tree: tree.clone(),
                attempted_ms: Some(11),
                success_ms: Some(12),
            })
            .collect(),
        reached_peer: true,
        ..RoundReport::default()
    }
}

fn network_failure() -> RoundReport {
    RoundReport {
        network_failure: true,
        ..RoundReport::default()
    }
}

#[test]
fn unrelated_peer_stays_asleep_but_shared_slow_tree_coalesces() {
    // T1/T2: run after initial enrollment; startup catch-up is separate.
    let now = Instant::now();
    let mut schedule = started(now);
    let (fast, slow) = (peer(), peer());
    let (x, y) = (tree("x"), tree("y"));
    schedule.update_peer(&fast, [(x.clone(), 5), (y.clone(), 3600)].into(), now);
    schedule.update_peer(&slow, [(y.clone(), 3600)].into(), now);
    for _ in 0..2 {
        let p = schedule.due_peer(now).unwrap().clone();
        schedule.start(&p, now);
        schedule.finish(
            &p,
            now,
            if p == fast {
                success(&[&x, &y])
            } else {
                success(&[&y])
            },
        );
    }
    let mut rounds = 0;
    while let Some(due) = schedule
        .wake_at(now, false)
        .filter(|at| *at <= now + Duration::from_secs(60))
    {
        assert_eq!(schedule.due_peer(due), Some(&fast));
        schedule.start(&fast, due);
        assert_eq!(schedule.peers[&fast].pairs.len(), 2);
        schedule.finish(&fast, due, success(&[&x, &y]));
        assert!(schedule.peers[&fast].pairs[&y].due >= due + Duration::from_secs(2700));
        rounds += 1;
    }
    assert!((9..=16).contains(&rounds), "ran {rounds} fast rounds");
}

#[test]
fn zero_and_daily_intervals_use_the_clamped_jitter_range() {
    // T3/T13: clamp before jitter (including zero and very large u64).
    for (raw, min, max) in [
        (0, 3750, 6250),
        (86400, 2700000, 4500000),
        (u64::MAX, 2700000, 4500000),
    ] {
        let now = Instant::now();
        let mut schedule = started(now);
        let (p, d) = (peer(), tree("configured"));
        schedule.update_peer(&p, [(d.clone(), raw)].into(), now);
        schedule.start(&p, now);
        schedule.finish(&p, now, success(&[&d]));
        let delay = schedule.wake_at(now, false).unwrap() - now;
        assert!((Duration::from_millis(min)..=Duration::from_millis(max)).contains(&delay));
    }
}

#[test]
fn settings_changes_do_not_push_an_attempt_away() {
    // T6: alternate periods every minute, including before the first due time.
    let start = Instant::now();
    let mut schedule = started(start);
    let (p, d) = (peer(), tree("flap"));
    schedule.update_peer(&p, [(d.clone(), 90)].into(), start);
    schedule.start(&p, start);
    schedule.finish(&p, start, success(&[&d]));
    let mut attempts = 1;
    for minute in 1..=10 {
        let now = start + Duration::from_secs(minute * 60);
        let pending = schedule.wake_at(now, false).unwrap();
        let interval = if minute % 2 == 0 { 90 } else { 120 };
        schedule.refresh([(p.clone(), [(d.clone(), interval)].into())].into(), now);
        assert!(schedule.wake_at(now, false).unwrap() <= pending);
        if schedule.due_peer(now).is_some() {
            schedule.start(&p, now);
            schedule.finish(&p, now, success(&[&d]));
            attempts += 1;
        }
        assert_eq!(schedule.ready.len(), 1, "no stale queue entries");
    }
    assert!(attempts >= 5, "only {attempts} attempts in 10 minutes");
}

#[test]
fn overdue_pair_catches_up_once_from_completion() {
    // T7: a long suspend doesn't leave a queue of missed ticks.
    let start = Instant::now();
    let mut schedule = started(start);
    let (p, d) = (peer(), tree("suspended"));
    schedule.update_peer(&p, [(d.clone(), 5)].into(), start);
    schedule.start(&p, start);
    schedule.finish(&p, start, success(&[&d]));
    let resumed = start + Duration::from_secs(80);
    assert_eq!(schedule.due_peer(resumed), Some(&p));
    schedule.start(&p, resumed);
    let completed = resumed + Duration::from_secs(5);
    schedule.finish(&p, completed, success(&[&d]));
    assert!(schedule.due_peer(completed).is_none());
    assert!(schedule.wake_at(completed, false).unwrap() >= completed + Duration::from_millis(3750));
}

#[test]
fn failed_peers_back_off_without_affecting_healthy_peers_or_giving_up() {
    // T8: measure uncapped and capped portions; compare the healthy peer's
    // scheduled instants before and after each dead peer failure.
    let start = Instant::now();
    let mut schedule = started(start);
    let (dead, live) = (peer(), peer());
    let d = tree("shared");
    schedule.update_peer(&dead, [(d.clone(), 300)].into(), start);
    schedule.update_peer(&live, [(d.clone(), 3600)].into(), start);
    let mut counts = (0_i32, 0);
    while let Some(now) = schedule
        .wake_at(start, false)
        .filter(|at| *at < start + Duration::from_secs(21600))
    {
        let p = schedule.due_peer(now).unwrap().clone();
        schedule.start(&p, now);
        if p == dead {
            counts.0 += 1;
            let live_due = schedule.peers[&live].queued_at;
            schedule.finish(&p, now, network_failure());
            assert_eq!(schedule.peers[&live].queued_at, live_due);
            let delay = schedule.peers[&p].not_before - now;
            let base = f64::from((300 * (1 << counts.0.min(5))).min(3600));
            assert!((base * 0.75..=base * 1.25).contains(&delay.as_secs_f64()));
        } else {
            counts.1 += 1;
            schedule.finish(&p, now, success(&[&d]));
            assert_eq!(schedule.peers[&p].failures, 0);
        }
    }
    assert!(counts.0 >= 6 && counts.1 >= 5, "attempts: {counts:?}");
    // Continue beyond any prior retry limit, then recover on the next start.
    schedule.update_peer(&live, Periods::new(), start);
    for _ in 0..16 {
        let now = schedule.wake_at(start, false).unwrap();
        schedule.start(&dead, now);
        schedule.finish(&dead, now, network_failure());
    }
    let now = schedule.wake_at(start, false).unwrap();
    schedule.start(&dead, now);
    schedule.finish(&dead, now, success(&[&d]));
    assert_eq!(schedule.peers[&dead].failures, 0);
}

#[test]
fn saturation_keeps_oldest_eligible_work_and_charges_actual_starts() {
    // T9/T10: saturate both budgets with 50 fast peers. An hourly peer
    // falls due while saturated; returning fast peers cannot overtake it.
    let now = Instant::now();
    let mut schedule = started(now);
    let d = tree("shared");
    let hourly = peer();
    for _ in 0..50 {
        schedule.update_peer(&peer(), [(d.clone(), 5)].into(), now);
    }
    schedule.update_peer(&hourly, [(d.clone(), 3600)].into(), now + TOKEN);
    let first_wave = |schedule: &mut Schedule| {
        let peers: Vec<_> = (0..MAX_CONCURRENT_PEER_SYNCS)
            .map(|_| {
                let p = schedule.due_peer(now).unwrap().clone();
                schedule.start(&p, now);
                p
            })
            .collect();
        assert_eq!(schedule.wake_at(now, true), None);
        for p in peers {
            schedule.finish(&p, now, success(&[&d]));
        }
    };
    first_wave(&mut schedule);
    assert_eq!(schedule.wake_at(now, false), Some(now));
    first_wave(&mut schedule);
    assert!(
        schedule.due_peer(now).is_none(),
        "only 32 instantaneous starts"
    );
    assert_eq!(schedule.wake_at(now, false), Some(now + TOKEN));
    assert_eq!(schedule.concurrency_deferrals, 2);
    assert_eq!(schedule.rate_deferrals, 1);
    for second in 1..=19 {
        let at = now + Duration::from_secs(second);
        let p = schedule
            .due_peer(at)
            .expect("one token refills per second")
            .clone();
        assert_eq!(
            p == hourly,
            second == 19,
            "served the hourly peer before any repeat"
        );
        assert!(
            schedule.peers[&p].pairs[&d].last_attempt_ms.is_none(),
            "oldest queued peers go before repeats"
        );
        schedule.start(&p, at);
        schedule.finish(&p, at, success(&[&d]));
        assert!(schedule.due_peer(at).is_none());
    }
}

#[test]
fn independent_starts_spread_each_peer_once_within_startup_window() {
    // T11/T12: startup is jittered by peer, not one synchronized fleet round.
    let now = Instant::now();
    let d = tree("startup");
    let peers: HashMap<_, _> = (0..3)
        .map(|_| (peer(), [(d.clone(), 3600)].into()))
        .collect();
    let due_times = |seed| {
        let mut schedule = Schedule::new(now, StdRng::seed_from_u64(seed));
        schedule.refresh(peers.clone(), now);
        let mut deadlines = Vec::new();
        for _ in 0..peers.len() {
            let due = schedule.wake_at(now, false).unwrap();
            assert!(due <= now + SETTINGS_CHECK);
            let p = schedule.due_peer(due).unwrap().clone();
            schedule.start(&p, due);
            schedule.finish(&p, due, success(&[&d]));
            deadlines.push(due);
        }
        assert!(schedule.wake_at(now, false).unwrap() > now + SETTINGS_CHECK);
        assert!(deadlines.windows(2).all(|pair| pair[0] < pair[1]));
        deadlines
    };
    assert_ne!(due_times(42), due_times(4242));
}

#[test]
fn partial_network_failure_does_not_advance_unfinished_pairs() {
    let now = Instant::now();
    let mut schedule = started(now);
    let (p, good, missing) = (peer(), tree("completed"), tree("skipped"));
    schedule.update_peer(&p, [(good.clone(), 5), (missing.clone(), 5)].into(), now);
    schedule.start(&p, now);
    let mut report = success(&[&good]);
    report.network_failure = true;
    schedule.finish(&p, now, report);
    let peer = &schedule.peers[&p];
    assert_eq!(peer.failures, 1);
    assert_eq!(peer.pairs[&missing].due, now);
    assert_eq!(peer.pairs[&missing].last_attempt_ms, None);
    assert!(peer.pairs[&good].due > now);
    assert_eq!(peer.pairs[&good].last_success_ms, Some(12));
}

#[test]
fn local_only_and_individual_tree_failures_leave_due_work_pending() {
    let now = Instant::now();
    let mut schedule = started(now);
    let (p, d) = (peer(), tree("not served"));
    schedule.update_peer(&p, [(d.clone(), 5)].into(), now);
    schedule.defer_refresh(&p, now);
    assert_eq!(schedule.peers[&p].pairs[&d].due, now);
    assert_eq!(schedule.wake_at(now, false), Some(now + SETTINGS_CHECK));
    let begin = now + SETTINGS_CHECK;
    schedule.start(&p, begin);
    assert!(schedule.due_peer(begin).is_none(), "already in flight");
    let completed = begin + Duration::from_secs(5);
    schedule.finish(
        &p,
        completed,
        RoundReport {
            trees: vec![TreeReport {
                tree: d.clone(),
                attempted_ms: Some(30),
                success_ms: None,
            }],
            reached_peer: true,
            ..RoundReport::default()
        },
    );
    assert_eq!(schedule.peers[&p].pairs[&d].due, now);
    assert_eq!(schedule.peers[&p].pairs[&d].last_success_ms, None);
    assert_eq!(schedule.peers[&p].failures, 0);
    assert_eq!(
        schedule.wake_at(completed, false),
        Some(completed + SPACING)
    );
    // T14 scheduling edge: losing the only due candidate for a peer must
    // cancel that wake even when other registrations for the peer remain.
    let keep = tree("still shared");
    let retry = completed + SPACING;
    schedule.update_peer(&p, [(d.clone(), 5), (keep.clone(), 300)].into(), retry);
    schedule.start(&p, retry);
    schedule.finish(&p, retry, success(&[&keep]));
    let retry = retry + SPACING;
    assert_eq!(schedule.due_peer(retry), Some(&p));
    schedule.update_peer(&p, [(keep, 300)].into(), retry);
    assert!(schedule.due_peer(retry).is_none());
}
