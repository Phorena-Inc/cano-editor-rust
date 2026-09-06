//! EasyMotion-style jump targets for the `s` and `t` motions.
//!
//! Both motions ask for one character, label every occurrence the user can
//! see, and jump to whichever label is typed next.  The two differ in what
//! counts as an occurrence, following vim-easymotion: `s` finds the character
//! in both directions, while `t` is forward only and lands one byte before
//! the match.
//!
//! Everything here is pure: the caller supplies the buffer, the visible byte
//! range and the cursor, so the search never has to know about rendering.

/// The label alphabet, in the order labels are handed out.
///
/// This is vim-easymotion's `g:EasyMotion_keys` default: home row first, so
/// the nearest targets are the cheapest to reach.
pub const KEYS: &[u8] = b"asdghklqwertyuiopzxcvbnmfj;";

/// Which motion asked for the targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// `s`: find the character forward and backward.
    Find,
    /// `t`: till before the character, forward only.
    Till,
}

/// One labeled jump destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    /// The matched byte, which is where the label is drawn.
    pub match_start: usize,
    /// Where the cursor lands, which `t` pulls one byte back from the match.
    pub destination: usize,
    /// The keys that select this target.
    pub label: Vec<u8>,
}

/// Collects and labels every occurrence of `needle` the user can see.
///
/// `visible` is the byte range the last frame drew; targets outside it are
/// dropped because a label nobody can read is not reachable.  Targets are
/// labeled nearest-first, so the closest match always takes one keystroke.
pub fn targets(
    data: &[u8],
    visible: (usize, usize),
    cursor: usize,
    kind: Kind,
    needle: u8,
) -> Vec<Target> {
    let (start, end) = (visible.0.min(data.len()), visible.1.min(data.len()));
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (offset, byte) in data[start..end].iter().enumerate() {
        if *byte != needle {
            continue;
        }
        let index = start + offset;
        let destination = match kind {
            // The cursor is already on this byte, so a label for it would do
            // nothing.
            Kind::Find if index == cursor => continue,
            Kind::Find => index,
            // `t` is forward only, and it has to leave room to stop short of
            // the match: the byte right after the cursor is already adjacent.
            Kind::Till if index > cursor.saturating_add(1) => index - 1,
            Kind::Till => continue,
        };
        found.push((index, destination));
    }

    // Nearest first, and forward before backward when a match on each side is
    // equidistant, so the labels stay predictable while typing.
    found.sort_by_key(|(index, _)| (index.abs_diff(cursor), *index < cursor, *index));

    let labels = labels(found.len(), KEYS);
    found
        .into_iter()
        .zip(labels)
        .map(|((match_start, destination), label)| Target {
            match_start,
            destination,
            label,
        })
        .collect()
}

/// Builds `count` distinct labels, shortest first.
///
/// As many targets as there are keys get a single keystroke.  Beyond that the
/// trailing keys become prefixes for groups that are labeled the same way
/// again, which keeps every label unique and never leaves one label a prefix
/// of another: the keys used for singles and the keys used for prefixes are
/// always disjoint.
pub fn labels(count: usize, keys: &[u8]) -> Vec<Vec<u8>> {
    if count == 0 {
        return Vec::new();
    }
    // Two labels can only avoid being prefixes of one another if there are at
    // least two keys to build them from, so a smaller alphabet can address a
    // single target at most.  Callers zip labels with targets, so the rest
    // are simply not reachable.
    if keys.len() < 2 {
        return keys.first().map(|key| vec![*key]).into_iter().collect();
    }
    if count <= keys.len() {
        return keys[..count].iter().map(|key| vec![*key]).collect();
    }

    // Take the fewest trailing keys that can carry the overflow as prefixes.
    // At the limit every key becomes a prefix, and the recursion below splits
    // whatever still does not fit.
    let mut prefixes = 1;
    while prefixes < keys.len() && (keys.len() - prefixes) + prefixes * keys.len() < count {
        prefixes += 1;
    }
    let singles = keys.len() - prefixes;

    let mut result: Vec<Vec<u8>> = keys[..singles].iter().map(|key| vec![*key]).collect();
    let remaining = count - singles;
    for group in 0..prefixes {
        let share = remaining / prefixes + usize::from(group < remaining % prefixes);
        for suffix in labels(share, keys) {
            let mut label = Vec::with_capacity(1 + suffix.len());
            label.push(keys[singles + group]);
            label.extend_from_slice(&suffix);
            result.push(label);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label_text(targets: &[Target]) -> Vec<String> {
        targets
            .iter()
            .map(|target| String::from_utf8_lossy(&target.label).into_owned())
            .collect()
    }

    #[test]
    fn find_is_bidirectional_and_labels_the_nearest_match_first() {
        //         0123456789
        let data = b"xo..o.x.ox";
        let found = targets(data, (0, data.len()), 5, Kind::Find, b'o');
        // Matches at 1, 4 and 8; from the cursor at 5 they are 4 away, 1 away
        // and 3 away, so the closest one takes the first key.
        assert_eq!(
            found
                .iter()
                .map(|target| (target.match_start, target.destination))
                .collect::<Vec<_>>(),
            [(4, 4), (8, 8), (1, 1)]
        );
        assert_eq!(label_text(&found), ["a", "s", "d"]);
    }

    #[test]
    fn find_skips_the_byte_the_cursor_is_already_on() {
        let data = b"o.o";
        let found = targets(data, (0, data.len()), 0, Kind::Find, b'o');
        assert_eq!(found.iter().map(|t| t.match_start).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn till_is_forward_only_and_stops_one_byte_short() {
        //         0123456789
        let data = b"o.o...o..o";
        let found = targets(data, (0, data.len()), 3, Kind::Till, b'o');
        // The match at 0 and 2 are behind the cursor, so only 6 and 9 remain,
        // and each destination sits one byte before its match.
        assert_eq!(
            found
                .iter()
                .map(|target| (target.match_start, target.destination))
                .collect::<Vec<_>>(),
            [(6, 5), (9, 8)]
        );

        // A match immediately after the cursor would not move it at all.
        let adjacent = targets(b".o.", (0, 3), 0, Kind::Till, b'o');
        assert!(adjacent.is_empty());
    }

    #[test]
    fn targets_outside_the_visible_range_are_dropped() {
        let data = b"o.o.o";
        assert_eq!(targets(data, (2, 5), 0, Kind::Find, b'o').len(), 2);
        assert_eq!(targets(data, (0, 0), 0, Kind::Find, b'o').len(), 0);
        // A range past the end is clamped rather than panicking.
        assert_eq!(targets(data, (0, 99), 0, Kind::Find, b'o').len(), 2);
    }

    #[test]
    fn labels_stay_short_unique_and_prefix_free_at_every_size() {
        let keys = b"abc";
        assert_eq!(labels(2, keys), [b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(
            labels(3, keys),
            [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]
        );
        // Four targets need one key to become a prefix, so two keep single
        // labels and the rest hang off the third.
        assert_eq!(
            labels(4, keys),
            [b"a".to_vec(), b"b".to_vec(), b"ca".to_vec(), b"cb".to_vec()]
        );

        for count in [0, 1, 7, 26, 27, 28, 100, 800, 5000] {
            let built = labels(count, KEYS);
            assert_eq!(built.len(), count, "count {count}");
            let mut sorted = built.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), count, "duplicate label at {count}");
            // No label may be a prefix of another, or typing the shorter one
            // would make the longer unreachable.
            assert!(
                !built.iter().any(|label| built
                    .iter()
                    .any(|other| other != label && other.starts_with(label))),
                "prefix collision at {count}"
            );
        }
    }

    #[test]
    fn a_degenerate_alphabet_cannot_run_away() {
        assert!(labels(5, b"").is_empty());
        assert!(labels(0, KEYS).is_empty());
        // With one key every label would be a prefix of the next, so only one
        // target is addressable.  Recursing to build more would never
        // terminate.
        assert_eq!(labels(3, b"a"), [b"a".to_vec()]);
        assert_eq!(labels(1, b"a"), [b"a".to_vec()]);
    }
}
